use std::{fs, path::PathBuf};

use serde_json::Value;
use sha2::{Digest, Sha256};

type Mutation = (&'static str, Box<dyn Fn(&mut Value)>);

fn contract(name: &str) -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../contracts/v1")
        .join(name);
    let bytes = fs::read(&path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    serde_json::from_slice(&bytes)
        .unwrap_or_else(|error| panic!("parse {}: {error}", path.display()))
}

#[test]
fn openapi_covers_every_implemented_v1_route() {
    let openapi = contract("openapi.json");
    assert_eq!(openapi["openapi"], "3.1.0");
    let expected = [
        ("/v1/ocr/uploads", "post"),
        ("/v1/ocr/uploads/{upload_id}", "get"),
        ("/v1/ocr/uploads/{upload_id}/complete", "post"),
        ("/v1/ocr/jobs", "post"),
        ("/v1/ocr/jobs/{job_id}", "get"),
        ("/v1/ocr/jobs/{job_id}/result", "get"),
        ("/v1/ocr/jobs/{job_id}/cancel", "post"),
    ];
    for (path, method) in expected {
        assert!(
            openapi["paths"][path][method].is_object(),
            "missing {method} {path}"
        );
    }
}

fn complete_result() -> Value {
    let digest = format!("sha256:{}", "a".repeat(64));
    serde_json::json!({
        "schema_version": "1.0",
        "document_id": "doc_CONTRACT",
        "document_version": digest,
        "content_trust": "untrusted",
        "text": "Total 12.50",
        "markdown": "Total **12.50**",
        "pages": [{
            "page": 1,
            "width": 1200,
            "height": 1600,
            "observations": [{
                "observation_id": "obs_TOTAL",
                "level": "word",
                "text": "12.50",
                "confidence": 0.98,
                "polygon": {"points": [
                    {"x": 0.7, "y": 0.8},
                    {"x": 0.9, "y": 0.8},
                    {"x": 0.9, "y": 0.85},
                    {"x": 0.7, "y": 0.85}
                ]},
                "reading_order": 0,
                "parent_observation_id": null
            }]
        }],
        "fields": {
            "total": {
                "value": {"currency": "AUD", "decimal": "12.50"},
                "confidence": 0.97,
                "evidence": [{
                    "page": 1,
                    "polygon": {"points": [
                        {"x": 0.7, "y": 0.8},
                        {"x": 0.9, "y": 0.8},
                        {"x": 0.9, "y": 0.85},
                        {"x": 0.7, "y": 0.85}
                    ]},
                    "observation_id": "obs_TOTAL"
                }]
            }
        },
        "tables": [{
            "table_id": "tbl_TOTALS",
            "cells": [{
                "row": 0,
                "column": 0,
                "text": "12.50",
                "confidence": 0.96,
                "evidence": [{
                    "page": 1,
                    "polygon": {"points": [
                        {"x": 0.7, "y": 0.8},
                        {"x": 0.9, "y": 0.8},
                        {"x": 0.9, "y": 0.85},
                        {"x": 0.7, "y": 0.85}
                    ]},
                    "observation_id": "obs_TOTAL"
                }]
            }]
        }],
        "confidence": {
            "input_quality": 0.95,
            "ocr": 0.98,
            "classification": 0.90,
            "extraction": 0.97,
            "validation": 1.0,
            "overall": 0.95
        },
        "citations": [{
            "page": 1,
            "polygon": {"points": [
                {"x": 0.7, "y": 0.8},
                {"x": 0.9, "y": 0.8},
                {"x": 0.9, "y": 0.85},
                {"x": 0.7, "y": 0.85}
            ]},
            "observation_id": "obs_TOTAL"
        }],
        "warnings": ["low_input_quality"],
        "validation_failures": [{"code": "total_mismatch", "severity": "warning"}],
        "provider": "tesserix",
        "model_version": "fixture-1.0.0",
        "processing_profile_version": "printed-en-v1",
        "duration_ms": 42,
        "cost": {"currency": "AUD", "decimal": "0.0012"}
    })
}

#[test]
fn canonical_result_schema_accepts_the_rust_domain_result() {
    let schema = contract("document-result.schema.json");
    let validator = jsonschema::validator_for(&schema).unwrap();
    let typed: ocr_domain::DocumentResult = serde_json::from_value(complete_result()).unwrap();
    let serialized = serde_json::to_value(typed).unwrap();
    assert!(validator.is_valid(&serialized));
}

#[test]
fn canonical_result_schema_rejects_unsafe_or_ambiguous_results() {
    let schema = contract("document-result.schema.json");
    let validator = jsonschema::validator_for(&schema).unwrap();
    let cases: [Mutation; 9] = [
        (
            "trusted content",
            Box::new(|value| value["content_trust"] = "trusted".into()),
        ),
        (
            "missing field evidence",
            Box::new(|value| value["fields"]["total"]["evidence"] = serde_json::json!([])),
        ),
        (
            "confidence above one",
            Box::new(|value| value["confidence"]["overall"] = 1.01.into()),
        ),
        (
            "coordinate outside page",
            Box::new(|value| value["citations"][0]["polygon"]["points"][0]["x"] = (-0.01).into()),
        ),
        (
            "short polygon",
            Box::new(|value| {
                value["citations"][0]["polygon"]["points"] =
                    serde_json::json!([{"x": 0.0, "y": 0.0}, {"x": 1.0, "y": 1.0}])
            }),
        ),
        (
            "floating point cost",
            Box::new(|value| value["cost"]["decimal"] = 0.0012.into()),
        ),
        (
            "storage locator",
            Box::new(|value| value["bucket"] = "private-source".into()),
        ),
        (
            "tenant identity",
            Box::new(|value| value["tenant_id"] = "tenant_other".into()),
        ),
        (
            "credential",
            Box::new(|value| value["credential"] = "not-a-real-secret".into()),
        ),
    ];

    for (name, mutate) in cases {
        let mut value = complete_result();
        mutate(&mut value);
        assert!(!validator.is_valid(&value), "accepted {name}");
    }
}

fn create_job_request() -> Value {
    serde_json::json!({
        "source": {"upload_id": "upl_CONTRACT"},
        "document_type": "auto",
        "output": {"text": true, "markdown": true, "layout": true, "evidence": true},
        "extraction": {"schema_id": "invoice", "schema_version": "1.0"},
        "language_hints": ["en-AU"],
        "processing_class": "interactive",
        "webhook_subscription_id": "whs_CONTRACT"
    })
}

#[test]
fn canonical_create_job_schema_matches_the_openapi_contract() {
    let openapi = contract("openapi.json");
    assert_eq!(
        openapi["components"]["schemas"]["CreateJobRequest"]["$ref"],
        "./create-job.schema.json"
    );
    let schema = contract("create-job.schema.json");
    let validator = jsonschema::validator_for(&schema).unwrap();
    assert!(validator.is_valid(&create_job_request()));
}

#[test]
fn canonical_create_job_schema_rejects_identity_locators_and_unbounded_values() {
    let schema = contract("create-job.schema.json");
    let validator = jsonschema::validator_for(&schema).unwrap();
    let cases: [Mutation; 8] = [
        (
            "product identity",
            Box::new(|value| value["product_id"] = "kora".into()),
        ),
        (
            "tenant identity",
            Box::new(|value| value["tenant_id"] = "ten_OTHER".into()),
        ),
        (
            "arbitrary URL",
            Box::new(|value| value["document_uri"] = "https://attacker.invalid/file".into()),
        ),
        (
            "provider credential",
            Box::new(|value| value["provider_api_key"] = "not-a-real-secret".into()),
        ),
        (
            "invalid upload id",
            Box::new(|value| value["source"]["upload_id"] = "../source".into()),
        ),
        (
            "too many language hints",
            Box::new(|value| {
                value["language_hints"] =
                    serde_json::json!(["en", "fr", "de", "it", "es", "pt", "nl", "sv", "da"])
            }),
        ),
        (
            "duplicate language hint",
            Box::new(|value| value["language_hints"] = serde_json::json!(["en-AU", "en-AU"])),
        ),
        (
            "oversized schema id",
            Box::new(|value| value["extraction"]["schema_id"] = "x".repeat(129).into()),
        ),
    ];

    for (name, mutate) in cases {
        let mut value = create_job_request();
        mutate(&mut value);
        assert!(!validator.is_valid(&value), "accepted {name}");
    }
}

#[test]
fn manifest_pins_every_canonical_contract_by_sha256() {
    let manifest = contract("manifest.json");
    assert_eq!(manifest["contract_version"], "1.0.0");
    assert_eq!(manifest["compatibility_status"], "pre-release");
    for name in [
        "openapi.json",
        "create-job.schema.json",
        "document-result.schema.json",
        "fixtures/product-alpha/create-job.json",
        "fixtures/product-alpha/document-result.json",
        "fixtures/product-beta/create-job.json",
        "fixtures/product-beta/document-result.json",
        "fixtures/negative-cases.json",
    ] {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../contracts/v1")
            .join(name);
        let digest = format!("{:x}", Sha256::digest(fs::read(path).unwrap()));
        assert_eq!(manifest["artifacts"][name]["sha256"], digest, "{name}");
    }
}

#[test]
fn reusable_fixtures_validate_for_two_product_consumers() {
    let job_schema = contract("create-job.schema.json");
    let job_validator = jsonschema::validator_for(&job_schema).unwrap();
    let result_schema = contract("document-result.schema.json");
    let result_validator = jsonschema::validator_for(&result_schema).unwrap();

    for product in ["product-alpha", "product-beta"] {
        let job = contract(&format!("fixtures/{product}/create-job.json"));
        let result = contract(&format!("fixtures/{product}/document-result.json"));
        assert!(
            job_validator.is_valid(&job),
            "invalid job fixture for {product}"
        );
        assert!(
            result_validator.is_valid(&result),
            "invalid result fixture for {product}"
        );
        assert!(
            job.get("product_id").is_none(),
            "identity leaked for {product}"
        );
        assert!(
            job.get("tenant_id").is_none(),
            "identity leaked for {product}"
        );
    }
}

#[test]
fn reusable_negative_fixture_mutations_are_rejected() {
    let cases = contract("fixtures/negative-cases.json");
    for case in cases.as_array().unwrap() {
        let schema_name = case["schema"].as_str().unwrap();
        let schema = contract(schema_name);
        let validator = jsonschema::validator_for(&schema).unwrap();
        let mut value = contract(case["base_fixture"].as_str().unwrap());
        let pointer = case["pointer"].as_str().unwrap();
        let replacement = case["value"].clone();
        if let Some(target) = value.pointer_mut(pointer) {
            *target = replacement;
        } else {
            let property = pointer.strip_prefix('/').unwrap();
            value
                .as_object_mut()
                .unwrap()
                .insert(property.to_owned(), replacement);
        }
        assert!(
            !validator.is_valid(&value),
            "accepted negative fixture {}",
            case["name"]
        );
    }
}

#[test]
fn committed_contract_json_is_byte_for_byte_deterministic() {
    for name in [
        "openapi.json",
        "create-job.schema.json",
        "document-result.schema.json",
        "manifest.json",
        "fixtures/product-alpha/create-job.json",
        "fixtures/product-alpha/document-result.json",
        "fixtures/product-beta/create-job.json",
        "fixtures/product-beta/document-result.json",
        "fixtures/negative-cases.json",
    ] {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../contracts/v1")
            .join(name);
        let committed = fs::read_to_string(&path).unwrap();
        let parsed: Value = serde_json::from_str(&committed).unwrap();
        let regenerated = format!("{}\n", serde_json::to_string_pretty(&parsed).unwrap());
        assert_eq!(committed, regenerated, "regeneration changed {name}");
    }
}
