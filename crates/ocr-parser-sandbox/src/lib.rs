//! Bounded structural inspection for the disposable document parser sandbox.

use std::{collections::HashSet, io::Cursor};

use image::{ImageFormat, ImageReader};
use lopdf::{DecompressError, Document, LoadOptions, Object, ObjectId};
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const MAXIMUM_ENCODED_BYTES: usize = 100 * 1024 * 1024;
const HARD_MAXIMUM_PAGES: usize = 300;
const HARD_MAXIMUM_PAGE_PIXELS: u64 = 100_000_000;
const HARD_MAXIMUM_TOTAL_PIXELS: u64 = 1_000_000_000;
const HARD_MAXIMUM_DECODED_STREAM_BYTES: usize = 64 * 1024 * 1024;
const MAXIMUM_DECOMPRESSION_RATIO: usize = 200;
const MAXIMUM_PDF_OBJECTS: usize = 500_000;
const MAXIMUM_PAGE_TREE_DEPTH: usize = 64;
const RENDER_DPI: f64 = 300.0;
const PDF_POINTS_PER_INCH: f64 = 72.0;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum Error {
    #[error("document limits are invalid")]
    InvalidLimits,
    #[error("document content type is unsupported")]
    UnsupportedContentType,
    #[error("document content is invalid")]
    InvalidDocument,
    #[error("document is password protected")]
    PasswordProtected,
    #[error("encoded document exceeds its byte limit")]
    EncodedByteLimitExceeded { bytes: usize, limit: usize },
    #[error("document exceeds its page limit")]
    PageLimitExceeded { pages: usize, limit: usize },
    #[error("document exceeds its pixel limit")]
    PixelLimitExceeded { pixels: u64, limit: u64 },
    #[error("document exceeds its object limit")]
    ObjectLimitExceeded { objects: usize, limit: usize },
    #[error("decoded PDF stream exceeds its byte limit")]
    DecodedStreamLimitExceeded { limit: usize },
    #[error("PDF stream exceeds its decompression ratio limit")]
    DecompressionRatioExceeded {
        compressed: usize,
        decompressed: usize,
        limit: usize,
    },
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct DocumentLimits {
    maximum_pages: usize,
    maximum_page_pixels: u64,
    maximum_total_pixels: u64,
    maximum_decoded_stream_bytes: usize,
}

impl DocumentLimits {
    pub fn new(
        maximum_pages: usize,
        maximum_page_pixels: u64,
        maximum_total_pixels: u64,
    ) -> Result<Self> {
        if maximum_pages == 0
            || maximum_pages > HARD_MAXIMUM_PAGES
            || maximum_page_pixels == 0
            || maximum_page_pixels > HARD_MAXIMUM_PAGE_PIXELS
            || maximum_total_pixels < maximum_page_pixels
            || maximum_total_pixels > HARD_MAXIMUM_TOTAL_PIXELS
        {
            return Err(Error::InvalidLimits);
        }
        Ok(Self {
            maximum_pages,
            maximum_page_pixels,
            maximum_total_pixels,
            maximum_decoded_stream_bytes: HARD_MAXIMUM_DECODED_STREAM_BYTES,
        })
    }

    pub fn with_maximum_decoded_stream_bytes(mut self, maximum_bytes: usize) -> Result<Self> {
        if maximum_bytes == 0 || maximum_bytes > HARD_MAXIMUM_DECODED_STREAM_BYTES {
            return Err(Error::InvalidLimits);
        }
        self.maximum_decoded_stream_bytes = maximum_bytes;
        Ok(self)
    }
}

#[derive(Debug, Copy, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct InspectionReport {
    pub page_count: usize,
    pub maximum_page_pixels: u64,
    pub total_page_pixels: u64,
    pub password_protected: bool,
}

pub fn inspect_document(
    encoded: &[u8],
    content_type: &str,
    limits: DocumentLimits,
) -> Result<InspectionReport> {
    if encoded.is_empty() {
        return Err(Error::InvalidDocument);
    }
    if encoded.len() > MAXIMUM_ENCODED_BYTES {
        return Err(Error::EncodedByteLimitExceeded {
            bytes: encoded.len(),
            limit: MAXIMUM_ENCODED_BYTES,
        });
    }
    match content_type {
        "application/pdf" => inspect_pdf(encoded, limits),
        "image/jpeg" => inspect_image(encoded, ImageFormat::Jpeg, limits),
        "image/png" => inspect_image(encoded, ImageFormat::Png, limits),
        "image/tiff" => inspect_image(encoded, ImageFormat::Tiff, limits),
        "image/webp" => inspect_image(encoded, ImageFormat::WebP, limits),
        _ => Err(Error::UnsupportedContentType),
    }
}

fn inspect_pdf(encoded: &[u8], limits: DocumentLimits) -> Result<InspectionReport> {
    if !encoded.starts_with(b"%PDF-") {
        return Err(Error::InvalidDocument);
    }
    let load_options = LoadOptions {
        max_decompressed_size: Some(limits.maximum_decoded_stream_bytes),
        ..LoadOptions::default()
    };
    let document = Document::load_mem_with_options(encoded, load_options).map_err(|error| {
        if matches!(
            error,
            lopdf::Error::Decompress(DecompressError::MemoryLimitExceeded { .. })
        ) {
            Error::DecodedStreamLimitExceeded {
                limit: limits.maximum_decoded_stream_bytes,
            }
        } else {
            Error::InvalidDocument
        }
    })?;
    if document.trailer.get(b"Encrypt").is_ok() {
        return Err(Error::PasswordProtected);
    }
    if document.objects.len() > MAXIMUM_PDF_OBJECTS {
        return Err(Error::ObjectLimitExceeded {
            objects: document.objects.len(),
            limit: MAXIMUM_PDF_OBJECTS,
        });
    }
    validate_pdf_streams(&document, limits.maximum_decoded_stream_bytes)?;
    let pages = document.get_pages();
    if pages.is_empty() {
        return Err(Error::InvalidDocument);
    }
    if pages.len() > limits.maximum_pages {
        return Err(Error::PageLimitExceeded {
            pages: pages.len(),
            limit: limits.maximum_pages,
        });
    }
    let mut maximum_page_pixels = 0_u64;
    let mut total_page_pixels = 0_u64;
    for page_id in pages.values() {
        let pixels = page_pixels(&document, *page_id)?;
        if pixels > limits.maximum_page_pixels {
            return Err(Error::PixelLimitExceeded {
                pixels,
                limit: limits.maximum_page_pixels,
            });
        }
        total_page_pixels =
            total_page_pixels
                .checked_add(pixels)
                .ok_or(Error::PixelLimitExceeded {
                    pixels: u64::MAX,
                    limit: limits.maximum_total_pixels,
                })?;
        if total_page_pixels > limits.maximum_total_pixels {
            return Err(Error::PixelLimitExceeded {
                pixels: total_page_pixels,
                limit: limits.maximum_total_pixels,
            });
        }
        maximum_page_pixels = maximum_page_pixels.max(pixels);
    }
    Ok(InspectionReport {
        page_count: pages.len(),
        maximum_page_pixels,
        total_page_pixels,
        password_protected: false,
    })
}

fn validate_pdf_streams(document: &Document, maximum_decoded_bytes: usize) -> Result<()> {
    for object in document.objects.values() {
        let Object::Stream(stream) = object else {
            continue;
        };
        let filters = match stream.filters() {
            Ok(filters) => filters,
            Err(_) => {
                if stream.content.len() > maximum_decoded_bytes {
                    return Err(Error::DecodedStreamLimitExceeded {
                        limit: maximum_decoded_bytes,
                    });
                }
                continue;
            }
        };
        if filters
            .iter()
            .any(|filter| !matches!(*filter, b"FlateDecode" | b"LZWDecode" | b"ASCII85Decode"))
        {
            continue;
        }
        let decompressed = stream
            .decompressed_content_with_limit(maximum_decoded_bytes)
            .map_err(|error| {
                if matches!(
                    error,
                    lopdf::Error::Decompress(DecompressError::MemoryLimitExceeded { .. })
                ) {
                    Error::DecodedStreamLimitExceeded {
                        limit: maximum_decoded_bytes,
                    }
                } else {
                    Error::InvalidDocument
                }
            })?;
        if !stream.content.is_empty()
            && decompressed.len()
                > stream
                    .content
                    .len()
                    .saturating_mul(MAXIMUM_DECOMPRESSION_RATIO)
        {
            return Err(Error::DecompressionRatioExceeded {
                compressed: stream.content.len(),
                decompressed: decompressed.len(),
                limit: MAXIMUM_DECOMPRESSION_RATIO,
            });
        }
    }
    Ok(())
}

fn inspect_image(
    encoded: &[u8],
    expected_format: ImageFormat,
    limits: DocumentLimits,
) -> Result<InspectionReport> {
    let reader = ImageReader::new(Cursor::new(encoded))
        .with_guessed_format()
        .map_err(|_| Error::InvalidDocument)?;
    if reader.format() != Some(expected_format) {
        return Err(Error::InvalidDocument);
    }
    let (width, height) = reader
        .into_dimensions()
        .map_err(|_| Error::InvalidDocument)?;
    let pixels =
        u64::from(width)
            .checked_mul(u64::from(height))
            .ok_or(Error::PixelLimitExceeded {
                pixels: u64::MAX,
                limit: limits.maximum_page_pixels,
            })?;
    if pixels == 0 {
        return Err(Error::InvalidDocument);
    }
    if pixels > limits.maximum_page_pixels {
        return Err(Error::PixelLimitExceeded {
            pixels,
            limit: limits.maximum_page_pixels,
        });
    }
    Ok(InspectionReport {
        page_count: 1,
        maximum_page_pixels: pixels,
        total_page_pixels: pixels,
        password_protected: false,
    })
}

fn page_pixels(document: &Document, page_id: ObjectId) -> Result<u64> {
    let media_box = inherited_value(document, page_id, b"MediaBox")?
        .as_array()
        .map_err(|_| Error::InvalidDocument)?;
    if media_box.len() != 4 {
        return Err(Error::InvalidDocument);
    }
    let coordinates = media_box
        .iter()
        .map(|value| {
            value
                .as_float()
                .map(f64::from)
                .map_err(|_| Error::InvalidDocument)
        })
        .collect::<Result<Vec<_>>>()?;
    let user_unit = inherited_optional_number(document, page_id, b"UserUnit")?.unwrap_or(1.0);
    let width_points = (coordinates[2] - coordinates[0]).abs() * user_unit;
    let height_points = (coordinates[3] - coordinates[1]).abs() * user_unit;
    if !width_points.is_finite()
        || !height_points.is_finite()
        || width_points <= 0.0
        || height_points <= 0.0
    {
        return Err(Error::InvalidDocument);
    }
    let width_pixels = (width_points * RENDER_DPI / PDF_POINTS_PER_INCH).ceil();
    let height_pixels = (height_points * RENDER_DPI / PDF_POINTS_PER_INCH).ceil();
    if width_pixels > u64::MAX as f64 || height_pixels > u64::MAX as f64 {
        return Err(Error::PixelLimitExceeded {
            pixels: u64::MAX,
            limit: u64::MAX,
        });
    }
    (width_pixels as u64)
        .checked_mul(height_pixels as u64)
        .ok_or(Error::PixelLimitExceeded {
            pixels: u64::MAX,
            limit: u64::MAX,
        })
}

fn inherited_optional_number(
    document: &Document,
    page_id: ObjectId,
    key: &[u8],
) -> Result<Option<f64>> {
    match inherited_optional_value(document, page_id, key)? {
        Some(value) => value
            .as_float()
            .map(f64::from)
            .map(Some)
            .map_err(|_| Error::InvalidDocument),
        None => Ok(None),
    }
}

fn inherited_value<'a>(
    document: &'a Document,
    page_id: ObjectId,
    key: &[u8],
) -> Result<&'a Object> {
    inherited_optional_value(document, page_id, key)?.ok_or(Error::InvalidDocument)
}

fn inherited_optional_value<'a>(
    document: &'a Document,
    page_id: ObjectId,
    key: &[u8],
) -> Result<Option<&'a Object>> {
    let mut current = page_id;
    let mut visited = HashSet::new();
    for _ in 0..MAXIMUM_PAGE_TREE_DEPTH {
        if !visited.insert(current) {
            return Err(Error::InvalidDocument);
        }
        let dictionary = document
            .get_dictionary(current)
            .map_err(|_| Error::InvalidDocument)?;
        if let Ok(value) = dictionary.get(key) {
            return Ok(Some(value));
        }
        if !dictionary.has(b"Parent") {
            return Ok(None);
        }
        current = dictionary
            .get(b"Parent")
            .and_then(Object::as_reference)
            .map_err(|_| Error::InvalidDocument)?;
    }
    Err(Error::InvalidDocument)
}
