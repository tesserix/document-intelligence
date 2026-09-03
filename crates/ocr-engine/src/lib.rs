//! Safe, bounded image preparation for the Rust OCR engine.

mod detection;
mod geometry;
mod page;
mod recognition;

pub use detection::{select_regions, DetectionCandidate, DetectionLimits};
pub use geometry::{CropRegion, Rotation, TransformChain, TransformStep};
pub use page::build_page_observations;
pub use recognition::{assemble_recognitions, RecognitionAlternative, RecognizedRegion};

use std::io::Cursor;

use image::{metadata::Orientation, GrayImage, ImageDecoder, ImageReader};
use thiserror::Error;

const DEFAULT_MAX_ENCODED_BYTES: usize = 32 * 1024 * 1024;
const BLANK_CONTRAST_THRESHOLD: f64 = 0.02;
const UNDEREXPOSED_LUMINANCE_THRESHOLD: f64 = 0.05;
const OVEREXPOSED_LUMINANCE_THRESHOLD: f64 = 0.95;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum Error {
    #[error("image limit must be greater than zero")]
    InvalidLimit,
    #[error("quality threshold must be greater than zero")]
    InvalidQualityThreshold,
    #[error("sharpness threshold must be finite and between zero and one")]
    InvalidSharpnessThreshold,
    #[error("encoded image exceeds the configured byte limit")]
    EncodedByteLimitExceeded { bytes: usize, limit: usize },
    #[error("image exceeds the configured pixel limit")]
    PixelLimitExceeded { pixels: u64, limit: u64 },
    #[error("image content is unsupported or invalid")]
    UnsupportedOrInvalidImage,
    #[error("image geometry is invalid")]
    InvalidGeometry,
    #[error("detector candidate is invalid")]
    InvalidDetectionCandidate,
    #[error("detector limits are invalid")]
    InvalidDetectionLimits,
    #[error("detector output exceeds the configured candidate limit")]
    DetectionOutputLimitExceeded { candidates: usize, limit: usize },
    #[error("recognizer output is invalid")]
    InvalidRecognition,
    #[error("recognizer batch does not exactly cover detector regions")]
    InvalidRecognitionBatch,
    #[error("page observation output is invalid")]
    InvalidPageObservation,
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
    Blurry,
    LowResolution,
    Overexposed,
    Underexposed,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum QualityDisposition {
    Continue,
    RequestBetterSource,
}

#[derive(Debug, Copy, Clone, PartialEq)]
pub struct QualityThresholds {
    minimum_width: u32,
    minimum_height: u32,
    minimum_sharpness: f64,
}

impl QualityThresholds {
    pub fn new(minimum_width: u32, minimum_height: u32) -> Result<Self> {
        if minimum_width == 0 || minimum_height == 0 {
            return Err(Error::InvalidQualityThreshold);
        }
        Ok(Self {
            minimum_width,
            minimum_height,
            minimum_sharpness: 0.0,
        })
    }

    pub fn with_minimum_sharpness(mut self, minimum_sharpness: f64) -> Result<Self> {
        if !minimum_sharpness.is_finite() || !(0.0..=1.0).contains(&minimum_sharpness) {
            return Err(Error::InvalidSharpnessThreshold);
        }
        self.minimum_sharpness = minimum_sharpness;
        Ok(self)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct QualityReport {
    pub width: u32,
    pub height: u32,
    pub contrast: f64,
    pub mean_luminance: f64,
    pub sharpness: f64,
    pub quality_score: f64,
    pub warnings: Vec<QualityWarning>,
    pub disposition: QualityDisposition,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PreparedImage {
    image: GrayImage,
    quality: QualityReport,
    transforms: TransformChain,
}

impl PreparedImage {
    pub fn dimensions(&self) -> (u32, u32) {
        self.image.dimensions()
    }

    pub fn pixels(&self) -> &[u8] {
        self.image.as_raw()
    }

    pub fn quality(&self) -> &QualityReport {
        &self.quality
    }

    pub fn transforms(&self) -> &TransformChain {
        &self.transforms
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum PreparationOutcome {
    Ready(PreparedImage),
    RequestBetterSource(QualityReport),
}

pub fn prepare_image(
    encoded: &[u8],
    limits: ImageLimits,
    thresholds: QualityThresholds,
) -> Result<PreparationOutcome> {
    let quality = inspect_image_with_thresholds(encoded, limits, thresholds)?;
    if quality.disposition == QualityDisposition::RequestBetterSource {
        return Ok(PreparationOutcome::RequestBetterSource(quality));
    }

    let mut decoder = reader(encoded)?
        .into_decoder()
        .map_err(|_| Error::UnsupportedOrInvalidImage)?;
    let orientation = decoder
        .orientation()
        .map_err(|_| Error::UnsupportedOrInvalidImage)?;
    let mut image =
        image::DynamicImage::from_decoder(decoder).map_err(|_| Error::UnsupportedOrInvalidImage)?;
    image.apply_orientation(orientation);
    let mut transforms = TransformChain::new();
    record_orientation(&mut transforms, orientation);
    Ok(PreparationOutcome::Ready(PreparedImage {
        image: image.into_luma8(),
        quality,
        transforms,
    }))
}

fn record_orientation(transforms: &mut TransformChain, orientation: Orientation) {
    match orientation {
        Orientation::NoTransforms => {}
        Orientation::Rotate90 => transforms.push_rotation(Rotation::Clockwise90),
        Orientation::Rotate180 => transforms.push_rotation(Rotation::Clockwise180),
        Orientation::Rotate270 => transforms.push_rotation(Rotation::Clockwise270),
        Orientation::FlipHorizontal => transforms.push_flip_horizontal(),
        Orientation::FlipVertical => transforms.push_flip_vertical(),
        Orientation::Rotate90FlipH => {
            transforms.push_rotation(Rotation::Clockwise90);
            transforms.push_flip_horizontal();
        }
        Orientation::Rotate270FlipH => {
            transforms.push_rotation(Rotation::Clockwise270);
            transforms.push_flip_horizontal();
        }
    }
}

pub fn inspect_image(encoded: &[u8], limits: ImageLimits) -> Result<QualityReport> {
    inspect_image_with_thresholds(encoded, limits, QualityThresholds::new(1, 1)?)
}

pub fn inspect_image_with_thresholds(
    encoded: &[u8],
    limits: ImageLimits,
    thresholds: QualityThresholds,
) -> Result<QualityReport> {
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
    let mean_luminance = grayscale
        .pixels()
        .map(|pixel| f64::from(pixel.0[0]))
        .sum::<f64>()
        / pixels as f64
        / 255.0;
    let sharpness = edge_difference_score(&grayscale);
    let blank = contrast < BLANK_CONTRAST_THRESHOLD;
    let low_resolution = width < thresholds.minimum_width || height < thresholds.minimum_height;
    let mut warnings = Vec::new();
    if blank {
        warnings.push(QualityWarning::Blank);
    }
    if !blank && sharpness < thresholds.minimum_sharpness {
        warnings.push(QualityWarning::Blurry);
    }
    if low_resolution {
        warnings.push(QualityWarning::LowResolution);
    }
    if !blank && mean_luminance > OVEREXPOSED_LUMINANCE_THRESHOLD {
        warnings.push(QualityWarning::Overexposed);
    }
    if !blank && mean_luminance < UNDEREXPOSED_LUMINANCE_THRESHOLD {
        warnings.push(QualityWarning::Underexposed);
    }
    let resolution_score = (f64::from(width) / f64::from(thresholds.minimum_width))
        .min(f64::from(height) / f64::from(thresholds.minimum_height))
        .min(1.0);
    let exposure_score = (1.0 - (mean_luminance - 0.5).abs() * 2.0).max(0.0);

    Ok(QualityReport {
        width,
        height,
        contrast,
        mean_luminance,
        sharpness,
        quality_score: if blank {
            0.0
        } else {
            contrast * resolution_score * exposure_score
        },
        disposition: if warnings.is_empty() {
            QualityDisposition::Continue
        } else {
            QualityDisposition::RequestBetterSource
        },
        warnings,
    })
}

fn edge_difference_score(image: &image::GrayImage) -> f64 {
    let mut total = 0_u64;
    let mut comparisons = 0_u64;
    for y in 0..image.height() {
        for x in 0..image.width() {
            let current = image.get_pixel(x, y).0[0];
            if x + 1 < image.width() {
                total += u64::from(current.abs_diff(image.get_pixel(x + 1, y).0[0]));
                comparisons += 1;
            }
            if y + 1 < image.height() {
                total += u64::from(current.abs_diff(image.get_pixel(x, y + 1).0[0]));
                comparisons += 1;
            }
        }
    }
    if comparisons == 0 {
        0.0
    } else {
        total as f64 / comparisons as f64 / 255.0
    }
}

fn reader(encoded: &[u8]) -> Result<ImageReader<Cursor<&[u8]>>> {
    ImageReader::new(Cursor::new(encoded))
        .with_guessed_format()
        .map_err(|_| Error::UnsupportedOrInvalidImage)
}
