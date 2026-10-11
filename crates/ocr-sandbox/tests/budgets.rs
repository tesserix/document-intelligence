use ocr_sandbox::{Error, Store};
use serde_json::json;

#[tokio::test]
#[ignore = "requires TEST_DATABASE_ADMIN_URL"]
async fn upload_budget_is_shared_and_duplicate_reservations_are_free() {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(4)
        .connect(&std::env::var("TEST_DATABASE_ADMIN_URL").unwrap())
        .await
        .unwrap();
    let schema = format!("sandbox_test_{}", uuid::Uuid::new_v4().simple());
    sqlx::query(sqlx::AssertSqlSafe(format!("create schema {schema}")))
        .execute(&pool)
        .await
        .unwrap();
    let options = std::env::var("TEST_DATABASE_ADMIN_URL")
        .unwrap()
        .parse::<sqlx::postgres::PgConnectOptions>()
        .unwrap()
        .options([("search_path", schema.as_str())]);
    let test_pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(4)
        .connect_with(options)
        .await
        .unwrap();
    sqlx::raw_sql(include_str!("../migrations/0001_sessions.sql"))
        .execute(&test_pool)
        .await
        .unwrap();
    let store = Store::new(test_pool.clone(), 1);
    let (_, one) = store.create_session("network-one").await.unwrap();
    let (_, two) = store.create_session("network-two").await.unwrap();
    let payload = json!({"content_type":"image/png","content_length":123,"sha256":format!("sha256:{}", "a".repeat(64))});
    let (a, b) = tokio::join!(
        store.reserve_upload(&one.token_hash, &payload),
        store.reserve_upload(&two.token_hash, &payload)
    );
    assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
    let winner = if a.is_ok() { &one } else { &two };
    assert!(matches!(
        if a.is_err() { a } else { b },
        Err(Error::Limited)
    ));
    assert!(store
        .reserve_upload(&winner.token_hash, &payload)
        .await
        .is_ok());
    test_pool.close().await;
    sqlx::query(sqlx::AssertSqlSafe(format!("drop schema {schema} cascade")))
        .execute(&pool)
        .await
        .unwrap();
}
