use crate::{store::Session, Config, Error};
use hmac::{Hmac, KeyInit, Mac};
use reqwest::{Client, Method, Response, Url};
use serde_json::Value;
use sha2::Sha256;

pub struct Upstream {
    client: Client,
    #[cfg(test)]
    upload_fixture: Option<Url>,
}

pub async fn response_json(mut response: Response) -> Result<Value, Error> {
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| Error::Unavailable)? {
        if body.len() + chunk.len() > ocr_domain::MAXIMUM_DOCUMENT_RESULT_BYTES {
            return Err(Error::Unavailable);
        }
        body.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&body).map_err(|_| Error::Unavailable)
}

impl Upstream {
    pub fn new(config: &Config) -> Result<Self, Error> {
        Ok(Self {
            client: config.client()?,
            #[cfg(test)]
            upload_fixture: None,
        })
    }

    pub async fn call(
        &self,
        config: &Config,
        session: &Session,
        method: Method,
        path: &str,
        body: Option<&Value>,
        jobs: bool,
    ) -> Result<Value, Error> {
        let timestamp = time::OffsetDateTime::now_utc().unix_timestamp();
        let canonical = format!(
            "{}\n{}\n{timestamp}\n{}\n{path}",
            config.key_id,
            session.tenant_id,
            method.as_str()
        );
        let mut mac =
            Hmac::<Sha256>::new_from_slice(&config.secret).map_err(|_| Error::Configuration)?;
        mac.update(canonical.as_bytes());
        let signature: String = mac
            .finalize()
            .into_bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let base = if jobs {
            &config.job_api
        } else {
            &config.upload_api
        };
        let mut request = self
            .client
            .request(method, base.join(path).map_err(|_| Error::Configuration)?)
            .header("x-ocr-key-id", &config.key_id)
            .header("x-ocr-tenant-id", &session.tenant_id)
            .header("x-ocr-timestamp", timestamp.to_string())
            .header("x-ocr-signature", signature)
            .header(
                "idempotency-key",
                format!(
                    "sandbox-{}-{}",
                    session.tenant_id,
                    if jobs { "job" } else { "upload" }
                ),
            );
        if let Some(body) = body {
            request = request.json(body);
        }
        let response = request.send().await.map_err(|_| Error::Unavailable)?;
        if !response.status().is_success() {
            return Err(Error::Upstream(response.status()));
        }
        response_json(response).await
    }

    pub async fn put(
        &self,
        config: &Config,
        session: &Session,
        reservation: &Value,
        body: bytes::Bytes,
        content_type: &str,
    ) -> Result<(), Error> {
        let url = Url::parse(
            reservation
                .get("upload_url")
                .and_then(Value::as_str)
                .ok_or(Error::Unavailable)?,
        )
        .map_err(|_| Error::Unavailable)?;
        if url.scheme() != "https"
            || url.host_str() != Some("storage.googleapis.com")
            || url.port().is_some()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
            || url.path()
                != format!(
                    "/{}/products/ocr-sandbox/tenants/{}/quarantine/{}",
                    config.buckets[0],
                    session.tenant_id,
                    session.upload_id.as_deref().ok_or(Error::Conflict)?
                )
        {
            return Err(Error::Unavailable);
        }
        #[cfg(test)]
        let url = if let Some(base) = &self.upload_fixture {
            base.join(url.path()).map_err(|_| Error::Configuration)?
        } else {
            url
        };
        let response = self
            .client
            .put(url)
            .header("content-type", content_type)
            .body(body)
            .send()
            .await
            .map_err(|_| Error::Unavailable)?;
        if !response.status().is_success() {
            return Err(Error::Unavailable);
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn fixture(&mut self, base: Url) {
        self.upload_fixture = Some(base);
    }
}
