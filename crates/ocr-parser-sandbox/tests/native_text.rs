use std::path::Path;

use lopdf::{
    content::{Content, Operation},
    dictionary, Document, Object, Stream,
};
use ocr_parser_sandbox::{extract_native_document, DocumentLimits};

fn pdf_text(pages: &[&str]) -> Vec<u8> {
    let mut document = Document::with_version("1.7");
    let pages_id = document.new_object_id();
    let font_id = document.add_object(
        dictionary! {"Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica"},
    );
    let resources = document.add_object(dictionary! {"Font" => dictionary! {"F1" => font_id}});
    let mut children = Vec::new();
    for text in pages {
        let content = Content {
            operations: vec![
                Operation::new("BT", vec![]),
                Operation::new("Tf", vec!["F1".into(), 12.into()]),
                Operation::new("Td", vec![72.into(), 720.into()]),
                Operation::new("Tj", vec![Object::string_literal(*text)]),
                Operation::new("ET", vec![]),
            ],
        };
        let content_id =
            document.add_object(Stream::new(dictionary! {}, content.encode().unwrap()));
        let page = document.add_object(dictionary! {"Type"=>"Page", "Parent"=>pages_id, "MediaBox"=>vec![0.into(),0.into(),612.into(),792.into()], "Resources"=>resources, "Contents"=>content_id});
        children.push(Object::Reference(page));
    }
    document.objects.insert(
        pages_id,
        Object::Dictionary(
            dictionary! {"Type"=>"Pages", "Kids"=>children, "Count"=>pages.len() as i64},
        ),
    );
    let catalog = document.add_object(dictionary! {"Type"=>"Catalog", "Pages"=>pages_id});
    document.trailer.set("Root", catalog);
    let mut bytes = Vec::new();
    document.save_to(&mut bytes).unwrap();
    bytes
}

fn poppler() -> &'static Path {
    if cfg!(target_os = "macos") {
        Path::new("/opt/homebrew/bin/pdftotext")
    } else {
        Path::new("/usr/bin/pdftotext")
    }
}

#[test]
fn extracts_real_pdf_text_with_source_bounding_boxes_and_page_order() {
    let pages = extract_native_document(
        &pdf_text(&["Invoice & total 18.90", "Second page"]),
        "application/pdf",
        DocumentLimits::new(10, 100_000_000, 1_000_000_000).unwrap(),
        poppler(),
    )
    .unwrap();
    assert_eq!(pages.len(), 2);
    assert_eq!(pages[0].observations[0].text, "Invoice & total 18.90");
    assert_eq!(pages[1].observations[0].text, "Second page");
    assert_eq!(u32::from(pages[1].page), 2);
    let points = &pages[0].observations[0].polygon.points;
    assert!(points[0].x > 0.1 && points[0].x < 0.13);
    assert!(points[0].y > 0.07 && points[0].y < 0.11);
    assert!(points[2].x > points[0].x && points[2].y > points[0].y);
}

#[test]
fn native_cli_reads_stdin_and_returns_citable_pages() {
    use std::{
        io::Write,
        process::{Command, Stdio},
    };
    let mut child = Command::new(env!("CARGO_BIN_EXE_ocr-parser-sandbox"))
        .args([
            "extract-native",
            "--content-type",
            "application/pdf",
            "--max-pages",
            "10",
            "--max-page-pixels",
            "100000000",
            "--max-total-pixels",
            "1000000000",
            "--pdf-text-executable",
        ])
        .arg(poppler())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(&pdf_text(&["Native CLI"]))
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "native CLI exited {:?}",
        output.status.code()
    );
    let pages: Vec<ocr_domain::DocumentPage> = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(pages[0].observations[0].text, "Native CLI");
}

#[test]
fn extracts_one_thousand_pages_without_a_raster_budget_rejection() {
    let text = vec!["A long document"; 1000];
    let encoded = pdf_text(&text);
    let limits = DocumentLimits::new(1000, 100_000_000, 100_000_000_000).unwrap();
    let pages = extract_native_document(&encoded, "application/pdf", limits, poppler()).unwrap();
    assert_eq!(pages.len(), 1000);
    assert_eq!(u32::from(pages.last().unwrap().page), 1000);
    assert_eq!(
        pages.last().unwrap().observations[0].text,
        "A long document"
    );
}

#[test]
fn multipage_tiff_preserves_every_page_for_manual_review_without_a_text_engine() {
    use std::io::Cursor;
    use tiff::encoder::{colortype, TiffEncoder};
    let mut buffer = Cursor::new(Vec::new());
    {
        let mut encoder = TiffEncoder::new(&mut buffer).unwrap();
        encoder
            .write_image::<colortype::Gray8>(2, 3, &[255; 6])
            .unwrap();
        encoder
            .write_image::<colortype::Gray8>(3, 4, &[255; 12])
            .unwrap();
    }
    let pages = extract_native_document(
        &buffer.into_inner(),
        "image/tiff",
        DocumentLimits::new(10, 100_000_000, 1_000_000_000).unwrap(),
        Path::new("/not-installed/pdftotext"),
    )
    .unwrap();
    assert_eq!(pages.len(), 2);
    assert_eq!((pages[1].width, pages[1].height), (3, 4));
    assert!(pages.iter().all(|page| page.observations.is_empty()));
}

#[test]
fn rotated_pdf_geometry_matches_the_displayed_page() {
    let mut document = Document::load_mem(&pdf_text(&["Rotated invoice"])).unwrap();
    let page = document.get_pages()[&1];
    document
        .get_object_mut(page)
        .unwrap()
        .as_dict_mut()
        .unwrap()
        .set("Rotate", 90);
    let mut encoded = Vec::new();
    document.save_to(&mut encoded).unwrap();
    let pages = extract_native_document(
        &encoded,
        "application/pdf",
        DocumentLimits::new(10, 100_000_000, 1_000_000_000).unwrap(),
        poppler(),
    )
    .unwrap();
    assert_eq!((pages[0].width, pages[0].height), (3300, 2550));
    assert_eq!(pages[0].observations[0].text, "Rotated invoice");
}

#[test]
fn dense_thousand_page_documents_preserve_all_readable_lines() {
    let mut document = Document::load_mem(&pdf_text(&vec![""; 1000])).unwrap();
    let row = "Invoice reference 1048. Confirm the source document before processing any payment.";
    for (_, id) in document.get_pages() {
        let contents = document
            .get_object(id)
            .unwrap()
            .as_dict()
            .unwrap()
            .get(b"Contents")
            .unwrap()
            .as_reference()
            .unwrap();
        let mut operations = vec![
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec!["F1".into(), 8.into()]),
            Operation::new("TL", vec![10.into()]),
            Operation::new("Td", vec![72.into(), 740.into()]),
        ];
        for _ in 0..60 {
            operations.push(Operation::new("Tj", vec![Object::string_literal(row)]));
            operations.push(Operation::new("T*", vec![]));
        }
        operations.push(Operation::new("ET", vec![]));
        document
            .get_object_mut(contents)
            .unwrap()
            .as_stream_mut()
            .unwrap()
            .set_content(Content { operations }.encode().unwrap());
    }
    let mut bytes = Vec::new();
    document.save_to(&mut bytes).unwrap();
    let pages = extract_native_document(
        &bytes,
        "application/pdf",
        DocumentLimits::new(1000, 100_000_000, 100_000_000_000).unwrap(),
        poppler(),
    )
    .unwrap();
    assert_eq!(pages.len(), 1000);
    assert_eq!(
        pages
            .iter()
            .map(|page| page.observations.len())
            .sum::<usize>(),
        60_000
    );
    assert_eq!(pages[999].observations[59].text, row);
}
