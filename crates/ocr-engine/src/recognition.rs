use serde::{Deserialize, Serialize};

use crate::{Error, Result};

const MAXIMUM_RECOGNIZED_TEXT_BYTES: usize = 4096;
const MAXIMUM_ALTERNATIVES: usize = 16;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "RawRecognitionAlternative")]
pub struct RecognitionAlternative {
    text: String,
    confidence: f64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRecognitionAlternative {
    text: String,
    confidence: f64,
}

impl TryFrom<RawRecognitionAlternative> for RecognitionAlternative {
    type Error = Error;

    fn try_from(value: RawRecognitionAlternative) -> Result<Self> {
        Self::new(value.text, value.confidence)
    }
}

impl RecognitionAlternative {
    pub fn new(text: impl Into<String>, confidence: f64) -> Result<Self> {
        let text = text.into();
        if !valid_text(&text) || !valid_confidence(confidence) {
            return Err(Error::InvalidRecognition);
        }
        Ok(Self { text, confidence })
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn confidence(&self) -> f64 {
        self.confidence
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "RawRecognizedRegion")]
pub struct RecognizedRegion {
    region_index: usize,
    text: String,
    confidence: f64,
    alternatives: Vec<RecognitionAlternative>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRecognizedRegion {
    region_index: usize,
    text: String,
    confidence: f64,
    alternatives: Vec<RecognitionAlternative>,
}

impl TryFrom<RawRecognizedRegion> for RecognizedRegion {
    type Error = Error;

    fn try_from(value: RawRecognizedRegion) -> Result<Self> {
        Self::new(
            value.region_index,
            value.text,
            value.confidence,
            value.alternatives,
        )
    }
}

impl RecognizedRegion {
    pub fn new(
        region_index: usize,
        text: impl Into<String>,
        confidence: f64,
        alternatives: Vec<RecognitionAlternative>,
    ) -> Result<Self> {
        let text = text.into();
        if !valid_text(&text)
            || !valid_confidence(confidence)
            || alternatives.len() > MAXIMUM_ALTERNATIVES
        {
            return Err(Error::InvalidRecognition);
        }
        Ok(Self {
            region_index,
            text,
            confidence,
            alternatives,
        })
    }

    pub fn region_index(&self) -> usize {
        self.region_index
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn confidence(&self) -> f64 {
        self.confidence
    }

    pub fn alternatives(&self) -> &[RecognitionAlternative] {
        &self.alternatives
    }
}

pub fn assemble_recognitions(
    mut outputs: Vec<RecognizedRegion>,
    region_count: usize,
) -> Result<Vec<RecognizedRegion>> {
    if outputs.len() != region_count {
        return Err(Error::InvalidRecognitionBatch);
    }
    outputs.sort_by_key(RecognizedRegion::region_index);
    if outputs
        .iter()
        .enumerate()
        .any(|(expected, output)| output.region_index != expected)
    {
        return Err(Error::InvalidRecognitionBatch);
    }
    Ok(outputs)
}

fn valid_text(value: &str) -> bool {
    !value.trim().is_empty() && value.len() <= MAXIMUM_RECOGNIZED_TEXT_BYTES
}

fn valid_confidence(value: f64) -> bool {
    value.is_finite() && (0.0..=1.0).contains(&value)
}
