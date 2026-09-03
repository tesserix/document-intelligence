use std::collections::BTreeMap;

use ocr_domain::{
    Confidence, DocumentId, DocumentResult, DocumentVersion, Evidence, ExtractedValue,
    NormalizedPoint, PageNumber, Polygon,
};
use serde_json::json;

fn evidence() -> Evidence {
    Evidence::new(
        PageNumber::new(1).unwrap(),
        Polygon::new(vec![
            NormalizedPoint::new(0.1, 0.1).unwrap(),
            NormalizedPoint::new(0.3, 0.1).unwrap(),
            NormalizedPoint::new(0.3, 0.2).unwrap(),
            NormalizedPoint::new(0.1, 0.2).unwrap(),
        ])
        .unwrap(),
        "obs_invoice_number_1".try_into().unwrap(),
    )
}

#[test]
fn result_serializes_untrusted_content_with_source_evidence() {
    let mut fields = BTreeMap::new();
    fields.insert(
        "invoice_number".to_owned(),
        ExtractedValue::new(
            json!("INV-1048"),
            Confidence::new(0.98).unwrap(),
            vec![evidence()],
        )
        .unwrap(),
    );
    let result = DocumentResult::new(
        DocumentId::new("doc_01JTEST").unwrap(),
        DocumentVersion::new(&format!("sha256:{}", "a".repeat(64))).unwrap(),
        fields,
    );

    let encoded = serde_json::to_value(result).unwrap();
    assert_eq!(encoded["schema_version"], "1.0");
    assert_eq!(encoded["content_trust"], "untrusted");
    assert_eq!(encoded["fields"]["invoice_number"]["confidence"], 0.98);
    assert_eq!(
        encoded["fields"]["invoice_number"]["evidence"][0]["page"],
        1
    );
}

#[test]
fn confidence_rejects_non_finite_and_out_of_range_values() {
    for invalid in [f64::NEG_INFINITY, -0.01, 1.01, f64::INFINITY, f64::NAN] {
        assert!(Confidence::new(invalid).is_err(), "accepted {invalid:?}");
    }
}

#[test]
fn evidence_rejects_invalid_page_and_geometry() {
    assert!(PageNumber::new(0).is_err());
    assert!(NormalizedPoint::new(-0.01, 0.5).is_err());
    assert!(NormalizedPoint::new(0.5, 1.01).is_err());
    assert!(Polygon::new(vec![
        NormalizedPoint::new(0.1, 0.1).unwrap(),
        NormalizedPoint::new(0.2, 0.2).unwrap(),
    ])
    .is_err());
    assert!(Polygon::new(vec![
        NormalizedPoint::new(0.1, 0.1).unwrap(),
        NormalizedPoint::new(0.2, 0.2).unwrap(),
        NormalizedPoint::new(0.3, 0.3).unwrap(),
    ])
    .is_err());
}

#[test]
fn extracted_values_require_evidence() {
    let result = ExtractedValue::new(json!("INV-1048"), Confidence::new(0.98).unwrap(), vec![]);
    assert!(result.is_err());
}

#[test]
fn document_identifiers_and_versions_are_validated_at_construction() {
    assert!(DocumentId::new("tenant-name/invoice.pdf").is_err());
    assert!(DocumentVersion::new("sha256:not-a-digest").is_err());
}

#[test]
fn deserialization_cannot_bypass_domain_invariants() {
    assert!(serde_json::from_value::<Confidence>(json!(1.2)).is_err());
    assert!(serde_json::from_value::<PageNumber>(json!(0)).is_err());
    assert!(serde_json::from_value::<DocumentId>(json!("../../passport.pdf")).is_err());
    assert!(serde_json::from_value::<Polygon>(json!({
        "points": [
            {"x": 0.1, "y": 0.1},
            {"x": 0.2, "y": 0.2},
            {"x": 0.3, "y": 0.3}
        ]
    }))
    .is_err());
    assert!(serde_json::from_value::<ExtractedValue>(json!({
        "value": "INV-1048",
        "confidence": 0.98,
        "evidence": []
    }))
    .is_err());
}
