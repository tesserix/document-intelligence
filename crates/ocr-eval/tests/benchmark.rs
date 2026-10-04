use ocr_eval::score_benchmark;

#[test]
fn scores_text_fields_latency_and_never_drops_missing_cases() {
    let manifest = r#"{"cases":[{"id":"receipt","sha256":"abc","reference_text":"TOTAL 18.90","expected_fields":{}},{"id":"label","sha256":"def","expected_fields":{"protein_g":10.0}}]}"#;
    let results = r#"[{"id":"receipt","sha256":"abc","status":"completed","elapsed_ms":100,"text":"TOTAL 18.90","fields":{}},{"id":"label","sha256":"def","status":"review_required","elapsed_ms":300,"text":"","fields":{"protein_g":{"value":10.0}}}]"#;
    let report = score_benchmark(manifest, results).unwrap();
    assert_eq!(report.cases.len(), 2);
    assert_eq!(report.cases[0].word_error_rate, Some(0.0));
    assert_eq!(report.cases[1].correct_fields, 1);
    assert_eq!(report.p50_ms, 100);
    assert_eq!(report.p95_ms, 300);
    assert!(score_benchmark(manifest, "[]").is_err());
    assert!(score_benchmark(manifest, &results.replace("abc", "changed")).is_err());
}

#[test]
fn counts_wrong_and_missing_values_and_keeps_failed_cases_in_the_report() {
    let manifest =
        r#"{"cases":[{"id":"label","sha256":"a","expected_fields":{"protein":10,"fat":4}}]}"#;
    let results = r#"[{"id":"label","sha256":"a","status":"failed","elapsed_ms":10,"text":"","fields":{"protein":{"value":99}}}]"#;
    let report = score_benchmark(manifest, results).unwrap();
    assert_eq!(report.cases[0].wrong_fields, 1);
    assert_eq!(report.cases[0].missing_fields, 1);
    assert_eq!(report.cases[0].status, "failed");
    assert!(score_benchmark(
        manifest,
        &format!(
            "[{},{}]",
            &results[1..results.len() - 1],
            &results[1..results.len() - 1]
        )
    )
    .is_err());
    assert!(score_benchmark("{\"cases\":[]}", "[]").is_err());
    assert!(score_benchmark(manifest, &results.replace("failed", "invented_status")).is_err());
}

#[test]
fn compares_nested_numeric_values_without_integer_float_false_positives() {
    let manifest = r#"{"cases":[{"id":"label","sha256":"a","expected_fields":{"serving_size":{"amount":50,"unit":"g"}}}]}"#;
    let samples = r#"[{"id":"label","sha256":"a","status":"completed","elapsed_ms":1,"text":"","fields":{"serving_size":{"value":{"amount":50.0,"unit":"g"}}}}]"#;
    assert_eq!(
        score_benchmark(manifest, samples).unwrap().cases[0].correct_fields,
        1
    );
    assert_eq!(
        score_benchmark(manifest, &samples.replace("50.0", "50.1"))
            .unwrap()
            .cases[0]
            .wrong_fields,
        1
    );
}
