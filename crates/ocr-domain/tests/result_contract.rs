use std::collections::BTreeMap;

use ocr_domain::{
    Confidence, ConfidenceDimensions, Cost, DocumentId, DocumentResult, DocumentResultPayload,
    DocumentTable, DocumentVersion, Evidence, ExtractedValue, NormalizedPoint, PageNumber, Polygon,
    ProcessingProvenance, StableCode, TableCell, TableId, ValidationFailure, ValidationSeverity,
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
        DocumentResultPayload {
            fields,
            ..DocumentResultPayload::default()
        },
    )
    .unwrap();

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
    assert!(serde_json::from_value::<DocumentResult>(json!({
        "schema_version": "2.0",
        "document_id": "doc_TEST",
        "document_version": format!("sha256:{}", "a".repeat(64)),
        "content_trust": "untrusted",
        "fields": {}
    }))
    .is_err());
}

#[test]
fn result_serializes_the_complete_provider_neutral_contract() {
    let source = evidence();
    let result = DocumentResult::new(
        DocumentId::new("doc_01JCOMPLETE").unwrap(),
        DocumentVersion::new(&format!("sha256:{}", "b".repeat(64))).unwrap(),
        DocumentResultPayload {
            text: "Invoice INV-1048".to_owned(),
            markdown: "# Invoice INV-1048".to_owned(),
            fields: BTreeMap::new(),
            tables: vec![DocumentTable::new(
                TableId::new("tbl_TOTALS").unwrap(),
                vec![TableCell::new(
                    0,
                    0,
                    "Total",
                    Confidence::new(0.97).unwrap(),
                    vec![source.clone()],
                )
                .unwrap()],
            )
            .unwrap()],
            confidence: Some(ConfidenceDimensions::new(0.95, 0.96, 0.97, 0.98, 1.0, 0.96).unwrap()),
            citations: vec![source],
            warnings: vec![StableCode::new("low_input_quality").unwrap()],
            validation_failures: vec![ValidationFailure::new(
                StableCode::new("subtotal_mismatch").unwrap(),
                ValidationSeverity::Warning,
            )],
            provenance: Some(
                ProcessingProvenance::new(
                    "tesserix-native",
                    "detector-recognizer-1.0.0",
                    "printed-en-cpu-1",
                    1234,
                )
                .unwrap(),
            ),
            cost: Some(Cost::new("AUD", "0.0125").unwrap()),
        },
    )
    .unwrap();

    let encoded = serde_json::to_value(result).unwrap();
    assert_eq!(encoded["text"], "Invoice INV-1048");
    assert_eq!(encoded["tables"][0]["cells"][0]["text"], "Total");
    assert_eq!(encoded["confidence"]["overall"], 0.96);
    assert_eq!(encoded["validation_failures"][0]["severity"], "warning");
    assert_eq!(encoded["provider"], "tesserix-native");
    assert_eq!(encoded["processing_profile_version"], "printed-en-cpu-1");
    assert_eq!(encoded["duration_ms"], 1234);
    assert_eq!(encoded["cost"]["currency"], "AUD");
}

#[test]
fn complete_result_rejects_uncited_content_and_invalid_metadata() {
    let empty = DocumentResultPayload {
        text: "uncited".to_owned(),
        ..DocumentResultPayload::default()
    };
    assert!(DocumentResult::new(
        DocumentId::new("doc_UNCITED").unwrap(),
        DocumentVersion::new(&format!("sha256:{}", "c".repeat(64))).unwrap(),
        empty,
    )
    .is_err());
    assert!(StableCode::new("Not Stable").is_err());
    assert!(Cost::new("usd", "NaN").is_err());
    assert!(ProcessingProvenance::new("", "model", "profile", 1).is_err());
    assert!(TableCell::new(0, 0, "Total", Confidence::new(0.9).unwrap(), vec![]).is_err());
}
