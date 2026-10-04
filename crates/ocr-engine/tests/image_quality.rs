use std::io::Cursor;

use image::{DynamicImage, GrayImage, ImageFormat, Luma};
use ocr_engine::{
    inspect_image, inspect_image_with_thresholds, Error, ImageLimits, QualityDisposition,
    QualityThresholds, QualityVerdict, QualityWarning, RetakeReason,
};

const BLANK: &[u8] = include_bytes!("fixtures/blank.pgm");
const HIGH_CONTRAST: &[u8] = include_bytes!("fixtures/high-contrast.pgm");

#[test]
fn reports_blank_images_instead_of_silently_improving_them() {
    let report = inspect_image(BLANK, ImageLimits::new(100).unwrap()).unwrap();

    assert_eq!((report.width, report.height), (4, 4));
    assert!(report.warnings.contains(&QualityWarning::Blank));
    assert_eq!(report.quality_score, 0.0);
}

#[test]
fn measures_usable_high_contrast_images() {
    let report = inspect_image(HIGH_CONTRAST, ImageLimits::new(100).unwrap()).unwrap();

    assert!(report.warnings.is_empty());
    assert_eq!(report.contrast, 1.0);
    assert!(report.quality_score >= 0.9);
}

#[test]
fn rejects_invalid_content_and_pixel_bombs_before_processing() {
    assert!(matches!(
        inspect_image(b"not an image", ImageLimits::new(100).unwrap()),
        Err(Error::UnsupportedOrInvalidImage)
    ));
    assert!(matches!(
        inspect_image(HIGH_CONTRAST, ImageLimits::new(15).unwrap()),
        Err(Error::PixelLimitExceeded {
            pixels: 16,
            limit: 15
        })
    ));
    assert!(ImageLimits::new(0).is_err());
}

#[test]
fn low_resolution_is_reported_instead_of_silently_upscaled() {
    let encoded = png(32, 24, |x, _| if x % 2 == 0 { 0 } else { 255 });
    let thresholds = QualityThresholds::new(64, 64).unwrap();

    let report =
        inspect_image_with_thresholds(&encoded, ImageLimits::new(10_000).unwrap(), thresholds)
            .unwrap();

    assert!(report.warnings.contains(&QualityWarning::LowResolution));
    assert_eq!(report.disposition, QualityDisposition::RequestBetterSource);
    assert_eq!((report.width, report.height), (32, 24));
}

#[test]
fn blurred_sources_are_scored_and_routed_for_replacement() {
    let encoded = png(128, 128, |x, _| ((x * 255) / 127) as u8);
    let thresholds = QualityThresholds::new(64, 64)
        .unwrap()
        .with_minimum_sharpness(0.05)
        .unwrap();

    let report =
        inspect_image_with_thresholds(&encoded, ImageLimits::new(20_000).unwrap(), thresholds)
            .unwrap();

    assert!(report.warnings.contains(&QualityWarning::Blurry));
    assert!(report.sharpness < 0.05);
    assert_eq!(report.disposition, QualityDisposition::RequestBetterSource);
}

#[test]
fn severe_exposure_is_reported_without_background_rewriting() {
    let encoded = png(128, 128, |x, y| if x == 0 && y == 0 { 0 } else { 255 });

    let report = inspect_image_with_thresholds(
        &encoded,
        ImageLimits::new(20_000).unwrap(),
        QualityThresholds::new(64, 64).unwrap(),
    )
    .unwrap();

    assert!(report.warnings.contains(&QualityWarning::Overexposed));
    assert!(report.mean_luminance > 0.99);
    assert_eq!(report.disposition, QualityDisposition::RequestBetterSource);
}

#[test]
fn severely_underexposed_sources_are_routed_for_replacement() {
    let encoded = png(128, 128, |x, y| if x == 0 && y == 0 { 255 } else { 0 });

    let report = inspect_image_with_thresholds(
        &encoded,
        ImageLimits::new(20_000).unwrap(),
        QualityThresholds::new(64, 64).unwrap(),
    )
    .unwrap();

    assert!(report.warnings.contains(&QualityWarning::Underexposed));
    assert!(report.mean_luminance < 0.01);
    assert_eq!(report.disposition, QualityDisposition::RequestBetterSource);
}

#[test]
fn specular_glare_on_a_mid_tone_surface_asks_for_a_retake() {
    let encoded = png(128, 128, |x, y| {
        if (40..64).contains(&x) && (40..64).contains(&y) {
            255
        } else {
            90 + ((x + y) % 2) as u8 * 60
        }
    });

    let report = inspect(&encoded);

    assert!(report.warnings.contains(&QualityWarning::Glare));
    assert_eq!(report.disposition, QualityDisposition::RequestBetterSource);
    assert_eq!(
        report.verdict(),
        QualityVerdict::Retake(RetakeReason::AvoidGlare)
    );
}

#[test]
fn white_paper_backgrounds_are_not_mistaken_for_glare() {
    let encoded = png(
        128,
        128,
        |x, y| {
            if y % 8 == 0 && x % 3 != 0 {
                20
            } else {
                255
            }
        },
    );

    let report = inspect(&encoded);

    assert!(!report.warnings.contains(&QualityWarning::Glare));
    assert_eq!(report.verdict(), QualityVerdict::Usable);
}

#[test]
fn verdict_names_one_actionable_reason_and_blank_is_unusable() {
    let blank = inspect_image(BLANK, ImageLimits::new(100).unwrap()).unwrap();
    assert_eq!(blank.verdict(), QualityVerdict::Unusable);

    let small = inspect(&png(32, 24, |x, _| if x % 2 == 0 { 0 } else { 255 }));
    assert_eq!(
        small.verdict(),
        QualityVerdict::Retake(RetakeReason::MoveCloser)
    );

    let dark = inspect(&png(
        128,
        128,
        |x, y| if x == 0 && y == 0 { 255 } else { 0 },
    ));
    assert_eq!(
        dark.verdict(),
        QualityVerdict::Retake(RetakeReason::AddLight)
    );

    let bright = inspect(&png(
        128,
        128,
        |x, y| if x == 0 && y == 0 { 0 } else { 255 },
    ));
    assert_eq!(
        bright.verdict(),
        QualityVerdict::Retake(RetakeReason::ReduceLight)
    );

    let blurred = inspect_image_with_thresholds(
        &png(128, 128, |x, _| ((x * 255) / 127) as u8),
        ImageLimits::new(20_000).unwrap(),
        QualityThresholds::new(64, 64)
            .unwrap()
            .with_minimum_sharpness(0.05)
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        blurred.verdict(),
        QualityVerdict::Retake(RetakeReason::HoldSteady)
    );
}

fn inspect(encoded: &[u8]) -> ocr_engine::QualityReport {
    inspect_image_with_thresholds(
        encoded,
        ImageLimits::new(20_000).unwrap(),
        QualityThresholds::new(64, 64).unwrap(),
    )
    .unwrap()
}

fn png(width: u32, height: u32, pixel: impl Fn(u32, u32) -> u8) -> Vec<u8> {
    let image = GrayImage::from_fn(width, height, |x, y| Luma([pixel(x, y)]));
    let mut encoded = Cursor::new(Vec::new());
    DynamicImage::ImageLuma8(image)
        .write_to(&mut encoded, ImageFormat::Png)
        .unwrap();
    encoded.into_inner()
}
