//! Safe, bounded image preparation for the Rust OCR engine.

use std::io::Cursor;

use image::ImageReader;
use thiserror::Error;

const DEFAULT_MAX_ENCODED_BYTES: usize = 32 * 1024 * 1024;
const BLANK_CONTRAST_THRESHOLD: f64 = 0.02;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum Error {
    #[error("image limit must be greater than zero")]
    InvalidLimit,
    #[error("encoded image exceeds the configured byte limit")]
    EncodedByteLimitExceeded { bytes: usize, limit: usize },
    #[error("image exceeds the configured pixel limit")]
    PixelLimitExceeded { pixels: u64, limit: u64 },
    #[error("image content is unsupported or invalid")]
    UnsupportedOrInvalidImage,
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct ImageLimits {
    max_encoded_bytes: usize,
    max_pixels: u64,
}

impl ImageLimits {
    pub fn new(max_pixels: u64) -> Result<Self> {
        if max_pixels == 0 {
            Err(Error::InvalidLimit)
        } else {
            Ok(Self {
                max_encoded_bytes: DEFAULT_MAX_ENCODED_BYTES,
                max_pixels,
            })
        }
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum QualityWarning {
    Blank,
}

#[derive(Debug, Clone, PartialEq)]
pub struct QualityReport {
    pub width: u32,
    pub height: u32,
    pub contrast: f64,
    pub quality_score: f64,
    pub warnings: Vec<QualityWarning>,
}

pub fn inspect_image(encoded: &[u8], limits: ImageLimits) -> Result<QualityReport> {
    if encoded.len() > limits.max_encoded_bytes {
        return Err(Error::EncodedByteLimitExceeded {
            bytes: encoded.len(),
            limit: limits.max_encoded_bytes,
        });
    }

    let metadata_reader = reader(encoded)?;
    let (width, height) = metadata_reader
        .into_dimensions()
        .map_err(|_| Error::UnsupportedOrInvalidImage)?;
    let pixels = u64::from(width) * u64::from(height);
    if pixels > limits.max_pixels {
        return Err(Error::PixelLimitExceeded {
            pixels,
            limit: limits.max_pixels,
        });
    }

    let grayscale = reader(encoded)?
        .decode()
        .map_err(|_| Error::UnsupportedOrInvalidImage)?
        .into_luma8();
    let (minimum, maximum) = grayscale
        .pixels()
        .fold((u8::MAX, u8::MIN), |(minimum, maximum), pixel| {
            (minimum.min(pixel.0[0]), maximum.max(pixel.0[0]))
        });
    let contrast = f64::from(maximum - minimum) / 255.0;
    let blank = contrast < BLANK_CONTRAST_THRESHOLD;

    Ok(QualityReport {
        width,
        height,
        contrast,
        quality_score: if blank { 0.0 } else { contrast },
        warnings: if blank {
            vec![QualityWarning::Blank]
        } else {
            Vec::new()
        },
    })
}

fn reader(encoded: &[u8]) -> Result<ImageReader<Cursor<&[u8]>>> {
    ImageReader::new(Cursor::new(encoded))
        .with_guessed_format()
        .map_err(|_| Error::UnsupportedOrInvalidImage)
}
