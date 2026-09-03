use std::{io, net::SocketAddr, time::Duration};

use ocr_temporal::{qualification_deployment_options, QualificationPageActivities};
use temporalio_client::{Client, ClientOptions, Connection, ConnectionOptions};
use temporalio_sdk::{Runtime, Worker, WorkerOptions};
use url::Url;

fn invalid_input(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args();
    let _program = args.next();
    let target = args
        .next()
        .ok_or_else(|| invalid_input("missing Temporal target"))?;
    let task_queue = args
        .next()
        .ok_or_else(|| invalid_input("missing task queue"))?;
    let mode = args
        .next()
        .ok_or_else(|| invalid_input("missing qualification mode"))?;
    let started_endpoint = args.next();
    if args.next().is_some() {
        return Err(invalid_input("unexpected argument").into());
    }

    let target = Url::parse(&target)?;
    if target.scheme() != "http"
        || target.host_str() != Some("127.0.0.1")
        || target.port().is_none()
        || target.path() != "/"
        || target.query().is_some()
        || target.fragment().is_some()
        || !target.username().is_empty()
        || target.password().is_some()
    {
        return Err(invalid_input("Temporal target must be loopback HTTP").into());
    }
    if task_queue.is_empty()
        || task_queue.len() > 127
        || !task_queue
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(invalid_input("invalid task queue").into());
    }
    let activities = match mode.as_str() {
        "hold" => {
            let started_endpoint = started_endpoint
                .ok_or_else(|| invalid_input("missing observation endpoint"))?
                .parse::<SocketAddr>()?;
            if !started_endpoint.ip().is_loopback() {
                return Err(invalid_input("observation endpoint must be loopback").into());
            }
            QualificationPageActivities::held_for_process_loss(started_endpoint)
        }
        "normal" if started_endpoint.is_none() => QualificationPageActivities::default(),
        _ => return Err(invalid_input("invalid qualification mode").into()),
    };

    let connection = Connection::connect(
        ConnectionOptions::new(target)
            .identity("ocr-temporal-qualification-worker")
            .connect_timeout(Duration::from_secs(5))
            .build(),
    )
    .await?;
    let client = Client::new(connection, ClientOptions::new("default").build())?;
    let runtime = Runtime::new_assume_tokio(Default::default())?;
    let worker_options = WorkerOptions::new(task_queue)
        .deployment_options(qualification_deployment_options())
        .register_activities(activities)
        .build();
    let mut worker = Worker::new(&runtime, client, worker_options)?;
    worker.run().await?;
    Ok(())
}
