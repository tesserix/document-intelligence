use ocr_domain::{PageGeometry, PageNumber};

#[test]
fn page_geometry_is_bounded_and_round_trips_without_losing_page_identity() {
    let geometry = PageGeometry::new(PageNumber::new(2).unwrap(), 2_550, 3_300).unwrap();

    assert_eq!(geometry.pixels(), 8_415_000);
    assert_eq!(u32::from(geometry.page), 2);
    assert_eq!(
        serde_json::to_string(&geometry).unwrap(),
        r#"{"page":2,"width":2550,"height":3300}"#
    );
}

#[test]
fn page_geometry_deserialization_rejects_unbounded_or_invalid_dimensions() {
    for encoded in [
        r#"{"page":0,"width":1,"height":1}"#,
        r#"{"page":1,"width":0,"height":1}"#,
        r#"{"page":1,"width":100001,"height":1000}"#,
        r#"{"page":1,"width":1,"height":1,"extra":true}"#,
    ] {
        assert!(
            serde_json::from_str::<PageGeometry>(encoded).is_err(),
            "{encoded}"
        );
    }
}
