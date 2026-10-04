use image::{DynamicImage, GrayImage, ImageFormat, Luma};
use ocr_parser_sandbox::prepare_ocr_image;
use std::io::Cursor;

fn png(width: u32, height: u32) -> Vec<u8> {
    let mut output = Cursor::new(Vec::new());
    DynamicImage::ImageLuma8(GrayImage::from_fn(width, height, |x, y| {
        Luma([if (x + y) % 7 == 0 { 0 } else { 255 }])
    }))
    .write_to(&mut output, ImageFormat::Png)
    .unwrap();
    output.into_inner()
}

#[test]
fn enlarges_only_small_images_without_changing_aspect_ratio() {
    let output = prepare_ocr_image(&png(350, 270), "image/png")
        .unwrap()
        .unwrap();
    let image = image::load_from_memory(&output).unwrap();
    assert_eq!((image.width(), image.height()), (1050, 810));
    assert_eq!(
        prepare_ocr_image(&png(1400, 1080), "image/png").unwrap(),
        None
    );
    assert_eq!(
        prepare_ocr_image(b"%PDF-1.7", "application/pdf").unwrap(),
        None
    );
}

#[test]
fn rejects_malformed_small_images_and_does_not_expand_tiny_or_large_inputs() {
    assert!(prepare_ocr_image(b"not an image", "image/png").is_err());
    assert!(prepare_ocr_image(&vec![0; 20 * 1024 * 1024 + 1], "image/png").is_err());
    assert_eq!(prepare_ocr_image(&png(32, 24), "image/png").unwrap(), None);
    assert_eq!(prepare_ocr_image(&png(641, 64), "image/png").unwrap(), None);
}

#[test]
fn cli_emits_the_same_png_as_the_library() {
    use std::{
        io::Write,
        process::{Command, Stdio},
    };
    let bytes = png(64, 64);
    let mut child = Command::new(env!("CARGO_BIN_EXE_ocr-parser-sandbox"))
        .args([
            "prepare-ocr",
            "--content-type",
            "image/png",
            "--max-pages",
            "1",
            "--max-page-pixels",
            "1000000",
            "--max-total-pixels",
            "1000000",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(&bytes).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    assert_eq!(
        output.stdout,
        prepare_ocr_image(&bytes, "image/png").unwrap().unwrap()
    );
}

#[test]
fn animated_images_keep_the_original_provider_path() {
    assert_eq!(
        prepare_ocr_image(include_bytes!("fixtures/animated.png"), "image/png").unwrap(),
        None
    );
}
