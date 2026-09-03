use serde::{Deserialize, Serialize};

use crate::{Error, Result};

#[derive(Debug, Copy, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "RawDetectionCandidate")]
pub struct DetectionCandidate {
    left: f64,
    top: f64,
    right: f64,
    bottom: f64,
    confidence: f64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawDetectionCandidate {
    left: f64,
    top: f64,
    right: f64,
    bottom: f64,
    confidence: f64,
}

impl TryFrom<RawDetectionCandidate> for DetectionCandidate {
    type Error = Error;

    fn try_from(value: RawDetectionCandidate) -> Result<Self> {
        Self::new(
            value.left,
            value.top,
            value.right,
            value.bottom,
            value.confidence,
        )
    }
}

impl DetectionCandidate {
    pub fn new(left: f64, top: f64, right: f64, bottom: f64, confidence: f64) -> Result<Self> {
        let values = [left, top, right, bottom, confidence];
        if values.iter().any(|value| !value.is_finite())
            || !(0.0..=1.0).contains(&left)
            || !(0.0..=1.0).contains(&top)
            || !(0.0..=1.0).contains(&right)
            || !(0.0..=1.0).contains(&bottom)
            || !(0.0..=1.0).contains(&confidence)
            || left >= right
            || top >= bottom
        {
            return Err(Error::InvalidDetectionCandidate);
        }
        Ok(Self {
            left,
            top,
            right,
            bottom,
            confidence,
        })
    }

    pub fn bounds(self) -> (f64, f64, f64, f64) {
        (self.left, self.top, self.right, self.bottom)
    }

    pub fn confidence(self) -> f64 {
        self.confidence
    }
}

#[derive(Debug, Copy, Clone, PartialEq)]
pub struct DetectionLimits {
    maximum_candidates: usize,
    overlap_threshold: f64,
}

impl DetectionLimits {
    pub fn new(maximum_candidates: usize, overlap_threshold: f64) -> Result<Self> {
        if maximum_candidates == 0
            || !overlap_threshold.is_finite()
            || !(0.0 < overlap_threshold && overlap_threshold <= 1.0)
        {
            return Err(Error::InvalidDetectionLimits);
        }
        Ok(Self {
            maximum_candidates,
            overlap_threshold,
        })
    }
}

pub fn select_regions(
    candidates: &[DetectionCandidate],
    limits: DetectionLimits,
) -> Result<Vec<DetectionCandidate>> {
    if candidates.len() > limits.maximum_candidates {
        return Err(Error::DetectionOutputLimitExceeded {
            candidates: candidates.len(),
            limit: limits.maximum_candidates,
        });
    }
    let mut ranked = candidates.to_vec();
    ranked.sort_by(|left, right| {
        right
            .confidence
            .total_cmp(&left.confidence)
            .then_with(|| left.top.total_cmp(&right.top))
            .then_with(|| left.left.total_cmp(&right.left))
    });
    let mut selected = Vec::with_capacity(ranked.len());
    for candidate in ranked {
        if selected.iter().all(|accepted| {
            intersection_over_union(candidate, *accepted) < limits.overlap_threshold
        }) {
            selected.push(candidate);
        }
    }
    selected.sort_by(|left, right| {
        left.top
            .total_cmp(&right.top)
            .then_with(|| left.left.total_cmp(&right.left))
            .then_with(|| left.bottom.total_cmp(&right.bottom))
            .then_with(|| left.right.total_cmp(&right.right))
    });
    Ok(selected)
}

fn intersection_over_union(left: DetectionCandidate, right: DetectionCandidate) -> f64 {
    let intersection_width = (left.right.min(right.right) - left.left.max(right.left)).max(0.0);
    let intersection_height = (left.bottom.min(right.bottom) - left.top.max(right.top)).max(0.0);
    let intersection = intersection_width * intersection_height;
    let left_area = (left.right - left.left) * (left.bottom - left.top);
    let right_area = (right.right - right.left) * (right.bottom - right.top);
    intersection / (left_area + right_area - intersection)
}
