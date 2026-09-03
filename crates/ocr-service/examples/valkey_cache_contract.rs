use fred::prelude::{Builder, ClientLike, Config, Expiration, KeysInterface};
use ocr_domain::{JobId, JobState, ProductId, TenantId};
use ocr_service::{CachePolicy, CacheRecord, CacheScope, JobStatusCache, ValkeyJobStatusCache};
use std::time::Duration;
use time::OffsetDateTime;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let url = std::env::var("TEST_VALKEY_URL")?;
    let policy = CachePolicy::new(
        Duration::from_secs(30),
        Duration::from_secs(300),
        Duration::from_millis(100),
        512,
    )?;
    let cache = ValkeyJobStatusCache::connect(&url, policy.clone()).await?;
    let scope = CacheScope::new(
        ProductId::new("integration-product")?,
        TenantId::new("ten_VALKEY")?,
        JobId::new("job_VALKEY")?,
    );
    let created_at = OffsetDateTime::from_unix_timestamp(1_725_000_000)?;
    let record = CacheRecord::new(&scope, JobState::Processing, created_at);

    cache
        .put(&scope, &record, policy.ttl(record.status()))
        .await
        .map_err(|_| anyhow::anyhow!("cache write failed"))?;
    let cached = cache
        .get(&scope)
        .await
        .map_err(|_| anyhow::anyhow!("cache read failed"))?
        .ok_or_else(|| anyhow::anyhow!("cache record is missing"))?;
    anyhow::ensure!(cached.matches_scope(&scope));
    anyhow::ensure!(cached.status() == JobState::Processing);
    anyhow::ensure!(cached.created_at() == created_at);

    let verifier = Builder::from_config(Config::from_url(&url)?)
        .build()
        .map_err(|_| anyhow::anyhow!("verification client configuration failed"))?;
    verifier.init().await?;
    let ttl: i64 = verifier.ttl(scope.key()).await?;
    anyhow::ensure!((1..=30).contains(&ttl));

    let _: () = verifier
        .set(
            scope.key(),
            vec![b'x'; 513],
            Some(Expiration::EX(30)),
            None,
            false,
        )
        .await?;
    anyhow::ensure!(cache.get(&scope).await.is_err());
    Ok(())
}
