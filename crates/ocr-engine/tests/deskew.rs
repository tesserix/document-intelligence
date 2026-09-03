use std::io::Cursor;

use image::{DynamicImage, GrayImage, ImageFormat, Luma};
use ocr_engine::{
    deskew_image, estimate_skew, prepare_image, DeskewDecision, DeskewPolicy, ImageLimits,
    PreparationOutcome, QualityThresholds, TransformStep,
};

#[test]
fn bounded_estimator_recovers_positive_and_negative_page_skew() {
    for skew_degrees in [-6.0_f64, 6.0] {
        let encoded = skewed_lines(512, 256, skew_degrees);
        let PreparationOutcome::Ready(page) = prepare_image(
            &encoded,
            ImageLimits::new(512 * 256).unwrap(),
            QualityThresholds::new(128, 128).unwrap(),
        )
        .unwrap() else {
            panic!("synthetic line page should be usable");
        };

        let estimate = estimate_skew(&page).unwrap();

        assert!((estimate.correction().degrees() + skew_degrees).abs() <= 0.5);
        assert!(estimate.confidence() >= 0.1);
        assert!(estimate.sampled_points() <= 50_000);
        assert_eq!(estimate.candidate_angles(), 97);
    }
}

#[test]
fn deskew_correction_is_explicit_and_reduces_residual_skew() {
    let encoded = skewed_lines(512, 256, 6.0);
    let PreparationOutcome::Ready(page) = prepare_image(
        &encoded,
        ImageLimits::new(512 * 256).unwrap(),
        QualityThresholds::new(128, 128).unwrap(),
    )
    .unwrap() else {
        panic!("synthetic line page should be usable");
    };
    let before = estimate_skew(&page).unwrap();

    let result = deskew_image(page, DeskewPolicy::new(0.1, 0.5).unwrap()).unwrap();

    assert_eq!(result.decision(), DeskewDecision::Applied);
    assert_eq!(result.image().dimensions(), (512, 256));
    assert!(matches!(
        result.image().transforms().steps().last(),
        Some(TransformStep::Deskew { .. })
    ));
    let after = estimate_skew(result.image()).unwrap();
    assert!(after.correction().degrees().abs() <= 0.5);
    assert!(after.confidence() < before.confidence());
}

#[test]
fn deskew_leaves_low_confidence_input_unchanged_with_a_reason() {
    let encoded = skewed_lines(512, 256, 4.0);
    let PreparationOutcome::Ready(page) = prepare_image(
        &encoded,
        ImageLimits::new(512 * 256).unwrap(),
        QualityThresholds::new(128, 128).unwrap(),
    )
    .unwrap() else {
        panic!("synthetic line page should be usable");
    };
    let pixels = page.pixels().to_vec();

    let result = deskew_image(page, DeskewPolicy::new(1.0, 0.5).unwrap()).unwrap();

    assert_eq!(result.decision(), DeskewDecision::BelowConfidence);
    assert_eq!(result.image().pixels(), pixels);
    assert!(result.image().transforms().is_empty());
    assert!(DeskewPolicy::new(f64::NAN, 0.5).is_err());
    assert!(DeskewPolicy::new(0.5, 12.01).is_err());
}

#[test]
fn deskew_rejects_an_out_of_profile_angle_instead_of_clipping_it() {
    let encoded = skewed_lines(512, 256, 13.0);
    let PreparationOutcome::Ready(page) = prepare_image(
        &encoded,
        ImageLimits::new(512 * 256).unwrap(),
        QualityThresholds::new(128, 128).unwrap(),
    )
    .unwrap() else {
        panic!("synthetic line page should be usable");
    };
    let pixels = page.pixels().to_vec();

    let result = deskew_image(page, DeskewPolicy::new(0.1, 0.5).unwrap()).unwrap();

    assert_eq!(result.decision(), DeskewDecision::BelowConfidence);
    assert_eq!(result.estimate().confidence(), 0.0);
    assert_eq!(result.image().pixels(), pixels);
    assert!(result.image().transforms().is_empty());
}

fn skewed_lines(width: u32, height: u32, degrees: f64) -> Vec<u8> {
    let slope = degrees.to_radians().tan();
    let center_x = f64::from(width) / 2.0;
    let baselines = [48.0, 88.0, 128.0, 168.0, 208.0];
    let image = GrayImage::from_fn(width, height, |x, y| {
        let ink = baselines.iter().any(|baseline| {
            let expected_y = baseline + (f64::from(x) - center_x) * slope;
            (f64::from(y) - expected_y).abs() <= 1.5
        });
        Luma([if ink { 0 } else { 255 }])
    });
    let mut encoded = Cursor::new(Vec::new());
    DynamicImage::ImageLuma8(image)
        .write_to(&mut encoded, ImageFormat::Png)
        .unwrap();
    encoded.into_inner()
}
