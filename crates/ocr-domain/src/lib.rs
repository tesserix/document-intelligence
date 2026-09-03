//! Provider-neutral document intelligence contracts.

mod page_workflow;

pub use page_workflow::{
    PageTask, PageWorkflow, PageWorkflowStatus, MAXIMUM_PAGE_ATTEMPTS, MAXIMUM_PAGE_COUNT,
};

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
    #[error("upload id is invalid")]
    InvalidUploadId,
    #[error("webhook subscription id is invalid")]
    InvalidWebhookSubscriptionId,
    #[error("idempotency key is invalid")]
    InvalidIdempotencyKey,
    #[error("request digest must be a lowercase sha256 digest")]
    InvalidRequestDigest,
    #[error("result schema version is unsupported")]
    UnsupportedResultSchemaVersion,
    #[error("stable code is invalid")]
    InvalidStableCode,
    #[error("table id is invalid")]
    InvalidTableId,
    #[error("table must contain bounded cells")]
    InvalidTable,
    #[error("document content requires source evidence")]
    MissingDocumentEvidence,
    #[error("extracted field name is invalid")]
    InvalidFieldName,
    #[error("processing provenance is invalid")]
    InvalidProcessingProvenance,
    #[error("cost is invalid")]
    InvalidCost,
    #[error("text observation is invalid")]
    InvalidTextObservation,
    #[error("document page is invalid")]
    InvalidDocumentPage,
    #[error("page workflow is invalid")]
    InvalidPageWorkflow,
    #[error("page task does not match active workflow state")]
    StalePageTask,
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

    pub fn as_str(&self) -> &str {
        &self.0
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

    pub fn as_str(&self) -> &str {
        &self.0
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

    pub fn as_str(&self) -> &str {
        &self.0
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

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct UploadId(String);

impl UploadId {
    pub fn new(value: &str) -> Result<Self> {
        validated_id(value, "upl_", Error::InvalidUploadId).map(Self)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for UploadId {
    type Error = Error;

    fn try_from(value: String) -> Result<Self> {
        Self::new(&value)
    }
}

impl From<UploadId> for String {
    fn from(value: UploadId) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct WebhookSubscriptionId(String);

impl WebhookSubscriptionId {
    pub fn new(value: &str) -> Result<Self> {
        validated_id(value, "whs_", Error::InvalidWebhookSubscriptionId).map(Self)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for WebhookSubscriptionId {
    type Error = Error;

    fn try_from(value: String) -> Result<Self> {
        Self::new(&value)
    }
}

impl From<WebhookSubscriptionId> for String {
    fn from(value: WebhookSubscriptionId) -> Self {
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

    pub fn as_str(&self) -> &str {
        &self.0
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

    pub fn as_str(&self) -> &str {
        &self.0
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

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
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

#[derive(Debug, Copy, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservationLevel {
    Page,
    Paragraph,
    Line,
    Word,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "RawTextObservation")]
pub struct TextObservation {
    pub observation_id: ObservationId,
    pub level: ObservationLevel,
    pub text: String,
    pub confidence: Confidence,
    pub polygon: Polygon,
    pub reading_order: u32,
    pub parent_observation_id: Option<ObservationId>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawTextObservation {
    observation_id: ObservationId,
    level: ObservationLevel,
    text: String,
    confidence: Confidence,
    polygon: Polygon,
    reading_order: u32,
    parent_observation_id: Option<ObservationId>,
}

impl TryFrom<RawTextObservation> for TextObservation {
    type Error = Error;

    fn try_from(value: RawTextObservation) -> Result<Self> {
        Self::new(
            value.observation_id,
            value.level,
            value.text,
            value.confidence,
            value.polygon,
            value.reading_order,
            value.parent_observation_id,
        )
    }
}

impl TextObservation {
    pub fn new(
        observation_id: ObservationId,
        level: ObservationLevel,
        text: impl Into<String>,
        confidence: Confidence,
        polygon: Polygon,
        reading_order: u32,
        parent_observation_id: Option<ObservationId>,
    ) -> Result<Self> {
        let text = text.into();
        if text.trim().is_empty() || text.len() > 65_536 {
            return Err(Error::InvalidTextObservation);
        }
        if parent_observation_id.as_ref() == Some(&observation_id) {
            return Err(Error::InvalidTextObservation);
        }
        Ok(Self {
            observation_id,
            level,
            text,
            confidence,
            polygon,
            reading_order,
            parent_observation_id,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "RawDocumentPage")]
pub struct DocumentPage {
    pub page: PageNumber,
    pub width: u32,
    pub height: u32,
    pub observations: Vec<TextObservation>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawDocumentPage {
    page: PageNumber,
    width: u32,
    height: u32,
    observations: Vec<TextObservation>,
}

impl TryFrom<RawDocumentPage> for DocumentPage {
    type Error = Error;

    fn try_from(value: RawDocumentPage) -> Result<Self> {
        Self::new(value.page, value.width, value.height, value.observations)
    }
}

impl DocumentPage {
    pub fn new(
        page: PageNumber,
        width: u32,
        height: u32,
        mut observations: Vec<TextObservation>,
    ) -> Result<Self> {
        if width == 0 || height == 0 || observations.len() > 100_000 {
            return Err(Error::InvalidDocumentPage);
        }
        observations.sort_by_key(|observation| observation.reading_order);
        let mut seen_ids = std::collections::BTreeSet::new();
        let mut seen_orders = std::collections::BTreeSet::new();
        for observation in &observations {
            if !seen_orders.insert(observation.reading_order)
                || observation
                    .parent_observation_id
                    .as_ref()
                    .is_some_and(|parent| !seen_ids.contains(parent))
                || !seen_ids.insert(observation.observation_id.clone())
            {
                return Err(Error::InvalidDocumentPage);
            }
        }
        Ok(Self {
            page,
            width,
            height,
            observations,
        })
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

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct StableCode(String);

impl StableCode {
    pub fn new(value: &str) -> Result<Self> {
        let mut bytes = value.bytes();
        let valid = (1..=64).contains(&value.len())
            && bytes.next().is_some_and(|byte| byte.is_ascii_lowercase())
            && bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_');
        if valid {
            Ok(Self(value.to_owned()))
        } else {
            Err(Error::InvalidStableCode)
        }
    }
}

impl TryFrom<String> for StableCode {
    type Error = Error;

    fn try_from(value: String) -> Result<Self> {
        Self::new(&value)
    }
}

impl From<StableCode> for String {
    fn from(value: StableCode) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct TableId(String);

impl TableId {
    pub fn new(value: &str) -> Result<Self> {
        validated_id(value, "tbl_", Error::InvalidTableId).map(Self)
    }
}

impl TryFrom<String> for TableId {
    type Error = Error;

    fn try_from(value: String) -> Result<Self> {
        Self::new(&value)
    }
}

impl From<TableId> for String {
    fn from(value: TableId) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "RawTableCell")]
pub struct TableCell {
    pub row: u32,
    pub column: u32,
    pub text: String,
    pub confidence: Confidence,
    pub evidence: Vec<Evidence>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawTableCell {
    row: u32,
    column: u32,
    text: String,
    confidence: Confidence,
    evidence: Vec<Evidence>,
}

impl TryFrom<RawTableCell> for TableCell {
    type Error = Error;

    fn try_from(value: RawTableCell) -> Result<Self> {
        Self::new(
            value.row,
            value.column,
            value.text,
            value.confidence,
            value.evidence,
        )
    }
}

impl TableCell {
    pub fn new(
        row: u32,
        column: u32,
        text: impl Into<String>,
        confidence: Confidence,
        evidence: Vec<Evidence>,
    ) -> Result<Self> {
        if evidence.is_empty() {
            return Err(Error::MissingEvidence);
        }
        Ok(Self {
            row,
            column,
            text: text.into(),
            confidence,
            evidence,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "RawDocumentTable")]
pub struct DocumentTable {
    pub table_id: TableId,
    pub cells: Vec<TableCell>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawDocumentTable {
    table_id: TableId,
    cells: Vec<TableCell>,
}

impl TryFrom<RawDocumentTable> for DocumentTable {
    type Error = Error;

    fn try_from(value: RawDocumentTable) -> Result<Self> {
        Self::new(value.table_id, value.cells)
    }
}

impl DocumentTable {
    pub fn new(table_id: TableId, cells: Vec<TableCell>) -> Result<Self> {
        if cells.is_empty() || cells.len() > 10_000 {
            return Err(Error::InvalidTable);
        }
        Ok(Self { table_id, cells })
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConfidenceDimensions {
    pub input_quality: Confidence,
    pub ocr: Confidence,
    pub classification: Confidence,
    pub extraction: Confidence,
    pub validation: Confidence,
    pub overall: Confidence,
}

impl ConfidenceDimensions {
    pub fn new(
        input_quality: f64,
        ocr: f64,
        classification: f64,
        extraction: f64,
        validation: f64,
        overall: f64,
    ) -> Result<Self> {
        Ok(Self {
            input_quality: Confidence::new(input_quality)?,
            ocr: Confidence::new(ocr)?,
            classification: Confidence::new(classification)?,
            extraction: Confidence::new(extraction)?,
            validation: Confidence::new(validation)?,
            overall: Confidence::new(overall)?,
        })
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValidationSeverity {
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidationFailure {
    pub code: StableCode,
    pub severity: ValidationSeverity,
}

impl ValidationFailure {
    pub fn new(code: StableCode, severity: ValidationSeverity) -> Self {
        Self { code, severity }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "RawProcessingProvenance")]
pub struct ProcessingProvenance {
    pub provider: String,
    pub model_version: String,
    pub processing_profile_version: String,
    pub duration_ms: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawProcessingProvenance {
    provider: String,
    model_version: String,
    processing_profile_version: String,
    duration_ms: u64,
}

impl TryFrom<RawProcessingProvenance> for ProcessingProvenance {
    type Error = Error;

    fn try_from(value: RawProcessingProvenance) -> Result<Self> {
        Self::new(
            value.provider,
            value.model_version,
            value.processing_profile_version,
            value.duration_ms,
        )
    }
}

impl ProcessingProvenance {
    pub fn new(
        provider: impl Into<String>,
        model_version: impl Into<String>,
        processing_profile_version: impl Into<String>,
        duration_ms: u64,
    ) -> Result<Self> {
        let provider = provider.into();
        let model_version = model_version.into();
        let processing_profile_version = processing_profile_version.into();
        if !valid_version_name(&provider)
            || !valid_version_name(&model_version)
            || !valid_version_name(&processing_profile_version)
        {
            return Err(Error::InvalidProcessingProvenance);
        }
        Ok(Self {
            provider,
            model_version,
            processing_profile_version,
            duration_ms,
        })
    }
}

fn valid_version_name(value: &str) -> bool {
    (1..=128).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        && value
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "RawCost")]
pub struct Cost {
    pub currency: String,
    pub decimal: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCost {
    currency: String,
    decimal: String,
}

impl TryFrom<RawCost> for Cost {
    type Error = Error;

    fn try_from(value: RawCost) -> Result<Self> {
        Self::new(value.currency, value.decimal)
    }
}

impl Cost {
    pub fn new(currency: impl Into<String>, decimal: impl Into<String>) -> Result<Self> {
        let currency = currency.into();
        let decimal = decimal.into();
        let valid_currency =
            currency.len() == 3 && currency.bytes().all(|byte| byte.is_ascii_uppercase());
        let mut parts = decimal.split('.');
        let whole = parts.next().unwrap_or_default();
        let fraction = parts.next();
        let valid_decimal = !whole.is_empty()
            && whole.bytes().all(|byte| byte.is_ascii_digit())
            && fraction.is_none_or(|part| {
                !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit())
            })
            && parts.next().is_none();
        if !valid_currency || !valid_decimal {
            return Err(Error::InvalidCost);
        }
        Ok(Self { currency, decimal })
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct DocumentResultPayload {
    pub text: String,
    pub markdown: String,
    pub pages: Vec<DocumentPage>,
    pub fields: BTreeMap<String, ExtractedValue>,
    pub tables: Vec<DocumentTable>,
    pub confidence: Option<ConfidenceDimensions>,
    pub citations: Vec<Evidence>,
    pub warnings: Vec<StableCode>,
    pub validation_failures: Vec<ValidationFailure>,
    pub provenance: Option<ProcessingProvenance>,
    pub cost: Option<Cost>,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ContentTrust {
    Untrusted,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "RawDocumentResult")]
pub struct DocumentResult {
    schema_version: String,
    pub document_id: DocumentId,
    pub document_version: DocumentVersion,
    content_trust: ContentTrust,
    pub text: String,
    pub markdown: String,
    pub pages: Vec<DocumentPage>,
    pub fields: BTreeMap<String, ExtractedValue>,
    pub tables: Vec<DocumentTable>,
    pub confidence: Option<ConfidenceDimensions>,
    pub citations: Vec<Evidence>,
    pub warnings: Vec<StableCode>,
    pub validation_failures: Vec<ValidationFailure>,
    pub provider: Option<String>,
    pub model_version: Option<String>,
    pub processing_profile_version: Option<String>,
    pub duration_ms: Option<u64>,
    pub cost: Option<Cost>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawDocumentResult {
    schema_version: String,
    document_id: DocumentId,
    document_version: DocumentVersion,
    content_trust: ContentTrust,
    #[serde(default)]
    text: String,
    #[serde(default)]
    markdown: String,
    #[serde(default)]
    pages: Vec<DocumentPage>,
    #[serde(default)]
    fields: BTreeMap<String, ExtractedValue>,
    #[serde(default)]
    tables: Vec<DocumentTable>,
    confidence: Option<ConfidenceDimensions>,
    #[serde(default)]
    citations: Vec<Evidence>,
    #[serde(default)]
    warnings: Vec<StableCode>,
    #[serde(default)]
    validation_failures: Vec<ValidationFailure>,
    provider: Option<String>,
    model_version: Option<String>,
    processing_profile_version: Option<String>,
    duration_ms: Option<u64>,
    cost: Option<Cost>,
}

impl TryFrom<RawDocumentResult> for DocumentResult {
    type Error = Error;

    fn try_from(value: RawDocumentResult) -> Result<Self> {
        if value.schema_version != RESULT_SCHEMA_VERSION {
            return Err(Error::UnsupportedResultSchemaVersion);
        }
        let ContentTrust::Untrusted = value.content_trust;
        let provenance = match (
            value.provider,
            value.model_version,
            value.processing_profile_version,
            value.duration_ms,
        ) {
            (None, None, None, None) => None,
            (Some(provider), Some(model), Some(profile), Some(duration_ms)) => Some(
                ProcessingProvenance::new(provider, model, profile, duration_ms)?,
            ),
            _ => return Err(Error::InvalidProcessingProvenance),
        };
        Self::new(
            value.document_id,
            value.document_version,
            DocumentResultPayload {
                text: value.text,
                markdown: value.markdown,
                pages: value.pages,
                fields: value.fields,
                tables: value.tables,
                confidence: value.confidence,
                citations: value.citations,
                warnings: value.warnings,
                validation_failures: value.validation_failures,
                provenance,
                cost: value.cost,
            },
        )
    }
}

impl DocumentResult {
    pub fn new(
        document_id: DocumentId,
        document_version: DocumentVersion,
        mut payload: DocumentResultPayload,
    ) -> Result<Self> {
        if (!payload.text.is_empty() || !payload.markdown.is_empty())
            && payload.citations.is_empty()
        {
            return Err(Error::MissingDocumentEvidence);
        }
        if payload
            .fields
            .keys()
            .any(|name| name.is_empty() || name.len() > 128)
        {
            return Err(Error::InvalidFieldName);
        }
        if payload.pages.len() > 300 {
            return Err(Error::InvalidDocumentPage);
        }
        payload.pages.sort_by_key(|page| u32::from(page.page));
        if payload
            .pages
            .windows(2)
            .any(|pages| pages[0].page == pages[1].page)
        {
            return Err(Error::InvalidDocumentPage);
        }
        let provenance = payload.provenance;
        Ok(Self {
            schema_version: RESULT_SCHEMA_VERSION.to_owned(),
            document_id,
            document_version,
            content_trust: ContentTrust::Untrusted,
            text: payload.text,
            markdown: payload.markdown,
            pages: payload.pages,
            fields: payload.fields,
            tables: payload.tables,
            confidence: payload.confidence,
            citations: payload.citations,
            warnings: payload.warnings,
            validation_failures: payload.validation_failures,
            provider: provenance.as_ref().map(|value| value.provider.clone()),
            model_version: provenance.as_ref().map(|value| value.model_version.clone()),
            processing_profile_version: provenance
                .as_ref()
                .map(|value| value.processing_profile_version.clone()),
            duration_ms: provenance.map(|value| value.duration_ms),
            cost: payload.cost,
        })
    }
}
