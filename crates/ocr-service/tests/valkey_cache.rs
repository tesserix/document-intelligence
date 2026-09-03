use ocr_domain::{JobId, JobState, ProductId, TenantId};
use ocr_service::{CachePolicy, CacheRecord, CacheScope, JobStatusCache, ValkeyJobStatusCache};
use std::time::Duration;
use time::OffsetDateTime;

#[tokio::test]
#[ignore = "requires TEST_VALKEY_URL"]
async fn valkey_round_trip_is_scoped_and_expiring() {
    let url = std::env::var("TEST_VALKEY_URL").expect("TEST_VALKEY_URL must be set");
    let policy = CachePolicy::new(
        Duration::from_secs(30),
        Duration::from_secs(300),
        Duration::from_millis(100),
        512,
    )
    .unwrap();
    let cache = ValkeyJobStatusCache::connect(&url, policy.clone())
        .await
        .unwrap();
    let scope = CacheScope::new(
        ProductId::new("integration-product").unwrap(),
        TenantId::new("ten_VALKEY").unwrap(),
        JobId::new("job_VALKEY").unwrap(),
    );
    let created_at = OffsetDateTime::from_unix_timestamp(1_725_000_000).unwrap();
    let record = CacheRecord::new(&scope, JobState::Processing, created_at);

    cache
        .put(&scope, &record, policy.ttl(record.status()))
        .await
        .unwrap();
    let cached = cache.get(&scope).await.unwrap().unwrap();

    assert!(cached.matches_scope(&scope));
    assert_eq!(cached.status(), JobState::Processing);
    assert_eq!(cached.created_at(), created_at);

    let verifier = Builder::from_config(Config::from_url(&url).unwrap())
        .build()
        .unwrap();
    verifier.init().await.unwrap();
    let ttl: i64 = verifier.ttl(scope.key()).await.unwrap();
    assert!((1..=30).contains(&ttl));

    let _: () = verifier
        .set(
            scope.key(),
            vec![b'x'; 513],
            Some(Expiration::EX(30)),
            None,
            false,
        )
        .await
        .unwrap();
    assert!(cache.get(&scope).await.is_err());
}
use fred::prelude::{Builder, ClientLike, Config, Expiration, KeysInterface};
