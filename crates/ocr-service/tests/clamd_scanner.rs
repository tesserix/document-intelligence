use std::{io, time::Duration};

use bytes::Bytes;
use futures_util::stream;
use ocr_service::{ClamdScanner, MalwareScanError, MalwareScanOutcome};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

#[tokio::test]
async fn scanner_streams_the_bounded_protocol_and_accepts_only_a_clean_reply() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut command = [0_u8; 10];
        socket.read_exact(&mut command).await.unwrap();
        assert_eq!(&command, b"zINSTREAM\0");

        let mut length = [0_u8; 4];
        socket.read_exact(&mut length).await.unwrap();
        assert_eq!(u32::from_be_bytes(length), 8);
        let mut payload = [0_u8; 8];
        socket.read_exact(&mut payload).await.unwrap();
        assert_eq!(&payload, b"%PDF-1.7");
        socket.read_exact(&mut length).await.unwrap();
        assert_eq!(u32::from_be_bytes(length), 0);
        socket.write_all(b"stream: OK\0").await.unwrap();
    });
    let scanner = ClamdScanner::new(address, Duration::from_secs(1), 32).unwrap();

    let outcome = scanner
        .scan(stream::iter([Ok::<_, io::Error>(Bytes::from_static(
            b"%PDF-1.7",
        ))]))
        .await
        .unwrap();

    assert_eq!(outcome, MalwareScanOutcome::Clean);
    server.await.unwrap();
}

#[tokio::test]
async fn scanner_fails_closed_for_infection_limits_and_non_loopback_endpoints() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        socket.read_to_end(&mut request).await.unwrap();
        socket.write_all(b"stream: fixture FOUND\0").await.unwrap();
    });
    let scanner = ClamdScanner::new(address, Duration::from_secs(1), 32).unwrap();
    let outcome = scanner
        .scan(stream::iter([Ok::<_, io::Error>(Bytes::from_static(
            b"bad",
        ))]))
        .await
        .unwrap();
    assert_eq!(outcome, MalwareScanOutcome::Infected);
    server.await.unwrap();

    let limit_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let limit_address = limit_listener.local_addr().unwrap();
    let limit_server = tokio::spawn(async move {
        let (mut socket, _) = limit_listener.accept().await.unwrap();
        let mut request = Vec::new();
        socket.read_to_end(&mut request).await.unwrap();
    });
    let limit_scanner = ClamdScanner::new(limit_address, Duration::from_secs(1), 32).unwrap();
    let oversized = limit_scanner
        .scan(stream::iter([Ok::<_, io::Error>(Bytes::from(vec![0; 33]))]))
        .await
        .unwrap_err();
    assert_eq!(oversized, MalwareScanError::LimitExceeded);
    limit_server.await.unwrap();
    assert!(ClamdScanner::new(
        "192.0.2.1:3310".parse().unwrap(),
        Duration::from_secs(1),
        32,
    )
    .is_err());
}
