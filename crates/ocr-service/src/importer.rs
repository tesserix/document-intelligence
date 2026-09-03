use std::{future::Future, sync::Arc};

use ocr_domain::{ProductId, TenantId, UploadId};
use ocr_store::{
    AcceptUpload, AcceptUploadOutcome, ClaimUploadInspection, ClaimUploadInspectionOutcome,
    ParserInspectionMetadata, PgJobStore, RejectUploadOutcome, StoredUpload, UploadRejectionReason,
    UploadState,
};
use thiserror::Error;

use crate::{
    GcsSourcePromoter, GcsUploadMalwareInspector, ParserInspectionReport, ParserProcess,
    ParserProcessError, PromotedSource, SourcePromotionError, UploadInspectionError,
    PARSER_PROFILE, PARSER_VERSION,
};

#[derive(Debug, Copy, Clone, Error, PartialEq, Eq)]
pub enum DocumentReadError {
    #[error("document source is invalid")]
    Invalid,
    #[error("document source is unavailable")]
    Unavailable,
}

#[derive(Debug, Copy, Clone, Error, PartialEq, Eq)]
pub enum DocumentParseError {
    #[error("document is invalid")]
    Invalid,
    #[error("document exceeds parser limits")]
    LimitsExceeded,
    #[error("document password is required")]
    PasswordRequired,
    #[error("parser is unavailable")]
    Unavailable,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum ImportOutcome {
    Accepted,
    Existing,
    Rejected,
    Busy,
    NotFound,
    LeaseLost,
    Retryable,
    Conflict,
    NotInspectable,
}

#[derive(Debug, Error)]
pub enum ImportError {
    #[error(transparent)]
    Store(#[from] ocr_store::Error),
}

pub trait MalwareInspector: Send + Sync {
    fn inspect<'a>(
        &'a self,
        upload: &'a StoredUpload,
    ) -> impl Future<Output = Result<(), UploadInspectionError>> + Send + 'a;
}

pub trait UploadDocumentReader: Send + Sync {
    fn read<'a>(
        &'a self,
        upload: &'a StoredUpload,
    ) -> impl Future<Output = Result<Vec<u8>, DocumentReadError>> + Send + 'a;
}

pub trait DocumentParser: Send + Sync {
    fn inspect<'a>(
        &'a self,
        encoded: &'a [u8],
        content_type: &'a str,
    ) -> impl Future<Output = Result<ParserInspectionReport, DocumentParseError>> + Send + 'a;
}

pub trait SourcePromoter: Send + Sync {
    fn promote<'a>(
        &'a self,
        product_id: &'a ProductId,
        tenant_id: &'a TenantId,
        upload: &'a StoredUpload,
    ) -> impl Future<Output = Result<PromotedSource, SourcePromotionError>> + Send + 'a;
}

pub struct Importer<M, R, P, S> {
    jobs: PgJobStore,
    malware: Arc<M>,
    documents: Arc<R>,
    parser: Arc<P>,
    sources: Arc<S>,
}

impl<M, R, P, S> Importer<M, R, P, S>
where
    M: MalwareInspector,
    R: UploadDocumentReader,
    P: DocumentParser,
    S: SourcePromoter,
{
    pub fn new(
        jobs: PgJobStore,
        malware: Arc<M>,
        documents: Arc<R>,
        parser: Arc<P>,
        sources: Arc<S>,
    ) -> Self {
        Self {
            jobs,
            malware,
            documents,
            parser,
            sources,
        }
    }

    pub async fn process(
        &self,
        tenant_id: &TenantId,
        product_id: &ProductId,
        upload_id: &UploadId,
        lease_owner: &str,
    ) -> Result<ImportOutcome, ImportError> {
        match self
            .jobs
            .claim_upload_inspection(
                tenant_id,
                product_id,
                upload_id,
                ClaimUploadInspection {
                    lease_owner: lease_owner.to_owned(),
                },
            )
            .await?
        {
            ClaimUploadInspectionOutcome::Claimed | ClaimUploadInspectionOutcome::Existing => {}
            ClaimUploadInspectionOutcome::Busy => return Ok(ImportOutcome::Busy),
            ClaimUploadInspectionOutcome::AttemptsExhausted => return Ok(ImportOutcome::Rejected),
            ClaimUploadInspectionOutcome::NotFound => return Ok(ImportOutcome::NotFound),
            ClaimUploadInspectionOutcome::NotInspectable => {
                return Ok(
                    match self
                        .jobs
                        .find_upload(tenant_id, product_id, upload_id)
                        .await?
                    {
                        Some(upload) if upload.state == UploadState::Accepted => {
                            ImportOutcome::Existing
                        }
                        Some(upload) if upload.state == UploadState::Rejected => {
                            ImportOutcome::Rejected
                        }
                        Some(_) => ImportOutcome::NotInspectable,
                        None => ImportOutcome::NotFound,
                    },
                );
            }
        }
        let Some(upload) = self
            .jobs
            .load_claimed_upload(tenant_id, product_id, upload_id, lease_owner)
            .await?
        else {
            return Ok(ImportOutcome::LeaseLost);
        };

        match self.malware.inspect(&upload).await {
            Ok(()) => {}
            Err(UploadInspectionError::Infected) => {
                return self
                    .reject(
                        tenant_id,
                        product_id,
                        upload_id,
                        lease_owner,
                        UploadRejectionReason::MalwareDetected,
                    )
                    .await
            }
            Err(UploadInspectionError::Invalid) => {
                return self
                    .reject(
                        tenant_id,
                        product_id,
                        upload_id,
                        lease_owner,
                        UploadRejectionReason::InvalidDocument,
                    )
                    .await
            }
            Err(UploadInspectionError::Unavailable) => return Ok(ImportOutcome::Retryable),
        }
        let bytes = match self.documents.read(&upload).await {
            Ok(bytes) => bytes,
            Err(DocumentReadError::Invalid) => {
                return self
                    .reject(
                        tenant_id,
                        product_id,
                        upload_id,
                        lease_owner,
                        UploadRejectionReason::InvalidDocument,
                    )
                    .await
            }
            Err(DocumentReadError::Unavailable) => return Ok(ImportOutcome::Retryable),
        };
        let inspection = match self
            .parser
            .inspect(&bytes, &upload.expected_content_type)
            .await
        {
            Ok(inspection) => inspection,
            Err(DocumentParseError::Invalid) => {
                return self
                    .reject(
                        tenant_id,
                        product_id,
                        upload_id,
                        lease_owner,
                        UploadRejectionReason::InvalidDocument,
                    )
                    .await
            }
            Err(DocumentParseError::LimitsExceeded) => {
                return self
                    .reject(
                        tenant_id,
                        product_id,
                        upload_id,
                        lease_owner,
                        UploadRejectionReason::ParserLimitsExceeded,
                    )
                    .await
            }
            Err(DocumentParseError::PasswordRequired) => {
                return self
                    .reject(
                        tenant_id,
                        product_id,
                        upload_id,
                        lease_owner,
                        UploadRejectionReason::PasswordRequired,
                    )
                    .await
            }
            Err(DocumentParseError::Unavailable) => return Ok(ImportOutcome::Retryable),
        };
        let source = match self.sources.promote(product_id, tenant_id, &upload).await {
            Ok(source) => source,
            Err(SourcePromotionError::InvalidSource) => {
                return self
                    .reject(
                        tenant_id,
                        product_id,
                        upload_id,
                        lease_owner,
                        UploadRejectionReason::InvalidDocument,
                    )
                    .await
            }
            Err(SourcePromotionError::DestinationConflict) => {
                return self
                    .reject(
                        tenant_id,
                        product_id,
                        upload_id,
                        lease_owner,
                        UploadRejectionReason::SourceConflict,
                    )
                    .await
            }
            Err(SourcePromotionError::InvalidConfiguration | SourcePromotionError::Unavailable) => {
                return Ok(ImportOutcome::Retryable)
            }
        };
        let outcome = self
            .jobs
            .accept_upload(
                tenant_id,
                product_id,
                upload_id,
                AcceptUpload {
                    inspection_lease_owner: lease_owner.to_owned(),
                    source_bucket: source.bucket,
                    source_object_name: source.object_name,
                    source_object_generation: source.generation,
                    source_digest: source.digest,
                    source_content_length: source.content_length,
                    parser_inspection: ParserInspectionMetadata {
                        page_count: inspection.page_count,
                        maximum_page_pixels: inspection.maximum_page_pixels,
                        total_page_pixels: inspection.total_page_pixels,
                        profile: PARSER_PROFILE.to_owned(),
                        version: PARSER_VERSION.to_owned(),
                    },
                },
            )
            .await?;
        Ok(match outcome {
            AcceptUploadOutcome::Accepted => ImportOutcome::Accepted,
            AcceptUploadOutcome::Existing => ImportOutcome::Existing,
            AcceptUploadOutcome::SourceMismatch => ImportOutcome::Conflict,
            AcceptUploadOutcome::NotAcceptable => ImportOutcome::LeaseLost,
            AcceptUploadOutcome::NotFound => ImportOutcome::NotFound,
        })
    }

    async fn reject(
        &self,
        tenant_id: &TenantId,
        product_id: &ProductId,
        upload_id: &UploadId,
        lease_owner: &str,
        reason: UploadRejectionReason,
    ) -> Result<ImportOutcome, ImportError> {
        Ok(
            match self
                .jobs
                .reject_upload(tenant_id, product_id, upload_id, lease_owner, reason)
                .await?
            {
                RejectUploadOutcome::Rejected | RejectUploadOutcome::Existing => {
                    ImportOutcome::Rejected
                }
                RejectUploadOutcome::ReasonMismatch => ImportOutcome::Conflict,
                RejectUploadOutcome::NotRejectable => ImportOutcome::LeaseLost,
                RejectUploadOutcome::NotFound => ImportOutcome::NotFound,
            },
        )
    }
}

impl MalwareInspector for GcsUploadMalwareInspector {
    fn inspect<'a>(
        &'a self,
        upload: &'a StoredUpload,
    ) -> impl Future<Output = Result<(), UploadInspectionError>> + Send + 'a {
        GcsUploadMalwareInspector::inspect(self, upload)
    }
}

impl DocumentParser for ParserProcess {
    async fn inspect<'a>(
        &'a self,
        encoded: &'a [u8],
        content_type: &'a str,
    ) -> Result<ParserInspectionReport, DocumentParseError> {
        ParserProcess::inspect(self, encoded, content_type)
            .await
            .map_err(|error| match error {
                ParserProcessError::InvalidDocument => DocumentParseError::Invalid,
                ParserProcessError::LimitsExceeded => DocumentParseError::LimitsExceeded,
                ParserProcessError::PasswordRequired => DocumentParseError::PasswordRequired,
                ParserProcessError::InvalidConfiguration | ParserProcessError::Unavailable => {
                    DocumentParseError::Unavailable
                }
            })
    }
}

impl SourcePromoter for GcsSourcePromoter {
    fn promote<'a>(
        &'a self,
        product_id: &'a ProductId,
        tenant_id: &'a TenantId,
        upload: &'a StoredUpload,
    ) -> impl Future<Output = Result<PromotedSource, SourcePromotionError>> + Send + 'a {
        GcsSourcePromoter::promote(self, product_id, tenant_id, upload)
    }
}
