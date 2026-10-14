use std::{
    io::{Read, Write},
    path::Path,
    process::{Command, Stdio},
};

use ocr_domain::{
    Confidence, DocumentPage, NormalizedPoint, ObservationLevel, Polygon, TextObservation,
};
use serde::Deserialize;

use crate::{inspect_document, DocumentLimits, Error, Result};

const MAXIMUM_XML_BYTES: usize = ocr_domain::MAXIMUM_DOCUMENT_RESULT_BYTES * 2;

#[derive(Deserialize)]
struct Html {
    body: Body,
}
#[derive(Deserialize)]
struct Body {
    doc: PdfText,
}
#[derive(Deserialize)]
struct PdfText {
    #[serde(rename = "page", default)]
    pages: Vec<PdfPage>,
}
#[derive(Deserialize)]
struct PdfPage {
    #[serde(rename = "@width")]
    width: f64,
    #[serde(rename = "@height")]
    height: f64,
    #[serde(rename = "flow", default)]
    flows: Vec<Flow>,
}
#[derive(Deserialize)]
struct Flow {
    #[serde(rename = "block", default)]
    blocks: Vec<Block>,
}
#[derive(Deserialize)]
struct Block {
    #[serde(rename = "line", default)]
    lines: Vec<Line>,
}
#[derive(Deserialize)]
struct Line {
    #[serde(rename = "@xMin")]
    left: f64,
    #[serde(rename = "@yMin")]
    top: f64,
    #[serde(rename = "@xMax")]
    right: f64,
    #[serde(rename = "@yMax")]
    bottom: f64,
    #[serde(rename = "word", default)]
    words: Vec<Word>,
}
#[derive(Deserialize)]
struct Word {
    #[serde(rename = "$text", default)]
    text: String,
}

pub fn extract_native_document(
    encoded: &[u8],
    content_type: &str,
    limits: DocumentLimits,
    executable: &Path,
) -> Result<Vec<DocumentPage>> {
    let report = inspect_document(encoded, content_type, limits)?;
    if content_type != "application/pdf" {
        return report
            .pages
            .into_iter()
            .map(|geometry| {
                DocumentPage::new(geometry.page, geometry.width, geometry.height, Vec::new())
                    .map_err(|_| Error::InvalidDocument)
            })
            .collect();
    }
    if !executable.is_absolute() {
        return Err(Error::NativeTextUnavailable);
    }
    let xml = pdf_layout(encoded, executable)?;
    let text: Html =
        quick_xml::de::from_reader(xml.as_slice()).map_err(|_| Error::InvalidDocument)?;
    if text.body.doc.pages.len() != report.page_count {
        return Err(Error::InvalidDocument);
    }
    text.body
        .doc
        .pages
        .into_iter()
        .zip(report.pages)
        .map(|(page, geometry)| {
            if !page.width.is_finite()
                || !page.height.is_finite()
                || page.width <= 0.0
                || page.height <= 0.0
            {
                return Err(Error::InvalidDocument);
            }
            // Poppler may report unrotated sizes while its boxes follow the display rotation.
            let (layout_width, layout_height) = if geometry.width != geometry.height
                && (geometry.width > geometry.height) != (page.width > page.height)
            {
                (page.height, page.width)
            } else {
                (page.width, page.height)
            };
            let mut observations = Vec::new();
            let mut usable = true;
            for line in page
                .flows
                .into_iter()
                .flat_map(|flow| flow.blocks)
                .flat_map(|block| block.lines)
            {
                let text = line
                    .words
                    .into_iter()
                    .map(|word| word.text)
                    .collect::<Vec<_>>()
                    .join(" ");
                if text.trim().is_empty() {
                    continue;
                }
                if text.len() > 65_536
                    || text
                        .chars()
                        .any(|c| c == '\u{fffd}' || (c.is_control() && !c.is_whitespace()))
                {
                    usable = false;
                    break;
                }
                let coordinates = [
                    line.left / layout_width,
                    line.top / layout_height,
                    line.right / layout_width,
                    line.bottom / layout_height,
                ];
                if coordinates.iter().any(|value| !value.is_finite()) {
                    return Err(Error::InvalidDocument);
                }
                let [left, top, right, bottom] = coordinates.map(|v| v.clamp(0.0, 1.0));
                if left >= right || top >= bottom {
                    continue;
                }
                let polygon = Polygon::new(
                    vec![
                        NormalizedPoint::new(left, top),
                        NormalizedPoint::new(right, top),
                        NormalizedPoint::new(right, bottom),
                        NormalizedPoint::new(left, bottom),
                    ]
                    .into_iter()
                    .collect::<std::result::Result<Vec<_>, _>>()
                    .map_err(|_| Error::InvalidDocument)?,
                )
                .map_err(|_| Error::InvalidDocument)?;
                let order = u32::try_from(observations.len())
                    .map_err(|_| Error::NativeTextLimitExceeded)?;
                // This value denotes direct source fidelity, not a model probability.
                let observation = TextObservation::new(
                    format!("obs_native_p{}_line{}", u32::from(geometry.page), order)
                        .try_into()
                        .map_err(|_| Error::InvalidDocument)?,
                    ObservationLevel::Line,
                    text,
                    Confidence::new(1.0).map_err(|_| Error::InvalidDocument)?,
                    polygon,
                    order,
                    None,
                )
                .map_err(|_| Error::InvalidDocument)?;
                observations.push(observation);
            }
            if !usable {
                observations.clear();
            }
            DocumentPage::new(geometry.page, geometry.width, geometry.height, observations)
                .map_err(|_| Error::InvalidDocument)
        })
        .collect()
}

fn pdf_layout(encoded: &[u8], executable: &Path) -> Result<Vec<u8>> {
    let mut child = Command::new(executable)
        .env_clear()
        .args(["-bbox-layout", "-enc", "UTF-8", "-", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| Error::NativeTextUnavailable)?;
    let mut stdin = child.stdin.take().ok_or(Error::NativeTextUnavailable)?;
    let stdout = child.stdout.take().ok_or(Error::NativeTextUnavailable)?;
    std::thread::scope(|scope| {
        let writer = scope.spawn(move || stdin.write_all(encoded));
        let mut output = Vec::new();
        let read = stdout
            .take((MAXIMUM_XML_BYTES + 1) as u64)
            .read_to_end(&mut output);
        if read.is_err() || output.len() > MAXIMUM_XML_BYTES {
            let _ = child.kill();
            let _ = child.wait();
            let _ = writer.join();
            return Err(Error::NativeTextLimitExceeded);
        }
        let write = writer.join().map_err(|_| Error::NativeTextUnavailable)?;
        let status = child.wait().map_err(|_| Error::NativeTextUnavailable)?;
        if write.is_err() || !status.success() {
            return Err(Error::InvalidDocument);
        }
        Ok(output)
    })
}
