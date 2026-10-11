use crate::{api::App, store::Session, Error};
use reqwest::Method;
use serde_json::{json, Value};
use std::sync::Arc;

pub async fn images(app: &App, session: &Session) -> Result<(), Error> {
    app.storage.purge(&session.tenant_id, false).await?;
    app.store
        .cleanup_finished(&session.token_hash, true, false, false)
        .await
}

fn terminal(value: &Value) -> bool {
    matches!(
        value.get("status").and_then(Value::as_str),
        Some("completed" | "partial" | "review_required" | "failed" | "cancelled")
    )
}

async fn clean(app: &App, mut session: Session) -> Result<(bool, bool), Error> {
    let expired = time::OffsetDateTime::now_utc().unix_timestamp() > session.expires + 120;
    if let Some(upload) = &session.upload_id {
        if session.job_id.is_none() {
            let status = app
                .upstream
                .call(
                    &app.config,
                    &session,
                    Method::GET,
                    &format!("/v1/ocr/uploads/{upload}"),
                    None,
                    false,
                )
                .await?;
            match status.get("status").and_then(Value::as_str) {
                Some("uploaded" | "inspecting") => return Ok((false, false)),
                Some("accepted") => {
                    if !expired {
                        return Ok((false, false));
                    }
                    if let Some(payload) = &session.job_request {
                        let job = app
                            .upstream
                            .call(
                                &app.config,
                                &session,
                                Method::POST,
                                "/v1/ocr/jobs",
                                Some(payload),
                                true,
                            )
                            .await?;
                        let id = job
                            .get("job_id")
                            .and_then(Value::as_str)
                            .filter(|s| {
                                s.starts_with("job_")
                                    && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
                            })
                            .ok_or(Error::Unavailable)?;
                        app.store.record_id(&session.token_hash, id, true).await?;
                        session.job_id = Some(id.to_owned());
                    }
                }
                Some("expired" | "rejected") => {
                    app.storage.purge(&session.tenant_id, expired).await?;
                    return Ok((true, expired));
                }
                Some("reserved") => {}
                _ => return Err(Error::Unavailable),
            }
        }
    }
    if let Some(id) = &session.job_id {
        let status = app
            .upstream
            .call(
                &app.config,
                &session,
                Method::GET,
                &format!("/v1/ocr/jobs/{id}"),
                None,
                true,
            )
            .await?;
        if !terminal(&status) {
            if expired {
                app.upstream
                    .call(
                        &app.config,
                        &session,
                        Method::POST,
                        &format!("/v1/ocr/jobs/{id}/cancel"),
                        Some(&json!({})),
                        true,
                    )
                    .await?;
            }
            return Ok((false, false));
        }
    } else if !expired {
        return Ok((false, false));
    }
    if !session.images_deleted || expired {
        app.storage.purge(&session.tenant_id, expired).await?;
    }
    Ok((true, expired))
}

pub async fn pass(app: Arc<App>) -> Result<(), Error> {
    use futures_util::{stream, StreamExt};
    let sessions = app.store.cleanup_batch().await?;
    stream::iter(sessions)
        .map(|session| {
            let app = app.clone();
            async move {
                let key = session.token_hash.clone();
                let result =
                    tokio::time::timeout(std::time::Duration::from_secs(90), clean(&app, session))
                        .await;
                let (images, purged, failed) = match result {
                    Ok(Ok((images, purged))) => (images, purged, false),
                    _ => (false, false, true),
                };
                if failed {
                    tracing::warn!("sandbox cleanup pending; retry scheduled")
                }
                if app
                    .store
                    .cleanup_finished(&key, images, purged, failed)
                    .await
                    .is_err()
                {
                    tracing::warn!("sandbox cleanup checkpoint unavailable")
                }
            }
        })
        .buffer_unordered(4)
        .collect::<Vec<_>>()
        .await;
    app.store.prune().await
}
