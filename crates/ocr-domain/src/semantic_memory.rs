use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    is_canonical_sha256, validated_id, DocumentId, DocumentVersion, Error, ObservationId, Result,
    TenantId,
};

const MAXIMUM_QUERY_RESULTS: usize = 100;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct MemoryRecordId(String);

impl MemoryRecordId {
    pub fn new(value: &str) -> Result<Self> {
        validated_id(value, "mem_", Error::InvalidMemoryRecordId).map(Self)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for MemoryRecordId {
    type Error = Error;

    fn try_from(value: String) -> Result<Self> {
        Self::new(&value)
    }
}

impl From<MemoryRecordId> for String {
    fn from(value: MemoryRecordId) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ChunkId(String);

impl ChunkId {
    pub fn new(value: &str) -> Result<Self> {
        validated_id(value, "chk_", Error::InvalidChunkId).map(Self)
    }
}

impl TryFrom<String> for ChunkId {
    type Error = Error;

    fn try_from(value: String) -> Result<Self> {
        Self::new(&value)
    }
}

impl From<ChunkId> for String {
    fn from(value: ChunkId) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct MemoryRecordVersion(String);

impl MemoryRecordVersion {
    pub fn new(value: &str) -> Result<Self> {
        if is_canonical_sha256(value) {
            Ok(Self(value.to_owned()))
        } else {
            Err(Error::InvalidMemoryRecordVersion)
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for MemoryRecordVersion {
    type Error = Error;

    fn try_from(value: String) -> Result<Self> {
        Self::new(&value)
    }
}

impl From<MemoryRecordVersion> for String {
    fn from(value: MemoryRecordVersion) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct EmbeddingVersion(String);

impl EmbeddingVersion {
    pub fn new(value: &str) -> Result<Self> {
        let suffix = value.strip_prefix("emb_").unwrap_or_default();
        if !suffix.is_empty()
            && suffix.len() <= 64
            && suffix
                .as_bytes()
                .first()
                .zip(suffix.as_bytes().last())
                .is_some_and(|(first, last)| {
                    first.is_ascii_alphanumeric() && last.is_ascii_alphanumeric()
                })
            && suffix.bytes().all(|byte| {
                byte.is_ascii_lowercase()
                    || byte.is_ascii_digit()
                    || matches!(byte, b'.' | b'_' | b'-')
            })
        {
            Ok(Self(value.to_owned()))
        } else {
            Err(Error::InvalidEmbeddingVersion)
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for EmbeddingVersion {
    type Error = Error;

    fn try_from(value: String) -> Result<Self> {
        Self::new(&value)
    }
}

impl From<EmbeddingVersion> for String {
    fn from(value: EmbeddingVersion) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VectorPointId(String);

impl VectorPointId {
    pub fn derive(
        tenant_id: &TenantId,
        record_id: &MemoryRecordId,
        record_version: &MemoryRecordVersion,
        embedding_version: &EmbeddingVersion,
    ) -> Self {
        let mut digest = Sha256::new();
        digest.update(b"ocr-memory-point-v1");
        for value in [
            tenant_id.as_str(),
            record_id.as_str(),
            record_version.as_str(),
            embedding_version.as_str(),
        ] {
            digest.update((value.len() as u64).to_be_bytes());
            digest.update(value.as_bytes());
        }
        let digest = digest.finalize();
        let mut bytes = [0_u8; 16];
        bytes.copy_from_slice(&digest[..16]);
        bytes[6] = (bytes[6] & 0x0f) | 0x80;
        bytes[8] = (bytes[8] & 0x3f) | 0x80;
        Self(Uuid::from_bytes(bytes).to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "RawSemanticCollection")]
pub struct SemanticCollection {
    schema_major: u16,
    embedding_major: u16,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSemanticCollection {
    schema_major: u16,
    embedding_major: u16,
}

impl TryFrom<RawSemanticCollection> for SemanticCollection {
    type Error = Error;

    fn try_from(value: RawSemanticCollection) -> Result<Self> {
        Self::new(value.schema_major, value.embedding_major)
    }
}

impl SemanticCollection {
    pub fn new(schema_major: u16, embedding_major: u16) -> Result<Self> {
        if schema_major == 0 || embedding_major == 0 {
            Err(Error::InvalidSemanticCollection)
        } else {
            Ok(Self {
                schema_major,
                embedding_major,
            })
        }
    }

    pub fn alias(self) -> String {
        format!(
            "ocr-memory-s{}-e{}",
            self.schema_major, self.embedding_major
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticQueryScope {
    tenant_id: TenantId,
    collection: SemanticCollection,
    limit: usize,
}

impl SemanticQueryScope {
    pub fn new(tenant_id: TenantId, collection: SemanticCollection, limit: usize) -> Result<Self> {
        if !(1..=MAXIMUM_QUERY_RESULTS).contains(&limit) {
            return Err(Error::InvalidSemanticQueryScope);
        }
        Ok(Self {
            tenant_id,
            collection,
            limit,
        })
    }

    pub fn tenant_id(&self) -> &TenantId {
        &self.tenant_id
    }

    pub fn collection(&self) -> SemanticCollection {
        self.collection
    }

    pub fn limit(&self) -> usize {
        self.limit
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "VectorPointMetadataInput")]
pub struct VectorPointMetadata {
    tenant_id: TenantId,
    memory_record_id: MemoryRecordId,
    memory_record_version: MemoryRecordVersion,
    document_id: DocumentId,
    document_version: DocumentVersion,
    chunk_id: ChunkId,
    observation_ids: Vec<ObservationId>,
    embedding_version: EmbeddingVersion,
    collection: SemanticCollection,
    retention_deadline_unix_seconds: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VectorPointMetadataInput {
    pub tenant_id: TenantId,
    pub memory_record_id: MemoryRecordId,
    pub memory_record_version: MemoryRecordVersion,
    pub document_id: DocumentId,
    pub document_version: DocumentVersion,
    pub chunk_id: ChunkId,
    pub observation_ids: Vec<ObservationId>,
    pub embedding_version: EmbeddingVersion,
    pub collection: SemanticCollection,
    pub retention_deadline_unix_seconds: i64,
}

impl TryFrom<VectorPointMetadataInput> for VectorPointMetadata {
    type Error = Error;

    fn try_from(value: VectorPointMetadataInput) -> Result<Self> {
        Self::new(value)
    }
}

impl VectorPointMetadata {
    pub fn new(input: VectorPointMetadataInput) -> Result<Self> {
        let evidence_is_valid = !input.observation_ids.is_empty()
            && input.observation_ids.len() <= 128
            && input
                .observation_ids
                .iter()
                .enumerate()
                .all(|(index, id)| !input.observation_ids[..index].contains(id));
        if !evidence_is_valid || input.retention_deadline_unix_seconds <= 0 {
            return Err(Error::InvalidVectorPointMetadata);
        }
        Ok(Self {
            tenant_id: input.tenant_id,
            memory_record_id: input.memory_record_id,
            memory_record_version: input.memory_record_version,
            document_id: input.document_id,
            document_version: input.document_version,
            chunk_id: input.chunk_id,
            observation_ids: input.observation_ids,
            embedding_version: input.embedding_version,
            collection: input.collection,
            retention_deadline_unix_seconds: input.retention_deadline_unix_seconds,
        })
    }
}
