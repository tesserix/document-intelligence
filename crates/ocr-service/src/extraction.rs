use std::collections::BTreeMap;

use ocr_domain::{DocumentPage, ExtractedValue, ValidationFailure};

use crate::nutrition_label;

/// An immutable, registered extraction schema; clients name one, never define one.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum ExtractionSchema {
    KoraNutritionLabelV1,
}

#[derive(Debug, Default)]
pub struct ExtractedFields {
    pub fields: BTreeMap<String, ExtractedValue>,
    pub validation_failures: Vec<ValidationFailure>,
}

impl ExtractionSchema {
    pub fn registered(schema_id: &str, schema_version: &str) -> Option<Self> {
        match (schema_id, schema_version) {
            ("kora.nutrition_label", "1") => Some(Self::KoraNutritionLabelV1),
            _ => None,
        }
    }

    pub fn extract(self, pages: &[DocumentPage]) -> ExtractedFields {
        match self {
            Self::KoraNutritionLabelV1 => nutrition_label::extract(pages),
        }
    }
}
