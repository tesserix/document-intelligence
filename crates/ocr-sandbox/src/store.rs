use crate::Error;
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::{postgres::PgRow, PgPool, Row};
use uuid::Uuid;

const COLUMNS: &str = "token_hash,tenant_id,upload_request,upload_id,job_request,job_id,content_uploaded,images_deleted_at is not null as images_deleted,extract(epoch from expires_at)::bigint as expires";

#[derive(Clone)]
pub struct Store {
    pub(crate) pool: PgPool,
    daily_uploads: i64,
}

pub struct Session {
    pub token_hash: String,
    pub tenant_id: String,
    pub upload_request: Option<Value>,
    pub upload_id: Option<String>,
    pub job_request: Option<Value>,
    pub job_id: Option<String>,
    pub content_uploaded: bool,
    pub images_deleted: bool,
    pub expires: i64,
}

fn session(row: PgRow) -> Result<Session, Error> {
    let json = |name| -> Result<Option<Value>, Error> {
        let text: Option<String> = row.try_get(name)?;
        text.map(|s| serde_json::from_str(&s).map_err(|_| Error::Unavailable))
            .transpose()
    };
    Ok(Session {
        token_hash: row.try_get("token_hash")?,
        tenant_id: row.try_get("tenant_id")?,
        upload_request: json("upload_request")?,
        upload_id: row.try_get("upload_id")?,
        job_request: json("job_request")?,
        job_id: row.try_get("job_id")?,
        content_uploaded: row.try_get("content_uploaded")?,
        images_deleted: row.try_get("images_deleted")?,
        expires: row.try_get("expires")?,
    })
}

pub fn hash(value: &[u8]) -> String {
    Sha256::digest(value)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

impl Store {
    pub fn new(pool: PgPool, daily_uploads: i64) -> Self {
        Self {
            pool,
            daily_uploads,
        }
    }

    pub async fn create_session(&self, network: &str) -> Result<(String, Session), Error> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("select pg_advisory_xact_lock(683928741)")
            .execute(&mut *tx)
            .await?;
        let counts=sqlx::query("select count(*) filter(where network_hash=$1 and created_at>now()-interval '1 hour') as network, count(*) filter(where created_at>date_trunc('day',now() at time zone 'UTC') at time zone 'UTC') as global from ocr_sandbox_sessions")
            .bind(network).fetch_one(&mut *tx).await?;
        if counts.try_get::<i64, _>("network")? >= 10 || counts.try_get::<i64, _>("global")? >= 1000
        {
            return Err(Error::Limited);
        }
        let token = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
        let token_hash = hash(token.as_bytes());
        let tenant = format!("ten_SBX_{}", Uuid::new_v4().simple());
        let row=sqlx::query(sqlx::AssertSqlSafe(format!("insert into ocr_sandbox_sessions(token_hash,tenant_id,network_hash) values($1,$2,$3) returning {COLUMNS}")))
            .bind(&token_hash).bind(tenant).bind(network).fetch_one(&mut *tx).await?;
        tx.commit().await?;
        Ok((token, session(row)?))
    }

    pub async fn authorize(&self, token: &str) -> Result<Session, Error> {
        if token.len() != 64 || !token.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(Error::Unauthorized);
        }
        let key = hash(token.as_bytes());
        let row=sqlx::query(sqlx::AssertSqlSafe(format!("update ocr_sandbox_sessions set request_window=extract(epoch from now())::bigint/60,request_count=case when request_window=extract(epoch from now())::bigint/60 then request_count+1 else 1 end where token_hash=$1 and expires_at>now() and (request_window<>extract(epoch from now())::bigint/60 or request_count<60) returning {COLUMNS}")))
            .bind(&key).fetch_optional(&self.pool).await?;
        if let Some(row) = row {
            return session(row);
        }
        let exists=sqlx::query_scalar::<_,bool>("select exists(select 1 from ocr_sandbox_sessions where token_hash=$1 and expires_at>now())")
            .bind(key).fetch_one(&self.pool).await?;
        Err(if exists {
            Error::Limited
        } else {
            Error::Unauthorized
        })
    }

    pub async fn reserve_upload(&self, key: &str, payload: &Value) -> Result<Session, Error> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("select pg_advisory_xact_lock(683928742)")
            .execute(&mut *tx)
            .await?;
        let row=sqlx::query(sqlx::AssertSqlSafe(format!("select {COLUMNS} from ocr_sandbox_sessions where token_hash=$1 and expires_at>now() for update")))
            .bind(key).fetch_optional(&mut *tx).await?.ok_or(Error::Unauthorized)?;
        let existing = session(row)?;
        if let Some(body) = &existing.upload_request {
            if body != payload {
                return Err(Error::Conflict);
            }
            return Ok(existing);
        }
        let count=sqlx::query_scalar::<_,i64>("select count(*) from ocr_sandbox_sessions where admitted_at>=date_trunc('day',now() at time zone 'UTC') at time zone 'UTC'")
            .fetch_one(&mut *tx).await?;
        if count >= self.daily_uploads {
            return Err(Error::Limited);
        }
        let row=sqlx::query(sqlx::AssertSqlSafe(format!("update ocr_sandbox_sessions set upload_request=$2,admitted_at=now() where token_hash=$1 returning {COLUMNS}")))
            .bind(key).bind(payload.to_string()).fetch_one(&mut *tx).await?;
        tx.commit().await?;
        session(row)
    }

    pub async fn reserve_job(&self, key: &str, payload: &Value) -> Result<(), Error> {
        let updated=sqlx::query("update ocr_sandbox_sessions set job_request=$2 where token_hash=$1 and expires_at>now() and (job_request is null or job_request=$2)")
            .bind(key).bind(payload.to_string()).execute(&self.pool).await?;
        if updated.rows_affected() != 1 {
            return Err(Error::Conflict);
        }
        Ok(())
    }

    pub async fn record_id(&self, key: &str, id: &str, job: bool) -> Result<(), Error> {
        let sql = if job {
            "update ocr_sandbox_sessions set job_id=$2 where token_hash=$1 and (job_id is null or job_id=$2)"
        } else {
            "update ocr_sandbox_sessions set upload_id=$2 where token_hash=$1 and (upload_id is null or upload_id=$2)"
        };
        if sqlx::query(sql)
            .bind(key)
            .bind(id)
            .execute(&self.pool)
            .await?
            .rows_affected()
            != 1
        {
            return Err(Error::Conflict);
        }
        Ok(())
    }

    pub async fn claim_content(&self, key: &str) -> Result<(), Error> {
        let count=sqlx::query("update ocr_sandbox_sessions set content_started_at=now() where token_hash=$1 and expires_at>now() and content_started_at is null and not content_uploaded")
            .bind(key).execute(&self.pool).await?.rows_affected();
        if count != 1 {
            return Err(Error::Conflict);
        }
        Ok(())
    }

    pub async fn content_finished(&self, key: &str, success: bool) -> Result<(), Error> {
        sqlx::query("update ocr_sandbox_sessions set content_uploaded=$2,content_started_at=null where token_hash=$1")
            .bind(key).bind(success).execute(&self.pool).await?;
        Ok(())
    }

    pub async fn cleanup_batch(&self) -> Result<Vec<Session>, Error> {
        let rows=sqlx::query(sqlx::AssertSqlSafe(format!("update ocr_sandbox_sessions set cleanup_lease_until=now()+interval '2 minutes' where token_hash in (select token_hash from ocr_sandbox_sessions where purged_at is null and upload_request is not null and cleanup_after<=now() and (cleanup_lease_until is null or cleanup_lease_until<now()) order by cleanup_after for update skip locked limit 10) returning {COLUMNS}")))
            .fetch_all(&self.pool).await?;
        rows.into_iter().map(session).collect()
    }

    pub async fn cleanup_finished(
        &self,
        key: &str,
        images: bool,
        purged: bool,
        failed: bool,
    ) -> Result<(), Error> {
        sqlx::query("update ocr_sandbox_sessions set images_deleted_at=case when $2 then coalesce(images_deleted_at,now()) else images_deleted_at end,purged_at=case when $3 then now() else purged_at end,cleanup_failures=case when $4 then cleanup_failures+1 else 0 end,cleanup_lease_until=null,cleanup_after=case when $4 then now()+interval '30 seconds' when $2 then greatest(now()+interval '2 seconds',expires_at+interval '2 minutes') else now()+interval '2 seconds' end where token_hash=$1")
            .bind(key).bind(images).bind(purged).bind(failed).execute(&self.pool).await?;
        Ok(())
    }

    pub async fn prune(&self) -> Result<(), Error> {
        sqlx::query("delete from ocr_sandbox_sessions where created_at<now()-interval '7 days' and (purged_at is not null or upload_request is null)").execute(&self.pool).await?;
        Ok(())
    }
}
