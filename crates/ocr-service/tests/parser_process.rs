use std::{fs, os::unix::fs::PermissionsExt, time::Duration};

use ocr_service::{ParserProcess, ParserProcessError};

fn fixture(script: &str) -> (tempfile::TempDir, ParserProcess) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("parser-fixture");
    fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
    let mut permissions = fs::metadata(&path).unwrap().permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&path, permissions).unwrap();
    let process = ParserProcess::new(path, Duration::from_secs(1)).unwrap();
    (directory, process)
}

#[tokio::test]
async fn parser_process_returns_only_valid_bounded_metadata() {
    let (_directory, parser) = fixture(
        "cat >/dev/null; printf '{\"page_count\":2,\"maximum_page_pixels\":8500000,\"total_page_pixels\":16000000,\"password_protected\":false}'",
    );

    let report = parser
        .inspect(b"%PDF-fixture", "application/pdf")
        .await
        .unwrap();

    assert_eq!(report.page_count, 2);
    assert_eq!(report.maximum_page_pixels, 8_500_000);
    assert_eq!(report.total_page_pixels, 16_000_000);
}

#[tokio::test]
async fn parser_process_maps_stable_failures_and_bounds_output_and_time() {
    for (status, expected) in [
        (10, ParserProcessError::InvalidDocument),
        (11, ParserProcessError::LimitsExceeded),
        (12, ParserProcessError::PasswordRequired),
        (13, ParserProcessError::Unavailable),
    ] {
        let (_directory, parser) = fixture(&format!("cat >/dev/null; exit {status}"));
        assert_eq!(
            parser.inspect(b"fixture", "image/png").await.unwrap_err(),
            expected
        );
    }

    let (_directory, noisy) = fixture("cat >/dev/null; yes x | head -c 5000");
    assert_eq!(
        noisy.inspect(b"fixture", "image/png").await.unwrap_err(),
        ParserProcessError::Unavailable
    );

    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("slow-parser");
    fs::write(&path, "#!/bin/sh\ncat >/dev/null; sleep 5\n").unwrap();
    let mut permissions = fs::metadata(&path).unwrap().permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&path, permissions).unwrap();
    let parser = ParserProcess::new(path, Duration::from_millis(20)).unwrap();
    assert_eq!(
        parser.inspect(b"fixture", "image/png").await.unwrap_err(),
        ParserProcessError::Unavailable
    );
}
