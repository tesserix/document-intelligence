use ocr_sandbox::{App, Config};
use sqlx::postgres::PgPoolOptions;
use std::{net::SocketAddr, sync::Arc, time::Duration};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .json()
        .with_env_filter("ocr_sandbox=info")
        .init();
    let config = Config::from_env()?;
    let pool = PgPoolOptions::new()
        .max_connections(12)
        .acquire_timeout(Duration::from_secs(5))
        .connect_lazy(&config.database_url)?;
    let app = App::new(config, pool)?;
    let (stop, mut stopped) = tokio::sync::watch::channel(false);
    let worker_app = Arc::clone(&app);
    let worker = tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(2));
        loop {
            tokio::select! {
                _=stopped.changed()=>break,
                _=interval.tick()=>if ocr_sandbox::cleanup::pass(worker_app.clone()).await.is_err(){tracing::warn!("sandbox cleanup store unavailable")},
            }
        }
    });
    let address = std::env::var("SANDBOX_BIND_ADDRESS")
        .unwrap_or_else(|_| "0.0.0.0:8080".into())
        .parse::<SocketAddr>()?;
    let listener = tokio::net::TcpListener::bind(address).await?;
    axum::serve(
        listener,
        ocr_sandbox::router(app).into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("SIGTERM handler");
        tokio::select! {_=tokio::signal::ctrl_c()=>{},_=terminate.recv()=>{}}
    })
    .await?;
    let _ = stop.send(true);
    worker.await?;
    Ok(())
}
