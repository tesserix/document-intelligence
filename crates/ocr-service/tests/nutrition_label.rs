use ocr_domain::{
    Confidence, DocumentId, DocumentPage, DocumentResult, DocumentVersion, NormalizedPoint,
    ObservationLevel, PageNumber, Polygon, TextObservation, ValidationSeverity,
};
use ocr_service::{assemble_document_result, ExtractionSchema};
use serde_json::{json, Value};

fn polygon() -> Polygon {
    Polygon::new(vec![
        NormalizedPoint::new(0.1, 0.1).unwrap(),
        NormalizedPoint::new(0.9, 0.1).unwrap(),
        NormalizedPoint::new(0.9, 0.2).unwrap(),
        NormalizedPoint::new(0.1, 0.2).unwrap(),
    ])
    .unwrap()
}

fn label(lines: &[&str]) -> DocumentPage {
    let observations = lines
        .iter()
        .enumerate()
        .map(|(index, text)| {
            TextObservation::new(
                format!("obs_line{index}").as_str().try_into().unwrap(),
                ObservationLevel::Line,
                *text,
                Confidence::new(0.9).unwrap(),
                polygon(),
                index as u32,
                None,
            )
            .unwrap()
        })
        .collect();
    DocumentPage::new(PageNumber::new(1).unwrap(), 1000, 1400, observations).unwrap()
}

fn extract(lines: &[&str]) -> DocumentResult {
    assemble_document_result(
        DocumentId::new("doc_LABEL").unwrap(),
        DocumentVersion::new(&format!("sha256:{}", "c".repeat(64))).unwrap(),
        vec![label(lines)],
        ExtractionSchema::registered("kora.nutrition_label", "1"),
    )
    .unwrap()
}

fn field(result: &DocumentResult, name: &str) -> Option<Value> {
    result.fields.get(name).map(|value| value.value.clone())
}

fn failure_codes(result: &DocumentResult) -> Vec<String> {
    result
        .validation_failures
        .iter()
        .map(|failure| String::from(failure.code.clone()))
        .collect()
}

const AU_LABEL: [&str; 14] = [
    "NUTRITION INFORMATION",
    "Servings per package: 8",
    "Serving size: 30g",
    "Avg Quantity Per Serving Per 100g",
    "Energy 540kJ (129Cal) 1800kJ (430Cal)",
    "Protein 3.2g 10.7g",
    "Fat, total 1.5g 5.0g",
    "- saturated 0.3g 1.0g",
    "Carbohydrate 20.1g 67.0g",
    "- sugars 4.5g 15.0g",
    "Dietary Fibre 3.0g 10.0g",
    "Sodium 120mg 400mg",
    "Ingredients: wholegrain oats, sugar.",
    "9300605000117",
];

#[test]
fn registers_only_kora_nutrition_label_v1() {
    assert!(ExtractionSchema::registered("kora.nutrition_label", "1").is_some());
    assert!(ExtractionSchema::registered("kora.nutrition_label", "2").is_none());
    assert!(ExtractionSchema::registered("invoice", "1.0").is_none());
}

#[test]
fn extracts_both_columns_of_an_australian_panel_with_evidence() {
    let result = extract(&AU_LABEL);

    assert_eq!(field(&result, "servings_per_pack"), Some(json!(8.0)));
    assert_eq!(
        field(&result, "serving_size"),
        Some(json!({"amount": 30.0, "unit": "g"}))
    );
    assert_eq!(
        field(&result, "column_headers"),
        Some(json!(["per_serving", "per_100g"]))
    );
    assert_eq!(field(&result, "per_serving.energy_kj"), Some(json!(540.0)));
    assert_eq!(
        field(&result, "per_serving.energy_kcal"),
        Some(json!(129.0))
    );
    assert_eq!(field(&result, "per_100g.energy_kj"), Some(json!(1800.0)));
    assert_eq!(field(&result, "per_100g.protein_g"), Some(json!(10.7)));
    assert_eq!(field(&result, "per_serving.fat_g"), Some(json!(1.5)));
    assert_eq!(field(&result, "per_100g.saturated_fat_g"), Some(json!(1.0)));
    assert_eq!(field(&result, "per_100g.carbohydrate_g"), Some(json!(67.0)));
    assert_eq!(field(&result, "per_serving.sugars_g"), Some(json!(4.5)));
    assert_eq!(field(&result, "per_100g.fibre_g"), Some(json!(10.0)));
    assert_eq!(field(&result, "per_serving.sodium_mg"), Some(json!(120.0)));
    assert_eq!(field(&result, "barcode"), Some(json!("9300605000117")));
    assert_eq!(field(&result, "locale"), Some(json!("en-AU")));

    let protein = &result.fields["per_100g.protein_g"];
    assert_eq!(f64::from(protein.confidence), 0.9);
    assert_eq!(
        String::from(protein.evidence[0].observation_id.clone()),
        "obs_line5"
    );
    assert!(
        failure_codes(&result).is_empty(),
        "{:?}",
        failure_codes(&result)
    );
}

#[test]
fn extracts_a_us_panel_ignoring_daily_value_percentages() {
    let result = extract(&[
        "Nutrition Facts",
        "8 servings per container",
        "Serving size 2/3 cup (55g)",
        "Amount per serving",
        "Calories 230",
        "Total Fat 8g 10%",
        "Saturated Fat 1g 5%",
        "Trans Fat 0g",
        "Sodium 160mg 7%",
        "Total Carbohydrate 37g 13%",
        "Dietary Fiber 4g 14%",
        "Total Sugars 12g",
        "Includes 10g Added Sugars 20%",
        "Protein 3g",
        "0012345678905",
    ]);

    assert_eq!(field(&result, "servings_per_pack"), Some(json!(8.0)));
    assert_eq!(
        field(&result, "serving_size"),
        Some(json!({"amount": 55.0, "unit": "g"}))
    );
    assert_eq!(
        field(&result, "column_headers"),
        Some(json!(["per_serving"]))
    );
    assert_eq!(
        field(&result, "per_serving.energy_kcal"),
        Some(json!(230.0))
    );
    assert_eq!(field(&result, "per_serving.fat_g"), Some(json!(8.0)));
    assert_eq!(field(&result, "per_serving.sugars_g"), Some(json!(12.0)));
    assert_eq!(field(&result, "per_serving.fibre_g"), Some(json!(4.0)));
    assert_eq!(field(&result, "barcode"), Some(json!("0012345678905")));
    assert_eq!(field(&result, "locale"), Some(json!("en-US")));
    assert!(
        failure_codes(&result).is_empty(),
        "{:?}",
        failure_codes(&result)
    );
}

#[test]
fn maps_unitless_indian_columns_by_header_order_and_label_unit() {
    let result = extract(&[
        "Nutritional Information (Approx.)",
        "Per 100g Per serve (30g)",
        "Energy (kcal) 389 117",
        "Protein (g) 10.2 3.1",
        "Carbohydrate (g) 70.1 21.0",
        "of which Sugars (g) 12.0 3.6",
        "Total Fat (g) 7.5 2.3",
        "Saturated Fat (g) 3.1 0.9",
        "Sodium (mg) 450 135",
        "FSSAI Lic. No. 10012345678901",
    ]);

    assert_eq!(
        field(&result, "column_headers"),
        Some(json!(["per_100g", "per_serving"]))
    );
    assert_eq!(
        field(&result, "serving_size"),
        Some(json!({"amount": 30.0, "unit": "g"}))
    );
    assert_eq!(field(&result, "per_100g.energy_kcal"), Some(json!(389.0)));
    assert_eq!(
        field(&result, "per_serving.energy_kcal"),
        Some(json!(117.0))
    );
    assert_eq!(field(&result, "per_serving.sugars_g"), Some(json!(3.6)));
    assert_eq!(field(&result, "per_100g.sodium_mg"), Some(json!(450.0)));
    assert_eq!(field(&result, "barcode"), None);
    assert_eq!(field(&result, "locale"), Some(json!("en-IN")));
    assert!(
        failure_codes(&result).is_empty(),
        "{:?}",
        failure_codes(&result)
    );
}

#[test]
fn normalises_units_and_skips_upper_bounds() {
    let result = extract(&[
        "Per 100g",
        "Energy 1,500 kJ",
        "Fat 0,5 g",
        "Sodium 0.4g",
        "Sugars <1g",
    ]);

    assert_eq!(field(&result, "per_100g.energy_kj"), Some(json!(1500.0)));
    assert_eq!(field(&result, "per_100g.fat_g"), Some(json!(0.5)));
    assert_eq!(field(&result, "per_100g.sodium_mg"), Some(json!(400.0)));
    assert_eq!(field(&result, "per_100g.sugars_g"), None);
}

#[test]
fn reports_inconsistencies_without_correcting_them() {
    let result = extract(&[
        "Serving size 50g",
        "Per serving Per 100g",
        "Energy 2000kJ (200kcal) 2000kJ (478kcal)",
        "Protein 2g 4g",
        "Fat 1g 30g",
        "Saturated fat 2g 31g",
        "Carbohydrate 5g 70g",
        "Sugars 6g 10g",
    ]);

    let codes = failure_codes(&result);
    for code in [
        "energy_unit_mismatch",
        "energy_atwater_mismatch",
        "saturated_fat_exceeds_fat",
        "sugars_exceed_carbohydrate",
        "per_100g_exceeds_100g",
        "serving_column_mismatch",
    ] {
        assert!(
            codes.contains(&code.to_owned()),
            "missing {code} in {codes:?}"
        );
    }
    assert_eq!(codes.len(), 6, "codes are reported once each: {codes:?}");
    assert!(result
        .validation_failures
        .iter()
        .all(|failure| failure.severity == ValidationSeverity::Warning));
    assert_eq!(
        field(&result, "per_serving.energy_kcal"),
        Some(json!(200.0))
    );
    assert_eq!(
        field(&result, "per_serving.saturated_fat_g"),
        Some(json!(2.0))
    );
}

#[test]
fn rejects_nutrient_rows_without_column_headers() {
    let result = extract(&["Protein 3.2g 10.7g", "Fat 1.5g 5.0g"]);

    assert_eq!(
        failure_codes(&result),
        vec!["nutrition_columns_unidentified"]
    );
    assert_eq!(
        result.validation_failures[0].severity,
        ValidationSeverity::Error
    );
    assert!(!result.fields.keys().any(|name| name.contains('.')));
}

#[test]
fn reports_a_missing_panel_and_ignores_an_invalid_barcode() {
    let result = extract(&["Best before 12/2027", "9300605000118"]);

    assert_eq!(failure_codes(&result), vec!["nutrition_panel_not_found"]);
    assert_eq!(field(&result, "barcode"), None);
}

#[test]
fn leaves_fields_empty_without_a_schema() {
    let result = assemble_document_result(
        DocumentId::new("doc_LABEL").unwrap(),
        DocumentVersion::new(&format!("sha256:{}", "c".repeat(64))).unwrap(),
        vec![label(&AU_LABEL)],
        None,
    )
    .unwrap();

    assert!(result.fields.is_empty());
    assert!(result.validation_failures.is_empty());
}

#[test]
fn extracts_separate_cells_from_production_ocr_with_original_evidence() {
    let pages =
        serde_json::from_str(include_str!("fixtures/nutrition-spatial-clear.json")).unwrap();
    let result = assemble_document_result(
        DocumentId::new("doc_SPATIAL").unwrap(),
        DocumentVersion::new(&format!("sha256:{}", "d".repeat(64))).unwrap(),
        pages,
        ExtractionSchema::registered("kora.nutrition_label", "1"),
    )
    .unwrap();
    assert_reference_nutrients(&result);
}

fn assert_reference_nutrients(result: &DocumentResult) {
    for (nutrient, expected) in [
        ("energy_kj", 836.8),
        ("energy_kcal", 200.0),
        ("protein_g", 10.0),
        ("fat_g", 4.0),
        ("saturated_fat_g", 1.0),
        ("carbohydrate_g", 31.0),
        ("sugars_g", 4.0),
        ("fibre_g", 3.0),
        ("sodium_mg", 200.0),
    ] {
        assert_eq!(
            field(result, &format!("per_100g.{nutrient}")),
            Some(json!(expected)),
            "{nutrient}"
        );
        assert_eq!(
            field(result, &format!("per_serving.{nutrient}")),
            Some(json!(expected / 2.0)),
            "{nutrient}"
        );
    }
    assert!(
        result.validation_failures.is_empty(),
        "{:?}",
        result.validation_failures
    );
    assert!(result.fields["per_100g.protein_g"].evidence.len() >= 2);
}

#[test]
fn extracts_spatial_nutrition_across_readable_quality_variants() {
    for (name, fixture) in [
        (
            "moderate_blur",
            include_str!("fixtures/nutrition-spatial-moderate_blur.json"),
        ),
        (
            "low_contrast",
            include_str!("fixtures/nutrition-spatial-low_contrast.json"),
        ),
        ("dark", include_str!("fixtures/nutrition-spatial-dark.json")),
        (
            "rotated_12deg",
            include_str!("fixtures/nutrition-spatial-rotated_12deg.json"),
        ),
        (
            "rotated_90deg",
            include_str!("fixtures/nutrition-spatial-rotated_90deg.json"),
        ),
        (
            "jpeg_artifacts",
            include_str!("fixtures/nutrition-spatial-jpeg_artifacts.json"),
        ),
    ] {
        eprintln!("case: {name}");
        let result = assemble_document_result(
            DocumentId::new("doc_SPATIAL").unwrap(),
            DocumentVersion::new(&format!("sha256:{}", "d".repeat(64))).unwrap(),
            serde_json::from_str(fixture).unwrap(),
            ExtractionSchema::registered("kora.nutrition_label", "1"),
        )
        .unwrap();
        assert_reference_nutrients(&result);
    }
}

#[test]
fn flags_unreadable_spatial_units_without_inventing_values() {
    let result = assemble_document_result(
        DocumentId::new("doc_LOWRES").unwrap(),
        DocumentVersion::new(&format!("sha256:{}", "d".repeat(64))).unwrap(),
        serde_json::from_str(include_str!(
            "fixtures/nutrition-spatial-low_resolution.json"
        ))
        .unwrap(),
        ExtractionSchema::registered("kora.nutrition_label", "1"),
    )
    .unwrap();
    assert_eq!(field(&result, "per_serving.fat_g"), None);
    assert_eq!(field(&result, "per_100g.sugars_g"), None);
    assert_eq!(field(&result, "per_100g.protein_g"), Some(json!(10.0)));
    assert!(failure_codes(&result).contains(&"nutrition_row_incomplete".to_owned()));
}

fn spatial_result(pages: Vec<DocumentPage>) -> DocumentResult {
    assemble_document_result(
        DocumentId::new("doc_SPATIAL").unwrap(),
        DocumentVersion::new(&format!("sha256:{}", "d".repeat(64))).unwrap(),
        pages,
        ExtractionSchema::registered("kora.nutrition_label", "1"),
    )
    .unwrap()
}

fn spatial_pages() -> Vec<DocumentPage> {
    serde_json::from_str(include_str!("fixtures/nutrition-spatial-clear.json")).unwrap()
}

#[test]
fn missing_cell_does_not_shift_the_second_column_into_the_first() {
    let mut pages = spatial_pages();
    pages[0].observations.retain(|o| o.text.trim() != "5.0 g");
    let result = spatial_result(pages);
    assert_eq!(field(&result, "per_serving.protein_g"), None);
    assert_eq!(field(&result, "per_100g.protein_g"), Some(json!(10.0)));
    assert!(failure_codes(&result).contains(&"nutrition_row_incomplete".to_owned()));
}

#[test]
fn rejects_ambiguous_spatial_cells_and_does_not_borrow_from_another_page() {
    let mut pages = spatial_pages();
    let mut duplicate = pages[0]
        .observations
        .iter()
        .find(|o| o.text.trim() == "5.0 g")
        .unwrap()
        .clone();
    duplicate.observation_id = "obs_DUPLICATE".try_into().unwrap();
    duplicate.text = "9.0 g".to_owned();
    pages[0].observations.push(duplicate);
    let result = spatial_result(pages);
    assert_eq!(field(&result, "per_serving.protein_g"), None);
    assert!(failure_codes(&result).contains(&"nutrition_row_incomplete".to_owned()));

    let mut pages = spatial_pages();
    let mut second = pages[0].clone();
    second.page = PageNumber::new(2).unwrap();
    second.observations.retain(|o| o.text.trim() == "5.0 g");
    pages[0].observations.retain(|o| o.text.trim() != "5.0 g");
    pages.push(second);
    let result = spatial_result(pages);
    assert_eq!(field(&result, "per_serving.protein_g"), None);
}

#[test]
fn unreadable_or_headerless_inputs_require_review_without_invented_nutrients() {
    for fixture in [
        include_str!("fixtures/nutrition-spatial-heavy_blur.json"),
        include_str!("fixtures/nutrition-spatial-cropped_headers.json"),
        include_str!("fixtures/nutrition-spatial-blank.json"),
    ] {
        let result = spatial_result(serde_json::from_str(fixture).unwrap());
        assert!(!result.validation_failures.is_empty());
        assert!(result.fields.keys().all(|key| !key.starts_with("per_")));
    }
}
