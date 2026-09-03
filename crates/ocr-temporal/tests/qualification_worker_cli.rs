use std::process::{Command, Stdio};

#[test]
fn qualification_worker_rejects_external_targets_and_invalid_modes() {
    for args in [
        ["http://169.254.169.254:7233", "ocr-qualification", "normal"],
        ["http://127.0.0.1:7233", "invalid queue", "normal"],
        ["http://127.0.0.1:7233", "ocr-qualification", "unknown"],
    ] {
        let status = Command::new(env!("CARGO_BIN_EXE_temporal-qualification-worker"))
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap();
        assert!(!status.success(), "unexpectedly accepted {args:?}");
    }
}
