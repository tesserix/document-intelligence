use ocr_eval::{
    aggregate_text_evaluations, evaluate_text, Error, TextEvaluationLimits,
    NORMALIZATION_POLICY_VERSION,
};

#[test]
fn exact_and_substituted_text_have_deterministic_character_and_word_errors() {
    let limits = TextEvaluationLimits::new(100, 100, 10_000).unwrap();

    let exact = evaluate_text("invoice total", "invoice total", limits).unwrap();
    let substituted = evaluate_text("invoice total", "invoice totals", limits).unwrap();

    assert_eq!(exact.policy_version(), NORMALIZATION_POLICY_VERSION);
    assert_eq!(exact.characters().edits(), 0);
    assert_eq!(exact.characters().reference_units(), 13);
    assert_eq!(exact.characters().rate(), Some(0.0));
    assert_eq!(exact.words().edits(), 0);
    assert_eq!(exact.words().reference_units(), 2);
    assert_eq!(substituted.characters().edits(), 1);
    assert_eq!(substituted.characters().rate(), Some(1.0 / 13.0));
    assert_eq!(substituted.words().edits(), 1);
    assert_eq!(substituted.words().rate(), Some(0.5));
}

#[test]
fn corpus_rates_sum_edits_and_reference_units_instead_of_averaging_documents() {
    let limits = TextEvaluationLimits::new(100, 100, 10_000).unwrap();
    let evaluations = [
        evaluate_text("a", "", limits).unwrap(),
        evaluate_text("123456789", "123456789", limits).unwrap(),
    ];

    let corpus = aggregate_text_evaluations(&evaluations).unwrap();

    assert_eq!(corpus.characters().edits(), 1);
    assert_eq!(corpus.characters().reference_units(), 10);
    assert_eq!(corpus.characters().rate(), Some(0.1));
    assert_eq!(corpus.words().edits(), 1);
    assert_eq!(corpus.words().reference_units(), 2);
    assert_eq!(corpus.words().rate(), Some(0.5));
}

#[test]
fn unicode_normalization_and_whitespace_tokenization_are_versioned_and_stable() {
    let limits = TextEvaluationLimits::new(100, 100, 10_000).unwrap();

    let unicode = evaluate_text("café", "cafe\u{301}", limits).unwrap();
    let whitespace = evaluate_text("invoice\n\ttotal", "invoice total", limits).unwrap();

    assert_eq!(unicode.characters().edits(), 0);
    assert_eq!(unicode.characters().reference_units(), 4);
    assert_eq!(unicode.words().edits(), 0);
    assert_eq!(whitespace.words().edits(), 0);
    assert_eq!(whitespace.words().reference_units(), 2);
}

#[test]
fn empty_reference_and_insertion_heavy_results_are_not_reported_as_zero() {
    let limits = TextEvaluationLimits::new(100, 100, 10_000).unwrap();

    let both_empty = evaluate_text("", "", limits).unwrap();
    let undefined = evaluate_text("", "invented", limits).unwrap();
    let insertion_heavy = evaluate_text("a", "abcdef", limits).unwrap();

    assert_eq!(both_empty.characters().rate(), Some(0.0));
    assert_eq!(both_empty.words().rate(), Some(0.0));
    assert_eq!(undefined.characters().edits(), 8);
    assert_eq!(undefined.characters().rate(), None);
    assert_eq!(undefined.words().edits(), 1);
    assert_eq!(undefined.words().rate(), None);
    assert_eq!(insertion_heavy.characters().edits(), 5);
    assert_eq!(insertion_heavy.characters().rate(), Some(5.0));
}

#[test]
fn unit_and_comparison_limits_fail_before_unbounded_dynamic_programming() {
    let unit_limits = TextEvaluationLimits::new(2, 2, 4).unwrap();
    assert_eq!(
        evaluate_text("abc", "ab", unit_limits),
        Err(Error::InputLimitExceeded)
    );

    let work_limits = TextEvaluationLimits::new(10, 10, 9).unwrap();
    assert_eq!(
        evaluate_text("abc", "xyz", work_limits),
        Err(Error::ComparisonLimitExceeded)
    );
    assert_eq!(
        TextEvaluationLimits::new(1_000_001, 1, 1),
        Err(Error::InvalidLimits)
    );
    assert_eq!(
        TextEvaluationLimits::new(1, 1, 100_000_001),
        Err(Error::InvalidLimits)
    );
}

#[test]
fn metric_debug_output_contains_counts_but_never_document_text() {
    let limits = TextEvaluationLimits::new(100, 100, 10_000).unwrap();
    let evaluation = evaluate_text(
        "passport number private-reference",
        "passport number private-candidate",
        limits,
    )
    .unwrap();

    let debug = format!("{evaluation:?}");

    assert!(debug.contains("reference_units"));
    assert!(!debug.contains("private-reference"));
    assert!(!debug.contains("private-candidate"));
}
