use std::collections::BTreeSet;

use ocr_domain::{
    DocumentId, DocumentPage, DocumentResult, DocumentResultPayload, DocumentVersion, Evidence,
    ObservationId,
};
use thiserror::Error;

use crate::MAXIMUM_RESULT_BYTES;

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
    mut pages: Vec<DocumentPage>,
) -> Result<DocumentResult, ResultAssemblyError> {
    pages.sort_by_key(|page| u32::from(page.page));
    let mut text = String::new();
    let mut citations = Vec::new();

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
            append_bounded(&mut text, &observation.text)?;
            citations.push(Evidence::new(
                page.page,
                observation.polygon.clone(),
                observation.observation_id.clone(),
            ));
        }
    }

    let result = DocumentResult::new(
        document_id,
        document_version,
        DocumentResultPayload {
            text,
            pages,
            citations,
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
