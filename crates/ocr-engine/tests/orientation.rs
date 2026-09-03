use std::io::Cursor;

use image::{DynamicImage, GrayImage, ImageFormat, Luma};
use ocr_engine::{
    prepare_image, ImageLimits, PreparationOutcome, QualityThresholds, Rotation, TransformStep,
};

#[test]
fn exif_orientation_is_applied_and_recorded_for_evidence_mapping() {
    let encoded = jpeg_with_orientation(96, 64, 6);

    let outcome = prepare_image(
        &encoded,
        ImageLimits::new(20_000).unwrap(),
        QualityThresholds::new(32, 32).unwrap(),
    )
    .unwrap();

    let PreparationOutcome::Ready(page) = outcome else {
        panic!("high-contrast source should be usable");
    };
    assert_eq!(page.dimensions(), (64, 96));
    assert_eq!(
        page.transforms().steps(),
        &[TransformStep::Rotation {
            rotation: Rotation::Clockwise90
        }]
    );
    assert_eq!(
        page.transforms().map_to_original(0.0, 0.0).unwrap(),
        (0.0, 1.0)
    );
}

#[test]
fn unusable_images_are_not_transformed() {
    let mut encoded = Cursor::new(Vec::new());
    DynamicImage::ImageLuma8(GrayImage::from_pixel(64, 64, Luma([127])))
        .write_to(&mut encoded, ImageFormat::Png)
        .unwrap();

    let outcome = prepare_image(
        &encoded.into_inner(),
        ImageLimits::new(20_000).unwrap(),
        QualityThresholds::new(32, 32).unwrap(),
    )
    .unwrap();

    assert!(matches!(
        outcome,
        PreparationOutcome::RequestBetterSource(_)
    ));
}

fn jpeg_with_orientation(width: u32, height: u32, orientation: u16) -> Vec<u8> {
    let image = GrayImage::from_fn(width, height, |x, _| {
        Luma([if x % 2 == 0 { 0 } else { 255 }])
    });
    let mut jpeg = Cursor::new(Vec::new());
    DynamicImage::ImageLuma8(image)
        .write_to(&mut jpeg, ImageFormat::Jpeg)
        .unwrap();
    let jpeg = jpeg.into_inner();

    let mut tiff = vec![
        b'I', b'I', 42, 0, 8, 0, 0, 0, 1, 0, 0x12, 0x01, 3, 0, 1, 0, 0, 0,
    ];
    tiff.extend_from_slice(&orientation.to_le_bytes());
    tiff.extend_from_slice(&[0, 0, 0, 0, 0, 0]);
    let mut app1 = b"Exif\0\0".to_vec();
    app1.extend_from_slice(&tiff);
    let length = u16::try_from(app1.len() + 2).unwrap().to_be_bytes();

    let mut encoded = vec![0xff, 0xd8, 0xff, 0xe1, length[0], length[1]];
    encoded.extend_from_slice(&app1);
    encoded.extend_from_slice(&jpeg[2..]);
    encoded
}
