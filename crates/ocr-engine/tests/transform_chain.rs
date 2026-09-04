use ocr_engine::{CropRegion, DeskewAngle, Rotation, TransformChain, TransformStep};

#[test]
fn crop_and_rotation_map_derived_coordinates_back_to_the_original_page() {
    let mut chain = TransformChain::new();
    chain.push_crop(CropRegion::new(0.1, 0.2, 0.5, 0.4).unwrap());
    chain.push_rotation(Rotation::Clockwise90);

    let original = chain.map_to_original(0.25, 0.75).unwrap();

    assert!((original.0 - 0.475).abs() < 1e-12);
    assert!((original.1 - 0.5).abs() < 1e-12);
    assert_eq!(chain.len(), 2);
}

#[test]
fn invalid_geometry_and_coordinates_fail_closed() {
    assert!(CropRegion::new(0.8, 0.2, 0.3, 0.4).is_err());
    assert!(CropRegion::new(0.1, 0.2, 0.0, 0.4).is_err());
    assert!(CropRegion::new(f64::NAN, 0.2, 0.5, 0.4).is_err());
    assert!(TransformChain::new().map_to_original(-0.1, 0.5).is_err());
}

#[test]
fn transform_provenance_round_trips_without_bypassing_crop_validation() {
    let mut chain = TransformChain::new();
    chain.push_crop(CropRegion::new(0.1, 0.2, 0.5, 0.4).unwrap());
    chain.push_rotation(Rotation::Clockwise270);

    let encoded = serde_json::to_value(&chain).unwrap();
    let decoded: TransformChain = serde_json::from_value(encoded.clone()).unwrap();

    assert_eq!(decoded, chain);
    assert_eq!(encoded["steps"][0]["kind"], "crop");
    assert_eq!(encoded["steps"][1]["rotation"], "clockwise_270");
    assert!(serde_json::from_value::<TransformChain>(serde_json::json!({
        "steps": [{
            "kind": "crop",
            "region": {"left": 0.8, "top": 0.2, "width": 0.3, "height": 0.4}
        }]
    }))
    .is_err());
}

#[test]
fn deskew_transform_maps_corrected_coordinates_back_to_the_original_page() {
    let angle = DeskewAngle::new(10.0).unwrap();
    let mut chain = TransformChain::new();
    chain.push_deskew(angle);
    let radians = 10.0_f64.to_radians();
    let corrected = (0.5 + 0.3 * radians.cos(), 0.5 + 0.3 * radians.sin());

    let original = chain.map_to_original(corrected.0, corrected.1).unwrap();

    assert!((original.0 - 0.8).abs() < 1e-12);
    assert!((original.1 - 0.5).abs() < 1e-12);
    assert_eq!(chain.steps(), &[TransformStep::Deskew { angle }]);
    assert!(DeskewAngle::new(12.01).is_err());
    assert!(DeskewAngle::new(f64::NAN).is_err());

    let encoded = serde_json::to_value(&chain).unwrap();
    assert_eq!(encoded["steps"][0]["kind"], "deskew");
    assert!(serde_json::from_value::<TransformChain>(serde_json::json!({
        "steps": [{"kind": "deskew", "angle": {"degrees": 13.0}}]
    }))
    .is_err());
}
