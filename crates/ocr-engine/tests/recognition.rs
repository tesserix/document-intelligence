use ocr_engine::{assemble_recognitions, Error, RecognitionAlternative, RecognizedRegion};

#[test]
fn recognizer_output_is_validated_and_ordered_by_region() {
    let outputs = vec![
        RecognizedRegion::new(1, "Total 42.00", 0.91, vec![]).unwrap(),
        RecognizedRegion::new(
            0,
            "Invoice",
            0.98,
            vec![RecognitionAlternative::new("lnvoice", 0.11).unwrap()],
        )
        .unwrap(),
    ];

    let ordered = assemble_recognitions(outputs, 2).unwrap();

    assert_eq!(ordered[0].region_index(), 0);
    assert_eq!(ordered[0].text(), "Invoice");
    assert_eq!(ordered[1].region_index(), 1);
}

#[test]
fn duplicate_missing_and_excess_region_outputs_fail_closed() {
    let duplicate = vec![
        RecognizedRegion::new(0, "one", 0.9, vec![]).unwrap(),
        RecognizedRegion::new(0, "duplicate", 0.8, vec![]).unwrap(),
    ];
    assert!(matches!(
        assemble_recognitions(duplicate, 2),
        Err(Error::InvalidRecognitionBatch)
    ));
    assert!(assemble_recognitions(vec![], 1).is_err());
    assert!(assemble_recognitions(
        vec![RecognizedRegion::new(1, "outside", 0.9, vec![]).unwrap()],
        1
    )
    .is_err());
}

#[test]
fn recognizer_contract_rejects_unbounded_or_invalid_values() {
    assert!(RecognizedRegion::new(0, "", 0.9, vec![]).is_err());
    assert!(RecognizedRegion::new(0, "   ", 0.9, vec![]).is_err());
    assert!(RecognizedRegion::new(0, "text", f64::NAN, vec![]).is_err());
    assert!(RecognitionAlternative::new("", 0.5).is_err());
    assert!(RecognizedRegion::new(
        0,
        "text",
        0.9,
        (0..17)
            .map(|_| RecognitionAlternative::new("alternative", 0.1).unwrap())
            .collect()
    )
    .is_err());
    assert!(
        serde_json::from_value::<RecognizedRegion>(serde_json::json!({
            "region_index": 0,
            "text": "text",
            "confidence": 1.1,
            "alternatives": []
        }))
        .is_err()
    );
}
