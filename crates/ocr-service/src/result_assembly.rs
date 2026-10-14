use ocr_domain::ProcessingProvenance;
use std::collections::BTreeSet;

use ocr_domain::{
    DocumentId, DocumentPage, DocumentResult, DocumentResultPayload, DocumentVersion, Evidence,
    ObservationId, StableCode, ValidationFailure, ValidationSeverity,
};
use thiserror::Error;

use crate::{ExtractionSchema, MAXIMUM_RESULT_BYTES};

#[derive(Debug, Error)]
pub enum ResultAssemblyError {
    #[error("assembled result exceeds the service limit")]
    TooLarge,
    #[error(transparent)]
    Domain(#[from] ocr_domain::Error),
    #[error("assembled result cannot be serialized")]
    Serialization,
}

pub fn assemble_document_result(
    document_id: DocumentId,
    document_version: DocumentVersion,
    pages: Vec<DocumentPage>,
    schema: Option<ExtractionSchema>,
) -> Result<DocumentResult, ResultAssemblyError> {
    assemble_with_provenance(document_id, document_version, pages, schema, None)
}

pub fn assemble_native_document_result(
    document_id: DocumentId,
    document_version: DocumentVersion,
    pages: Vec<DocumentPage>,
    schema: Option<ExtractionSchema>,
    duration_ms: u64,
) -> Result<DocumentResult, ResultAssemblyError> {
    let legacy = pages
        .iter()
        .flat_map(|page| &page.observations)
        .any(|observation| {
            !String::from(observation.observation_id.clone()).starts_with("obs_native_")
        });
    let (provider, model, profile) = if legacy {
        ("legacy-mixed", "unknown", "native-text-transition-v1")
    } else {
        ("native-text", "none", "native-text-v1")
    };
    let provenance = ProcessingProvenance::new(provider, model, profile, duration_ms)?;
    assemble_with_provenance(
        document_id,
        document_version,
        pages,
        schema,
        Some(provenance),
    )
}

pub(crate) fn assemble_legacy_document_result(
    document_id: DocumentId,
    document_version: DocumentVersion,
    pages: Vec<DocumentPage>,
    schema: Option<ExtractionSchema>,
) -> Result<DocumentResult, ResultAssemblyError> {
    assemble_with_provenance(
        document_id,
        document_version,
        pages,
        schema,
        Some(ProcessingProvenance::new(
            "legacy-mixed",
            "unknown",
            "native-text-transition-v1",
            0,
        )?),
    )
}

fn assemble_with_provenance(
    document_id: DocumentId,
    document_version: DocumentVersion,
    mut pages: Vec<DocumentPage>,
    schema: Option<ExtractionSchema>,
    provenance: Option<ProcessingProvenance>,
) -> Result<DocumentResult, ResultAssemblyError> {
    pages.sort_by_key(|page| u32::from(page.page));
    let mut text = String::new();
    let mut citations = Vec::new();
    let mut low_confidence = false;

    for page in &pages {
        let parent_ids = page
            .observations
            .iter()
            .filter_map(|observation| observation.parent_observation_id.clone())
            .collect::<BTreeSet<ObservationId>>();
        let leaves = page
            .observations
            .iter()
            .filter(|observation| !parent_ids.contains(&observation.observation_id))
            .collect::<Vec<_>>();
        if leaves.is_empty() {
            continue;
        }
        if !text.is_empty() {
            append_bounded(&mut text, "\n\n")?;
        }
        for (index, observation) in leaves.into_iter().enumerate() {
            if index > 0 {
                append_bounded(&mut text, "\n")?;
            }
            low_confidence |= f64::from(observation.confidence) < 0.6;
            append_bounded(&mut text, &observation.text)?;
            citations.push(Evidence::new(
                page.page,
                observation.polygon.clone(),
                observation.observation_id.clone(),
            ));
        }
    }

    let mut extraction = schema
        .map(|schema| schema.extract(&pages))
        .unwrap_or_default();
    let mut warnings = Vec::new();
    if provenance
        .as_ref()
        .is_some_and(|profile| profile.provider == "legacy-mixed")
    {
        extraction.validation_failures.push(ValidationFailure::new(
            StableCode::new("legacy_processing_requires_review")?,
            ValidationSeverity::Warning,
        ));
        warnings.push(StableCode::new("manual_review_required")?);
    }
    if pages.iter().any(|page| page.observations.is_empty()) {
        extraction.validation_failures.push(ValidationFailure::new(
            StableCode::new("ocr_page_requires_manual_review")?,
            ValidationSeverity::Warning,
        ));
        warnings.push(StableCode::new("manual_review_required")?);
    }
    if low_confidence {
        extraction.validation_failures.push(ValidationFailure::new(
            StableCode::new("ocr_low_confidence")?,
            ValidationSeverity::Warning,
        ));
    }
    let incomplete_row = StableCode::new("nutrition_row_incomplete")?;
    let missing_panel = StableCode::new("nutrition_panel_not_found")?;
    if text.trim().is_empty() {
        extraction.validation_failures.push(ValidationFailure::new(
            StableCode::new("ocr_no_text_detected")?,
            ValidationSeverity::Error,
        ));
        warnings.push(StableCode::new("retake_include_readable_document")?);
    } else if low_confidence
        || extraction
            .validation_failures
            .iter()
            .any(|failure| failure.code == incomplete_row)
    {
        warnings.push(StableCode::new("retake_sharper_image")?);
    } else if extraction
        .validation_failures
        .iter()
        .any(|failure| failure.code == missing_panel)
    {
        warnings.push(StableCode::new("retake_include_column_headers")?);
    }
    let result = DocumentResult::new(
        document_id,
        document_version,
        DocumentResultPayload {
            text,
            pages,
            citations,
            fields: extraction.fields,
            warnings,
            validation_failures: extraction.validation_failures,
            provenance,
            ..DocumentResultPayload::default()
        },
    )?;
    let encoded = serde_json::to_vec(&result).map_err(|_| ResultAssemblyError::Serialization)?;
    if encoded.len() > MAXIMUM_RESULT_BYTES {
        return Err(ResultAssemblyError::TooLarge);
    }
    Ok(result)
}

fn append_bounded(output: &mut String, value: &str) -> Result<(), ResultAssemblyError> {
    let length = output
        .len()
        .checked_add(value.len())
        .ok_or(ResultAssemblyError::TooLarge)?;
    if length > MAXIMUM_RESULT_BYTES {
        return Err(ResultAssemblyError::TooLarge);
    }
    output.push_str(value);
    Ok(())
}
