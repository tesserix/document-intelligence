//! Provider-neutral document intelligence contracts.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

const RESULT_SCHEMA_VERSION: &str = "1.0";

#[derive(Debug, Error, PartialEq)]
pub enum Error {
    #[error("confidence must be finite and between zero and one")]
    InvalidConfidence,
    #[error("page number must start at one")]
    InvalidPageNumber,
    #[error("normalized point must be finite and inside the source page")]
    InvalidPoint,
    #[error("polygon must contain a non-degenerate source region")]
    InvalidPolygon,
    #[error("extracted value requires source evidence")]
    MissingEvidence,
    #[error("document id is invalid")]
    InvalidDocumentId,
    #[error("document version must be a lowercase sha256 digest")]
    InvalidDocumentVersion,
    #[error("observation id is invalid")]
    InvalidObservationId,
    #[error("invalid initial job state")]
    InvalidInitialJobState,
    #[error("job cannot transition from {from:?} to {to:?}")]
    InvalidJobTransition { from: JobState, to: JobState },
    #[error("tenant id is invalid")]
    InvalidTenantId,
    #[error("product id is invalid")]
    InvalidProductId,
    #[error("job id is invalid")]
    InvalidJobId,
    #[error("idempotency key is invalid")]
    InvalidIdempotencyKey,
    #[error("request digest must be a lowercase sha256 digest")]
    InvalidRequestDigest,
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Copy, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Accepted,
    Inspecting,
    Processing,
    Validating,
    Cancelling,
    Cancelled,
    Rejected,
    Partial,
    ReviewRequired,
    Completed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "RawJobLifecycle")]
pub struct JobLifecycle {
    state: JobState,
}

#[derive(Deserialize)]
struct RawJobLifecycle {
    state: JobState,
}

impl TryFrom<RawJobLifecycle> for JobLifecycle {
    type Error = Error;

    fn try_from(value: RawJobLifecycle) -> Result<Self> {
        if value.state == JobState::Accepted {
            Ok(Self::new())
        } else {
            Err(Error::InvalidInitialJobState)
        }
    }
}

impl Default for JobLifecycle {
    fn default() -> Self {
        Self::new()
    }
}

impl JobLifecycle {
    pub fn new() -> Self {
        Self {
            state: JobState::Accepted,
        }
    }

    pub fn state(&self) -> JobState {
        self.state
    }

    pub fn transition_to(&mut self, next: JobState) -> Result<()> {
        if self.state == next || is_allowed_transition(self.state, next) {
            self.state = next;
            Ok(())
        } else {
            Err(Error::InvalidJobTransition {
                from: self.state,
                to: next,
            })
        }
    }

    pub fn request_cancellation(&mut self) -> Result<JobState> {
        match self.state {
            JobState::Accepted
            | JobState::Inspecting
            | JobState::Processing
            | JobState::Validating => {
                self.state = JobState::Cancelling;
                Ok(self.state)
            }
            JobState::Cancelling | JobState::Cancelled => Ok(self.state),
            _ => Err(Error::InvalidJobTransition {
                from: self.state,
                to: JobState::Cancelling,
            }),
        }
    }
}

fn is_allowed_transition(from: JobState, to: JobState) -> bool {
    matches!(
        (from, to),
        (JobState::Accepted, JobState::Inspecting)
            | (JobState::Inspecting, JobState::Rejected)
            | (JobState::Inspecting, JobState::Processing)
            | (JobState::Processing, JobState::Validating)
            | (JobState::Processing, JobState::Partial)
            | (JobState::Validating, JobState::Completed)
            | (JobState::Validating, JobState::ReviewRequired)
            | (JobState::Accepted, JobState::Cancelling)
            | (JobState::Inspecting, JobState::Cancelling)
            | (JobState::Processing, JobState::Cancelling)
            | (JobState::Validating, JobState::Cancelling)
            | (JobState::Cancelling, JobState::Cancelled)
            | (JobState::Partial, JobState::ReviewRequired)
    )
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct TenantId(String);

impl TenantId {
    pub fn new(value: &str) -> Result<Self> {
        validated_id(value, "ten_", Error::InvalidTenantId).map(Self)
    }
}

impl TryFrom<String> for TenantId {
    type Error = Error;

    fn try_from(value: String) -> Result<Self> {
        Self::new(&value)
    }
}

impl From<TenantId> for String {
    fn from(value: TenantId) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ProductId(String);

impl ProductId {
    pub fn new(value: &str) -> Result<Self> {
        let valid_length = !value.is_empty() && value.len() <= 63;
        let valid_edges = value
            .as_bytes()
            .first()
            .zip(value.as_bytes().last())
            .is_some_and(|(first, last)| {
                first.is_ascii_alphanumeric() && last.is_ascii_alphanumeric()
            });
        let valid_characters = value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-');

        if valid_length && valid_edges && valid_characters {
            Ok(Self(value.to_owned()))
        } else {
            Err(Error::InvalidProductId)
        }
    }
}

impl TryFrom<String> for ProductId {
    type Error = Error;

    fn try_from(value: String) -> Result<Self> {
        Self::new(&value)
    }
}

impl From<ProductId> for String {
    fn from(value: ProductId) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct JobId(String);

impl JobId {
    pub fn new(value: &str) -> Result<Self> {
        validated_id(value, "job_", Error::InvalidJobId).map(Self)
    }
}

impl TryFrom<String> for JobId {
    type Error = Error;

    fn try_from(value: String) -> Result<Self> {
        Self::new(&value)
    }
}

impl From<JobId> for String {
    fn from(value: JobId) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct IdempotencyKey(String);

impl IdempotencyKey {
    pub fn new(value: &str) -> Result<Self> {
        if !value.is_empty()
            && value.len() <= 128
            && value.bytes().all(|byte| byte.is_ascii_graphic())
        {
            Ok(Self(value.to_owned()))
        } else {
            Err(Error::InvalidIdempotencyKey)
        }
    }
}

impl TryFrom<String> for IdempotencyKey {
    type Error = Error;

    fn try_from(value: String) -> Result<Self> {
        Self::new(&value)
    }
}

impl From<IdempotencyKey> for String {
    fn from(value: IdempotencyKey) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct RequestDigest(String);

impl RequestDigest {
    pub fn new(value: &str) -> Result<Self> {
        if is_canonical_sha256(value) {
            Ok(Self(value.to_owned()))
        } else {
            Err(Error::InvalidRequestDigest)
        }
    }
}

impl TryFrom<String> for RequestDigest {
    type Error = Error;

    fn try_from(value: String) -> Result<Self> {
        Self::new(&value)
    }
}

impl From<RequestDigest> for String {
    fn from(value: RequestDigest) -> Self {
        value.0
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "f64", into = "f64")]
pub struct Confidence(f64);

impl Confidence {
    pub fn new(value: f64) -> Result<Self> {
        if value.is_finite() && (0.0..=1.0).contains(&value) {
            Ok(Self(value))
        } else {
            Err(Error::InvalidConfidence)
        }
    }
}

impl TryFrom<f64> for Confidence {
    type Error = Error;

    fn try_from(value: f64) -> Result<Self> {
        Self::new(value)
    }
}

impl From<Confidence> for f64 {
    fn from(value: Confidence) -> Self {
        value.0
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "u32", into = "u32")]
pub struct PageNumber(u32);

impl PageNumber {
    pub fn new(value: u32) -> Result<Self> {
        if value == 0 {
            Err(Error::InvalidPageNumber)
        } else {
            Ok(Self(value))
        }
    }
}

impl TryFrom<u32> for PageNumber {
    type Error = Error;

    fn try_from(value: u32) -> Result<Self> {
        Self::new(value)
    }
}

impl From<PageNumber> for u32 {
    fn from(value: PageNumber) -> Self {
        value.0
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "RawPoint")]
pub struct NormalizedPoint {
    pub x: f64,
    pub y: f64,
}

impl NormalizedPoint {
    pub fn new(x: f64, y: f64) -> Result<Self> {
        if x.is_finite() && y.is_finite() && (0.0..=1.0).contains(&x) && (0.0..=1.0).contains(&y) {
            Ok(Self { x, y })
        } else {
            Err(Error::InvalidPoint)
        }
    }
}

#[derive(Deserialize)]
struct RawPoint {
    x: f64,
    y: f64,
}

impl TryFrom<RawPoint> for NormalizedPoint {
    type Error = Error;

    fn try_from(value: RawPoint) -> Result<Self> {
        Self::new(value.x, value.y)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "RawPolygon")]
pub struct Polygon {
    pub points: Vec<NormalizedPoint>,
}

#[derive(Deserialize)]
struct RawPolygon {
    points: Vec<NormalizedPoint>,
}

impl TryFrom<RawPolygon> for Polygon {
    type Error = Error;

    fn try_from(value: RawPolygon) -> Result<Self> {
        Self::new(value.points)
    }
}

impl Polygon {
    pub fn new(points: Vec<NormalizedPoint>) -> Result<Self> {
        if points.len() < 3 || signed_double_area(&points).abs() <= f64::EPSILON {
            Err(Error::InvalidPolygon)
        } else {
            Ok(Self { points })
        }
    }
}

fn signed_double_area(points: &[NormalizedPoint]) -> f64 {
    points
        .iter()
        .zip(points.iter().cycle().skip(1))
        .take(points.len())
        .map(|(left, right)| left.x * right.y - right.x * left.y)
        .sum()
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct DocumentId(String);

impl DocumentId {
    pub fn new(value: &str) -> Result<Self> {
        validated_id(value, "doc_", Error::InvalidDocumentId).map(Self)
    }
}

impl TryFrom<String> for DocumentId {
    type Error = Error;

    fn try_from(value: String) -> Result<Self> {
        Self::new(&value)
    }
}

impl From<DocumentId> for String {
    fn from(value: DocumentId) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct DocumentVersion(String);

impl DocumentVersion {
    pub fn new(value: &str) -> Result<Self> {
        if is_canonical_sha256(value) {
            Ok(Self(value.to_owned()))
        } else {
            Err(Error::InvalidDocumentVersion)
        }
    }
}

impl TryFrom<String> for DocumentVersion {
    type Error = Error;

    fn try_from(value: String) -> Result<Self> {
        Self::new(&value)
    }
}

impl From<DocumentVersion> for String {
    fn from(value: DocumentVersion) -> Self {
        value.0
    }
}

fn is_canonical_sha256(value: &str) -> bool {
    let digest = value.strip_prefix("sha256:").unwrap_or_default();
    digest.len() == 64
        && digest
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ObservationId(String);

impl TryFrom<&str> for ObservationId {
    type Error = Error;

    fn try_from(value: &str) -> Result<Self> {
        validated_id(value, "obs_", Error::InvalidObservationId).map(Self)
    }
}

impl TryFrom<String> for ObservationId {
    type Error = Error;

    fn try_from(value: String) -> Result<Self> {
        Self::try_from(value.as_str())
    }
}

impl From<ObservationId> for String {
    fn from(value: ObservationId) -> Self {
        value.0
    }
}

fn validated_id(value: &str, prefix: &str, error: Error) -> Result<String> {
    let suffix = value.strip_prefix(prefix).unwrap_or_default();
    if !suffix.is_empty()
        && suffix.len() <= 64
        && suffix
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    {
        Ok(value.to_owned())
    } else {
        Err(error)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Evidence {
    pub page: PageNumber,
    pub polygon: Polygon,
    pub observation_id: ObservationId,
}

impl Evidence {
    pub fn new(page: PageNumber, polygon: Polygon, observation_id: ObservationId) -> Self {
        Self {
            page,
            polygon,
            observation_id,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "RawExtractedValue")]
pub struct ExtractedValue {
    pub value: Value,
    pub confidence: Confidence,
    pub evidence: Vec<Evidence>,
}

#[derive(Deserialize)]
struct RawExtractedValue {
    value: Value,
    confidence: Confidence,
    evidence: Vec<Evidence>,
}

impl TryFrom<RawExtractedValue> for ExtractedValue {
    type Error = Error;

    fn try_from(value: RawExtractedValue) -> Result<Self> {
        Self::new(value.value, value.confidence, value.evidence)
    }
}

impl ExtractedValue {
    pub fn new(value: Value, confidence: Confidence, evidence: Vec<Evidence>) -> Result<Self> {
        if evidence.is_empty() {
            Err(Error::MissingEvidence)
        } else {
            Ok(Self {
                value,
                confidence,
                evidence,
            })
        }
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ContentTrust {
    Untrusted,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DocumentResult {
    schema_version: &'static str,
    pub document_id: DocumentId,
    pub document_version: DocumentVersion,
    content_trust: ContentTrust,
    pub fields: BTreeMap<String, ExtractedValue>,
}

impl DocumentResult {
    pub fn new(
        document_id: DocumentId,
        document_version: DocumentVersion,
        fields: BTreeMap<String, ExtractedValue>,
    ) -> Self {
        Self {
            schema_version: RESULT_SCHEMA_VERSION,
            document_id,
            document_version,
            content_trust: ContentTrust::Untrusted,
            fields,
        }
    }
}
