use std::{
    fs,
    os::unix::fs::PermissionsExt,
    process::{Command as StdCommand, Stdio},
    time::Duration,
};

use ocr_service::{ParserProcess, ParserProcessError};

#[tokio::test]
async fn parser_timeout_terminates_the_parser_process_group() {
    let directory = tempfile::tempdir().unwrap();
    let child_pid_path = directory.path().join("child-pid");
    let parser_path = directory.path().join("group-parser");
    fs::write(
        &parser_path,
        format!(
            "#!/bin/sh\n(sleep 3) & child=$!; printf '%s' \"$child\" > '{}'; cat >/dev/null; wait \"$child\"\n",
            child_pid_path.display()
        ),
    )
    .unwrap();
    let mut permissions = fs::metadata(&parser_path).unwrap().permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&parser_path, permissions).unwrap();
    let parser = ParserProcess::new(parser_path, Duration::from_secs(1)).unwrap();
    let inspection = tokio::spawn(async move { parser.inspect(b"fixture", "image/png").await });
    let child_pid = tokio::time::timeout(Duration::from_millis(500), async {
        loop {
            match fs::read_to_string(&child_pid_path) {
                Ok(child_pid) => break child_pid,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    tokio::task::yield_now().await;
                }
                Err(error) => panic!("could not read parser child PID: {error}"),
            }
        }
    })
    .await
    .expect("parser child did not start before its deadline");

    assert_eq!(
        inspection.await.unwrap().unwrap_err(),
        ParserProcessError::Unavailable
    );
    let is_running = StdCommand::new("kill")
        .args(["-0", child_pid.trim()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap()
        .success();
    let _ = StdCommand::new("kill")
        .args(["-KILL", child_pid.trim()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();

    assert!(!is_running, "parser subprocess outlived its deadline");
}
