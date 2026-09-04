use std::io::Cursor;

use image::{DynamicImage, GrayImage, ImageFormat, Luma};
use lopdf::{dictionary, Document, Object, Stream};
use ocr_parser_sandbox::{inspect_document, DocumentLimits, Error};

fn pdf(page_count: usize, width_points: i64, height_points: i64) -> Vec<u8> {
    let mut document = Document::with_version("1.7");
    let pages_id = document.new_object_id();
    let mut page_ids = Vec::with_capacity(page_count);
    for _ in 0..page_count {
        let content_id = document.add_object(Stream::new(dictionary! {}, Vec::new()));
        let page_id = document.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "MediaBox" => vec![0.into(), 0.into(), width_points.into(), height_points.into()],
            "Contents" => content_id,
        });
        page_ids.push(Object::Reference(page_id));
    }
    document.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => page_ids,
            "Count" => page_count as i64,
        }),
    );
    let catalog_id = document.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
    });
    document.trailer.set("Root", catalog_id);
    let mut bytes = Vec::new();
    document.save_to(&mut bytes).unwrap();
    bytes
}

fn pdf_with_cyclic_page_parent() -> Vec<u8> {
    let mut document = Document::with_version("1.7");
    let pages_id = document.new_object_id();
    let page_id = document.new_object_id();
    let content_id = document.add_object(Stream::new(dictionary! {}, Vec::new()));
    document.objects.insert(
        page_id,
        Object::Dictionary(dictionary! {
            "Type" => "Page",
            "Parent" => page_id,
            "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
            "Contents" => content_id,
        }),
    );
    document.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => vec![Object::Reference(page_id)],
            "Count" => 1,
        }),
    );
    let catalog_id = document.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
    });
    document.trailer.set("Root", catalog_id);
    let mut bytes = Vec::new();
    document.save_to(&mut bytes).unwrap();
    bytes
}

fn pdf_with_compressed_content(decoded_bytes: usize) -> Vec<u8> {
    let mut document = Document::with_version("1.7");
    let pages_id = document.new_object_id();
    let mut content = Stream::new(dictionary! {}, vec![0; decoded_bytes]);
    content.compress().unwrap();
    let content_id = document.add_object(content);
    let page_id = document.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
        "Contents" => content_id,
    });
    document.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => vec![Object::Reference(page_id)],
            "Count" => 1,
        }),
    );
    let catalog_id = document.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
    });
    document.trailer.set("Root", catalog_id);
    let mut bytes = Vec::new();
    document.save_to(&mut bytes).unwrap();
    bytes
}

fn png() -> Vec<u8> {
    let image = GrayImage::from_pixel(2, 3, Luma([255]));
    let mut bytes = Cursor::new(Vec::new());
    DynamicImage::ImageLuma8(image)
        .write_to(&mut bytes, ImageFormat::Png)
        .unwrap();
    bytes.into_inner()
}

#[test]
fn reports_pdf_pages_and_render_pixel_budget_without_extracting_content() {
    let report = inspect_document(
        &pdf(2, 612, 792),
        "application/pdf",
        DocumentLimits::new(100, 20_000_000, 40_000_000).unwrap(),
    )
    .unwrap();

    assert_eq!(report.page_count, 2);
    assert_eq!(report.maximum_page_pixels, 8_415_000);
    assert_eq!(report.total_page_pixels, 16_830_000);
    assert_eq!(report.pages.len(), 2);
    assert_eq!(u32::from(report.pages[0].page), 1);
    assert_eq!(
        (report.pages[0].width, report.pages[0].height),
        (2550, 3300)
    );
    assert_eq!(u32::from(report.pages[1].page), 2);
    assert_eq!(
        (report.pages[1].width, report.pages[1].height),
        (2550, 3300)
    );
    assert!(!report.password_protected);
}

#[test]
fn rejects_page_and_pixel_bombs_before_rendering() {
    let too_many_pages = inspect_document(
        &pdf(3, 612, 792),
        "application/pdf",
        DocumentLimits::new(2, 20_000_000, 40_000_000).unwrap(),
    );
    assert!(matches!(
        too_many_pages,
        Err(Error::PageLimitExceeded { pages: 3, limit: 2 })
    ));

    let huge_page = inspect_document(
        &pdf(1, 10_000, 10_000),
        "application/pdf",
        DocumentLimits::new(2, 20_000_000, 40_000_000).unwrap(),
    );
    assert!(matches!(huge_page, Err(Error::PixelLimitExceeded { .. })));
}

#[test]
fn rejects_a_cyclic_page_parent_instead_of_defaulting_inherited_values() {
    let result = inspect_document(
        &pdf_with_cyclic_page_parent(),
        "application/pdf",
        DocumentLimits::new(2, 20_000_000, 40_000_000).unwrap(),
    );

    assert_eq!(result.unwrap_err(), Error::InvalidDocument);
}

#[test]
fn verifies_image_format_and_dimensions_without_decoding_pixels() {
    let limits = DocumentLimits::new(1, 100, 100).unwrap();
    let report = inspect_document(&png(), "image/png", limits).unwrap();
    assert_eq!(report.page_count, 1);
    assert_eq!(report.maximum_page_pixels, 6);
    assert_eq!(report.total_page_pixels, 6);
    assert_eq!(report.pages.len(), 1);
    assert_eq!(u32::from(report.pages[0].page), 1);
    assert_eq!((report.pages[0].width, report.pages[0].height), (2, 3));

    assert_eq!(
        inspect_document(&png(), "image/jpeg", limits).unwrap_err(),
        Error::InvalidDocument
    );
}

#[test]
fn rejects_compressed_pdf_streams_that_exceed_the_decoded_byte_limit() {
    let limits = DocumentLimits::new(1, 20_000_000, 20_000_000)
        .unwrap()
        .with_maximum_decoded_stream_bytes(1024)
        .unwrap();
    let result = inspect_document(
        &pdf_with_compressed_content(4096),
        "application/pdf",
        limits,
    );

    assert!(matches!(
        result,
        Err(Error::DecodedStreamLimitExceeded { limit: 1024 })
    ));
}
