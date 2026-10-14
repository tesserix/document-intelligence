#![cfg(target_os = "linux")]

use ocr_service::ParserProcess;
use std::{fs, process::Command, time::Duration};

#[tokio::test]
async fn parser_cannot_create_network_sockets() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("socket_probe.c");
    let executable = directory.path().join("socket_probe");
    fs::write(&source,r#"
#include <errno.h>
#include <stdio.h>
#include <sys/socket.h>
int main(void) {
    while (getchar() != EOF) {}
    if (socket(AF_INET, SOCK_STREAM, 0) != -1 || errno != EPERM) return 99;
    puts("{\"page_count\":1,\"maximum_page_pixels\":1,\"total_page_pixels\":1,\"pages\":[{\"page\":1,\"width\":1,\"height\":1}],\"password_protected\":false}");
    return 0;
}
"#).unwrap();
    assert!(Command::new("cc")
        .arg(&source)
        .arg("-o")
        .arg(&executable)
        .status()
        .unwrap()
        .success());
    let parser = ParserProcess::new(executable, Duration::from_secs(5)).unwrap();
    let report = parser.inspect(b"fixture", "image/png").await.unwrap();
    assert_eq!(report.page_count, 1);
}
