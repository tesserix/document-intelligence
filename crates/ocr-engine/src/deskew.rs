use image::{GrayImage, Luma};

use crate::{DeskewAngle, PreparedImage, Result};

const MAXIMUM_SAMPLED_PIXELS: usize = 50_000;
const MAXIMUM_DEGREES: f64 = 12.0;
const ANGLE_STEP_DEGREES: f64 = 0.25;
const CANDIDATE_ANGLES: usize = 97;

#[derive(Debug, Copy, Clone, PartialEq)]
pub struct DeskewPolicy {
    minimum_confidence: f64,
    minimum_material_degrees: f64,
}

impl DeskewPolicy {
    pub fn new(minimum_confidence: f64, minimum_material_degrees: f64) -> Result<Self> {
        if !minimum_confidence.is_finite()
            || !(0.0..=1.0).contains(&minimum_confidence)
            || !minimum_material_degrees.is_finite()
            || !(0.0..=MAXIMUM_DEGREES).contains(&minimum_material_degrees)
        {
            return Err(crate::Error::InvalidDeskewPolicy);
        }
        Ok(Self {
            minimum_confidence,
            minimum_material_degrees,
        })
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum DeskewDecision {
    Applied,
    BelowConfidence,
    BelowMaterialAngle,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DeskewResult {
    image: PreparedImage,
    estimate: SkewEstimate,
    decision: DeskewDecision,
}

impl DeskewResult {
    pub fn image(&self) -> &PreparedImage {
        &self.image
    }

    pub fn estimate(&self) -> SkewEstimate {
        self.estimate
    }

    pub fn decision(&self) -> DeskewDecision {
        self.decision
    }
}

#[derive(Debug, Copy, Clone, PartialEq)]
pub struct SkewEstimate {
    correction: DeskewAngle,
    confidence: f64,
    sampled_points: usize,
}

impl SkewEstimate {
    pub fn correction(self) -> DeskewAngle {
        self.correction
    }

    pub fn confidence(self) -> f64 {
        self.confidence
    }

    pub fn sampled_points(self) -> usize {
        self.sampled_points
    }

    pub fn candidate_angles(self) -> usize {
        CANDIDATE_ANGLES
    }
}

pub fn estimate_skew(image: &PreparedImage) -> Result<SkewEstimate> {
    let sampled = sample_pixels(&image.image);
    let minimum = sampled
        .iter()
        .map(|(_, _, value)| *value)
        .min()
        .unwrap_or(0);
    let maximum = sampled
        .iter()
        .map(|(_, _, value)| *value)
        .max()
        .unwrap_or(0);
    let threshold = minimum.saturating_add(maximum.saturating_sub(minimum) / 2);
    let ink = sampled
        .iter()
        .filter(|(_, _, value)| *value <= threshold)
        .map(|(x, y, _)| (*x, *y))
        .collect::<Vec<_>>();
    let sampled_points = sampled.len();
    if ink.len() < 32 || minimum == maximum {
        return Ok(SkewEstimate {
            correction: DeskewAngle::new(0.0)?,
            confidence: 0.0,
            sampled_points,
        });
    }

    let mut best_angle = 0.0_f64;
    let mut best_score = 0.0_f64;
    let mut baseline_score = 0.0_f64;
    for index in 0..CANDIDATE_ANGLES {
        let angle = -MAXIMUM_DEGREES + index as f64 * ANGLE_STEP_DEGREES;
        let score = projection_score(&ink, image.image.width(), image.image.height(), angle);
        if angle == 0.0 {
            baseline_score = score;
        }
        if score > best_score
            || (score == best_score && angle.abs() < best_angle.abs())
            || (score == best_score && angle.abs() == best_angle.abs() && angle < best_angle)
        {
            best_score = score;
            best_angle = angle;
        }
    }
    let confidence = if best_angle.abs() == MAXIMUM_DEGREES {
        0.0
    } else if best_score > 0.0 {
        ((best_score - baseline_score) / best_score).clamp(0.0, 1.0)
    } else {
        0.0
    };
    Ok(SkewEstimate {
        correction: DeskewAngle::new(best_angle)?,
        confidence,
        sampled_points,
    })
}

pub fn deskew_image(mut image: PreparedImage, policy: DeskewPolicy) -> Result<DeskewResult> {
    let estimate = estimate_skew(&image)?;
    let decision = if estimate.correction().degrees().abs() < policy.minimum_material_degrees {
        DeskewDecision::BelowMaterialAngle
    } else if estimate.confidence() < policy.minimum_confidence {
        DeskewDecision::BelowConfidence
    } else {
        image.image = rotate_bilinear(&image.image, estimate.correction());
        image.transforms.push_deskew(estimate.correction());
        DeskewDecision::Applied
    };
    Ok(DeskewResult {
        image,
        estimate,
        decision,
    })
}

fn sample_pixels(image: &GrayImage) -> Vec<(f64, f64, u8)> {
    let pixels = u64::from(image.width()) * u64::from(image.height());
    let stride = ((pixels as f64 / MAXIMUM_SAMPLED_PIXELS as f64)
        .sqrt()
        .ceil() as usize)
        .max(1);
    let mut sampled = Vec::with_capacity(MAXIMUM_SAMPLED_PIXELS.min(pixels as usize));
    'rows: for y in (0..image.height()).step_by(stride) {
        for x in (0..image.width()).step_by(stride) {
            if sampled.len() == MAXIMUM_SAMPLED_PIXELS {
                break 'rows;
            }
            sampled.push((f64::from(x), f64::from(y), image.get_pixel(x, y).0[0]));
        }
    }
    sampled
}

fn projection_score(ink: &[(f64, f64)], width: u32, height: u32, degrees: f64) -> f64 {
    let diagonal = f64::from(width).hypot(f64::from(height));
    let mut bins = vec![0_u32; diagonal.ceil() as usize + 3];
    let radians = degrees.to_radians();
    let center_x = f64::from(width) / 2.0;
    let center_y = f64::from(height) / 2.0;
    for (x, y) in ink {
        let projected = radians.sin() * (x - center_x) + radians.cos() * (y - center_y);
        let index = (projected + diagonal / 2.0)
            .round()
            .clamp(0.0, bins.len().saturating_sub(1) as f64) as usize;
        bins[index] += 1;
    }
    bins.into_iter()
        .map(|count| f64::from(count).powi(2))
        .sum::<f64>()
        / ink.len() as f64
}

fn rotate_bilinear(source: &GrayImage, angle: DeskewAngle) -> GrayImage {
    let radians = angle.degrees().to_radians();
    let cosine = radians.cos();
    let sine = radians.sin();
    let center_x = f64::from(source.width().saturating_sub(1)) / 2.0;
    let center_y = f64::from(source.height().saturating_sub(1)) / 2.0;
    GrayImage::from_fn(source.width(), source.height(), |x, y| {
        let translated_x = f64::from(x) - center_x;
        let translated_y = f64::from(y) - center_y;
        let source_x = center_x + cosine * translated_x + sine * translated_y;
        let source_y = center_y - sine * translated_x + cosine * translated_y;
        Luma([bilinear_luma(source, source_x, source_y)])
    })
}

fn bilinear_luma(source: &GrayImage, x: f64, y: f64) -> u8 {
    let maximum_x = f64::from(source.width().saturating_sub(1));
    let maximum_y = f64::from(source.height().saturating_sub(1));
    if x < 0.0 || y < 0.0 || x > maximum_x || y > maximum_y {
        return u8::MAX;
    }
    let left = x.floor() as u32;
    let top = y.floor() as u32;
    let right = left.saturating_add(1).min(source.width() - 1);
    let bottom = top.saturating_add(1).min(source.height() - 1);
    let horizontal = x - f64::from(left);
    let vertical = y - f64::from(top);
    let top_value = f64::from(source.get_pixel(left, top).0[0]) * (1.0 - horizontal)
        + f64::from(source.get_pixel(right, top).0[0]) * horizontal;
    let bottom_value = f64::from(source.get_pixel(left, bottom).0[0]) * (1.0 - horizontal)
        + f64::from(source.get_pixel(right, bottom).0[0]) * horizontal;
    (top_value * (1.0 - vertical) + bottom_value * vertical).round() as u8
}
