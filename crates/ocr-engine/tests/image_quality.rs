use ocr_engine::{inspect_image, Error, ImageLimits, QualityWarning};

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
