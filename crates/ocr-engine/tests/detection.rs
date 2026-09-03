use ocr_engine::{select_regions, DetectionCandidate, DetectionLimits, Error};

#[test]
fn detector_candidates_reject_non_finite_out_of_bounds_and_explosive_output() {
    assert!(DetectionCandidate::new(f64::NAN, 0.1, 0.5, 0.5, 0.9).is_err());
    assert!(DetectionCandidate::new(0.8, 0.1, 0.5, 0.5, 0.9).is_err());
    assert!(DetectionCandidate::new(0.1, 0.1, 0.5, 1.1, 0.9).is_err());
    assert!(DetectionCandidate::new(0.1, 0.1, 0.5, 0.5, f64::INFINITY).is_err());
    assert!(DetectionLimits::new(100, 0.0).is_err());

    let candidate = DetectionCandidate::new(0.1, 0.1, 0.5, 0.5, 0.9).unwrap();
    assert!(matches!(
        select_regions(&[candidate; 3], DetectionLimits::new(2, 0.5).unwrap()),
        Err(Error::DetectionOutputLimitExceeded {
            candidates: 3,
            limit: 2
        })
    ));
}

#[test]
fn overlapping_regions_are_suppressed_and_returned_in_reading_order() {
    let candidates = vec![
        DetectionCandidate::new(0.50, 0.60, 0.90, 0.80, 0.91).unwrap(),
        DetectionCandidate::new(0.11, 0.11, 0.51, 0.31, 0.80).unwrap(),
        DetectionCandidate::new(0.10, 0.10, 0.50, 0.30, 0.95).unwrap(),
    ];

    let selected = select_regions(&candidates, DetectionLimits::new(100, 0.5).unwrap()).unwrap();

    assert_eq!(selected.len(), 2);
    assert_eq!(selected[0].bounds(), (0.10, 0.10, 0.50, 0.30));
    assert_eq!(selected[1].bounds(), (0.50, 0.60, 0.90, 0.80));
    assert_eq!(selected[0].confidence(), 0.95);
}

#[test]
fn detector_contract_deserialization_cannot_bypass_validation() {
    assert!(
        serde_json::from_value::<DetectionCandidate>(serde_json::json!({
            "left": 0.8,
            "top": 0.1,
            "right": 0.5,
            "bottom": 0.5,
            "confidence": 0.9
        }))
        .is_err()
    );
}
