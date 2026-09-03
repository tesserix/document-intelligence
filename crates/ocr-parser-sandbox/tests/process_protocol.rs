use std::{
    io::{Cursor, Write},
    process::{Command, Stdio},
};

use image::{DynamicImage, GrayImage, ImageFormat, Luma};
use ocr_parser_sandbox::InspectionReport;

fn png() -> Vec<u8> {
    let image = GrayImage::from_pixel(2, 3, Luma([255]));
    let mut bytes = Cursor::new(Vec::new());
    DynamicImage::ImageLuma8(image)
        .write_to(&mut bytes, ImageFormat::Png)
        .unwrap();
    bytes.into_inner()
}

fn run(input: &[u8], content_type: &str) -> std::process::Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_ocr-parser-sandbox"))
        .args([
            "--content-type",
            content_type,
            "--max-pages",
            "300",
            "--max-page-pixels",
            "100000000",
            "--max-total-pixels",
            "1000000000",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn emits_only_a_bounded_json_metadata_report() {
    let output = run(&png(), "image/png");

    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    assert!(output.stdout.len() < 1024);
    let report: InspectionReport = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report.page_count, 1);
    assert_eq!(report.total_page_pixels, 6);
}

#[test]
fn returns_a_stable_nonzero_status_without_echoing_invalid_input() {
    let output = run(b"private invalid document bytes", "application/pdf");

    assert_eq!(output.status.code(), Some(10));
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
}
