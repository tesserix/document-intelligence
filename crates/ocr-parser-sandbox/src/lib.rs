//! Bounded structural inspection for the disposable document parser sandbox.

mod native_text;
pub use native_text::extract_native_document;

use std::{collections::HashSet, io::Cursor};

use image::{ImageFormat, ImageReader};
use lopdf::{DecompressError, Document, LoadOptions, Object, ObjectId};
use ocr_domain::{PageGeometry, PageNumber, MAXIMUM_PAGE_COUNT, MAXIMUM_TOTAL_PAGE_PIXELS};
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const MAXIMUM_ENCODED_BYTES: usize = 100 * 1024 * 1024;
const HARD_MAXIMUM_PAGES: usize = MAXIMUM_PAGE_COUNT as usize;
const HARD_MAXIMUM_PAGE_PIXELS: u64 = 100_000_000;
const HARD_MAXIMUM_TOTAL_PIXELS: u64 = MAXIMUM_TOTAL_PAGE_PIXELS;
const HARD_MAXIMUM_DECODED_STREAM_BYTES: usize = 64 * 1024 * 1024;
const MAXIMUM_DECOMPRESSION_RATIO: usize = 200;
const MAXIMUM_PDF_OBJECTS: usize = 500_000;
const MAXIMUM_PAGE_TREE_DEPTH: usize = 64;
const MAXIMUM_PNG_METADATA_BYTES: usize = 1024 * 1024;
const RENDER_DPI: f64 = 300.0;
const PDF_POINTS_PER_INCH: f64 = 72.0;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum Error {
    #[error("native text extraction is unavailable")]
    NativeTextUnavailable,
    #[error("native text exceeds its output limit")]
    NativeTextLimitExceeded,
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct InspectionReport {
    pub page_count: usize,
    pub maximum_page_pixels: u64,
    pub total_page_pixels: u64,
    pub pages: Vec<PageGeometry>,
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
        "image/tiff" => inspect_tiff(encoded, limits),
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
    let mut page_geometries = Vec::with_capacity(pages.len());
    for (page, page_id) in pages {
        let geometry = page_geometry(&document, page_id, page)?;
        let pixels = geometry.pixels();
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
        page_geometries.push(geometry);
    }
    Ok(InspectionReport {
        page_count: page_geometries.len(),
        maximum_page_pixels,
        total_page_pixels,
        pages: page_geometries,
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

fn png_text_field(data: &[u8]) -> Result<&[u8]> {
    let end = data
        .iter()
        .position(|byte| *byte == 0)
        .ok_or(Error::InvalidDocument)?;
    Ok(&data[end + 1..])
}

fn validate_png_metadata(encoded: &[u8]) -> Result<()> {
    let mut chunks = encoded.get(8..).ok_or(Error::InvalidDocument)?;
    let mut remaining = MAXIMUM_PNG_METADATA_BYTES;
    while !chunks.is_empty() {
        let header = chunks.get(..8).ok_or(Error::InvalidDocument)?;
        let length = u32::from_be_bytes(header[..4].try_into().map_err(|_| Error::InvalidDocument)?)
            as usize;
        let end = length.checked_add(12).ok_or(Error::InvalidDocument)?;
        let chunk = chunks.get(..end).ok_or(Error::InvalidDocument)?;
        let kind = &header[4..];
        let data = &chunk[8..end - 4];
        if kind[0].is_ascii_lowercase() && kind != b"fdAT" {
            remaining = remaining
                .checked_sub(data.len())
                .ok_or(Error::InvalidDocument)?;
        }
        let compressed = match kind {
            b"iCCP" | b"zTXt" => {
                let value = png_text_field(data)?;
                if value.first() != Some(&0) {
                    return Err(Error::InvalidDocument);
                }
                Some(&value[1..])
            }
            b"iTXt" => {
                let value = png_text_field(data)?;
                let flags = value.get(..2).ok_or(Error::InvalidDocument)?;
                if flags[0] > 1 || flags[1] != 0 {
                    return Err(Error::InvalidDocument);
                }
                let text = png_text_field(png_text_field(&value[2..])?)?;
                (flags[0] == 1).then_some(text)
            }
            _ => None,
        };
        if let Some(compressed) = compressed {
            let decoded = fdeflate::decompress_to_vec_bounded(compressed, remaining)
                .map_err(|_| Error::InvalidDocument)?;
            remaining = remaining
                .checked_sub(decoded.len())
                .ok_or(Error::InvalidDocument)?;
        }
        chunks = &chunks[end..];
        if kind == b"IEND" {
            return Ok(());
        }
    }
    Err(Error::InvalidDocument)
}

fn inspect_image(
    encoded: &[u8],
    expected_format: ImageFormat,
    limits: DocumentLimits,
) -> Result<InspectionReport> {
    let mut decode_limits = image::Limits::default();
    decode_limits.max_alloc = Some(64 * 1024 * 1024);
    let mut reader = ImageReader::new(Cursor::new(encoded))
        .with_guessed_format()
        .map_err(|_| Error::InvalidDocument)?;
    if reader.format() != Some(expected_format) {
        return Err(Error::InvalidDocument);
    }
    if expected_format == ImageFormat::Png {
        validate_png_metadata(encoded)?;
    }
    reader.limits(decode_limits.clone());
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
    decode_limits.max_image_width = Some(width);
    decode_limits.max_image_height = Some(height);
    let mut decoder = ImageReader::with_format(Cursor::new(encoded), expected_format);
    decoder.limits(decode_limits);
    decoder.decode().map_err(|_| Error::InvalidDocument)?;
    Ok(InspectionReport {
        page_count: 1,
        maximum_page_pixels: pixels,
        total_page_pixels: pixels,
        pages: vec![PageGeometry::new(
            PageNumber::new(1).map_err(|_| Error::InvalidDocument)?,
            width,
            height,
        )
        .map_err(|_| Error::InvalidDocument)?],
        password_protected: false,
    })
}

fn page_geometry(document: &Document, page_id: ObjectId, page: u32) -> Result<PageGeometry> {
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
    if width_pixels > u32::MAX as f64 || height_pixels > u32::MAX as f64 {
        return Err(Error::PixelLimitExceeded {
            pixels: u64::MAX,
            limit: u64::MAX,
        });
    }
    let rotation = inherited_optional_number(document, page_id, b"Rotate")?.unwrap_or(0.0);
    if !rotation.is_finite() || rotation.rem_euclid(90.0) != 0.0 {
        return Err(Error::InvalidDocument);
    }
    let (width, height) = if rotation.rem_euclid(180.0) == 90.0 {
        (height_pixels as u32, width_pixels as u32)
    } else {
        (width_pixels as u32, height_pixels as u32)
    };
    let pixels =
        u64::from(width)
            .checked_mul(u64::from(height))
            .ok_or(Error::PixelLimitExceeded {
                pixels: u64::MAX,
                limit: HARD_MAXIMUM_PAGE_PIXELS,
            })?;
    if pixels > HARD_MAXIMUM_PAGE_PIXELS {
        return Err(Error::PixelLimitExceeded {
            pixels,
            limit: HARD_MAXIMUM_PAGE_PIXELS,
        });
    }
    PageGeometry::new(
        PageNumber::new(page).map_err(|_| Error::InvalidDocument)?,
        width,
        height,
    )
    .map_err(|_| Error::InvalidDocument)
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

/// Upscale small, single-frame images; coordinates remain normalized to the same content.
pub fn prepare_ocr_image(encoded: &[u8], content_type: &str) -> Result<Option<Vec<u8>>> {
    if !matches!(content_type, "image/png" | "image/jpeg" | "image/webp") {
        return Ok(None);
    }
    if encoded.len() > 20 * 1024 * 1024 {
        return Err(Error::InvalidDocument);
    }
    let format = match content_type {
        "image/png" => ImageFormat::Png,
        "image/jpeg" => ImageFormat::Jpeg,
        _ => ImageFormat::WebP,
    };
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(64 * 1024 * 1024);
    let make_reader = || {
        let mut reader = ImageReader::with_format(Cursor::new(encoded), format);
        reader.limits(limits.clone());
        reader
    };
    let (width, height) = make_reader()
        .into_dimensions()
        .map_err(|_| Error::InvalidDocument)?;
    if width.min(height) < 64 || width.max(height) > 640 {
        return Ok(None);
    }
    let animated = match format {
        ImageFormat::Png => {
            image::codecs::png::PngDecoder::with_limits(Cursor::new(encoded), limits.clone())
                .and_then(|decoder| decoder.is_apng())
                .map_err(|_| Error::InvalidDocument)?
        }
        ImageFormat::WebP => image::codecs::webp::WebPDecoder::new(Cursor::new(encoded))
            .map_err(|_| Error::InvalidDocument)?
            .has_animation(),
        _ => false,
    };
    if animated {
        return Ok(None);
    }
    use image::ImageDecoder as _;
    let mut decoder = make_reader()
        .into_decoder()
        .map_err(|_| Error::InvalidDocument)?;
    if decoder.orientation().map_err(|_| Error::InvalidDocument)?
        != image::metadata::Orientation::NoTransforms
    {
        return Ok(None);
    }
    let image = image::DynamicImage::from_decoder(decoder).map_err(|_| Error::InvalidDocument)?;
    let enlarged = image.resize_exact(width * 3, height * 3, image::imageops::FilterType::Lanczos3);
    let mut output = Cursor::new(Vec::new());
    enlarged
        .write_to(&mut output, ImageFormat::Png)
        .map_err(|_| Error::InvalidDocument)?;
    Ok(Some(output.into_inner()))
}

fn inspect_tiff(encoded: &[u8], limits: DocumentLimits) -> Result<InspectionReport> {
    let mut decoder =
        tiff::decoder::Decoder::new(Cursor::new(encoded)).map_err(|_| Error::InvalidDocument)?;
    let mut decode_limits = tiff::decoder::Limits::default();
    decode_limits.decoding_buffer_size = HARD_MAXIMUM_DECODED_STREAM_BYTES;
    decode_limits.intermediate_buffer_size = HARD_MAXIMUM_DECODED_STREAM_BYTES;
    decoder = decoder.with_limits(decode_limits);
    let mut pages = Vec::new();
    let mut total = 0_u64;
    let mut maximum = 0_u64;
    loop {
        if pages.len() >= limits.maximum_pages {
            return Err(Error::PageLimitExceeded {
                pages: pages.len() + 1,
                limit: limits.maximum_pages,
            });
        }
        let (width, height) = decoder.dimensions().map_err(|_| Error::InvalidDocument)?;
        let pixels = u64::from(width)
            .checked_mul(u64::from(height))
            .filter(|n| *n > 0 && *n <= limits.maximum_page_pixels)
            .ok_or(Error::PixelLimitExceeded {
                pixels: u64::MAX,
                limit: limits.maximum_total_pixels,
            })?;
        total = total
            .checked_add(pixels)
            .filter(|n| *n <= limits.maximum_total_pixels)
            .ok_or(Error::PixelLimitExceeded {
                pixels: u64::MAX,
                limit: limits.maximum_total_pixels,
            })?;
        maximum = maximum.max(pixels);
        decoder.read_image().map_err(|error| match error {
            tiff::TiffError::LimitsExceeded => Error::PixelLimitExceeded {
                pixels,
                limit: limits.maximum_page_pixels,
            },
            _ => Error::InvalidDocument,
        })?;
        pages.push(
            PageGeometry::new(
                PageNumber::new(pages.len() as u32 + 1).map_err(|_| Error::InvalidDocument)?,
                width,
                height,
            )
            .map_err(|_| Error::InvalidDocument)?,
        );
        if !decoder.more_images() {
            break;
        }
        decoder.next_image().map_err(|_| Error::InvalidDocument)?;
    }
    Ok(InspectionReport {
        page_count: pages.len(),
        maximum_page_pixels: maximum,
        total_page_pixels: total,
        pages,
        password_protected: false,
    })
}
