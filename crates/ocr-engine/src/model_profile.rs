use std::collections::BTreeMap;

use ed25519_dalek::{Signature, VerifyingKey};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::{Error, Result};

const MAXIMUM_MODEL_PROFILE_BYTES: usize = 64 * 1024;
const MAXIMUM_TENSORS: usize = 16;
const MAXIMUM_TENSOR_RANK: usize = 8;
const MAXIMUM_TENSOR_DIMENSION: u32 = 65_536;
const MAXIMUM_TENSOR_BYTES: u64 = 64 * 1024 * 1024;
const MAXIMUM_PROFILE_TENSOR_BYTES: u64 = 128 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactLimits {
    maximum_bytes: usize,
}

impl ArtifactLimits {
    pub fn new(maximum_bytes: usize) -> Result<Self> {
        if maximum_bytes == 0 {
            return Err(Error::InvalidLimit);
        }
        Ok(Self { maximum_bytes })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustedModelKey {
    id: String,
    key: VerifyingKey,
}

impl TrustedModelKey {
    pub fn new(id: impl Into<String>, public_key: [u8; 32]) -> Result<Self> {
        let id = id.into();
        if !valid_identifier(&id) {
            return Err(Error::InvalidTrustedModelKey);
        }
        let key =
            VerifyingKey::from_bytes(&public_key).map_err(|_| Error::InvalidTrustedModelKey)?;
        Ok(Self { id, key })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelProfileVerifier {
    keys: BTreeMap<String, VerifyingKey>,
}

impl ModelProfileVerifier {
    pub fn new(keys: Vec<TrustedModelKey>) -> Result<Self> {
        if keys.is_empty() {
            return Err(Error::InvalidTrustedModelKey);
        }
        let mut trusted = BTreeMap::new();
        for key in keys {
            if trusted.insert(key.id, key.key).is_some() {
                return Err(Error::InvalidTrustedModelKey);
            }
        }
        Ok(Self { keys: trusted })
    }

    pub fn verify(
        &self,
        key_id: &str,
        manifest_bytes: &[u8],
        signature_bytes: &[u8; 64],
    ) -> Result<VerifiedModelProfile> {
        if manifest_bytes.len() > MAXIMUM_MODEL_PROFILE_BYTES {
            return Err(Error::ModelProfileByteLimitExceeded {
                bytes: manifest_bytes.len(),
                limit: MAXIMUM_MODEL_PROFILE_BYTES,
            });
        }
        let key = self.keys.get(key_id).ok_or(Error::UnknownModelSigningKey)?;
        let signature = Signature::from_bytes(signature_bytes);
        key.verify_strict(manifest_bytes, &signature)
            .map_err(|_| Error::InvalidModelSignature)?;
        let raw = serde_json::from_slice::<RawModelProfile>(manifest_bytes)
            .map_err(|_| Error::InvalidModelProfile)?;
        let profile = ModelProfile::try_from(raw)?;
        Ok(VerifiedModelProfile {
            profile,
            profile_digest: sha256(manifest_bytes),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedModelProfile {
    profile: ModelProfile,
    profile_digest: String,
}

impl VerifiedModelProfile {
    pub fn profile_id(&self) -> &str {
        &self.profile.profile_id
    }

    pub fn profile_digest(&self) -> &str {
        &self.profile_digest
    }

    pub fn stage(&self) -> ModelStage {
        self.profile.stage
    }

    pub fn model_version(&self) -> &str {
        &self.profile.model_version
    }

    pub fn license_id(&self) -> &str {
        &self.profile.license_id
    }

    pub fn dataset_version(&self) -> &str {
        &self.profile.dataset_version
    }

    pub fn calibration_version(&self) -> &str {
        &self.profile.calibration_version
    }

    pub fn execution_profile(&self) -> ExecutionProfile {
        self.profile.execution_profile
    }

    pub fn runtime_contract_version(&self) -> &str {
        &self.profile.runtime_contract_version
    }

    pub fn supported_locales(&self) -> &[String] {
        &self.profile.supported_locales
    }

    pub fn rollback_predecessor(&self) -> Option<&str> {
        self.profile.rollback_predecessor.as_deref()
    }

    pub fn inputs(&self) -> &[TensorContract] {
        &self.profile.inputs
    }

    pub fn outputs(&self) -> &[TensorContract] {
        &self.profile.outputs
    }

    pub fn verify_artifact(&self, artifact: &[u8], limits: ArtifactLimits) -> Result<()> {
        if artifact.len() > limits.maximum_bytes {
            return Err(Error::ModelArtifactByteLimitExceeded {
                bytes: artifact.len(),
                limit: limits.maximum_bytes,
            });
        }
        if sha256(artifact) == self.profile.artifact_digest {
            Ok(())
        } else {
            Err(Error::ModelArtifactDigestMismatch)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ModelProfile {
    profile_id: String,
    stage: ModelStage,
    model_version: String,
    artifact_digest: String,
    license_id: String,
    dataset_version: String,
    calibration_version: String,
    execution_profile: ExecutionProfile,
    runtime_contract_version: String,
    supported_locales: Vec<String>,
    rollback_predecessor: Option<String>,
    inputs: Vec<TensorContract>,
    outputs: Vec<TensorContract>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawModelProfile {
    schema_version: u16,
    profile_id: String,
    stage: ModelStage,
    model_version: String,
    artifact_digest: String,
    license_id: String,
    dataset_version: String,
    calibration_version: String,
    execution_profile: ExecutionProfile,
    runtime_contract_version: String,
    supported_locales: Vec<String>,
    rollback_predecessor: Option<String>,
    inputs: Vec<RawTensorContract>,
    outputs: Vec<RawTensorContract>,
}

impl TryFrom<RawModelProfile> for ModelProfile {
    type Error = Error;

    fn try_from(value: RawModelProfile) -> Result<Self> {
        if value.schema_version != 1
            || !valid_identifier(&value.profile_id)
            || !valid_semantic_version(&value.model_version)
            || !valid_sha256(&value.artifact_digest)
            || !valid_license_id(&value.license_id)
            || !valid_identifier(&value.dataset_version)
            || !valid_identifier(&value.calibration_version)
            || !valid_semantic_version(&value.runtime_contract_version)
            || value
                .rollback_predecessor
                .as_deref()
                .is_some_and(|digest| !valid_sha256(digest) || digest == value.artifact_digest)
        {
            return Err(Error::InvalidModelProfile);
        }

        let inputs = validate_tensors(value.inputs)?;
        let outputs = validate_tensors(value.outputs)?;
        validate_tensor_envelope(&inputs, &outputs)?;
        let supported_locales = validate_locales(value.supported_locales)?;
        if inputs
            .iter()
            .map(|tensor| tensor.name.as_str())
            .any(|input| outputs.iter().any(|output| output.name == input))
        {
            return Err(Error::InvalidModelProfile);
        }

        Ok(Self {
            profile_id: value.profile_id,
            stage: value.stage,
            model_version: value.model_version,
            artifact_digest: value.artifact_digest,
            license_id: value.license_id,
            dataset_version: value.dataset_version,
            calibration_version: value.calibration_version,
            execution_profile: value.execution_profile,
            runtime_contract_version: value.runtime_contract_version,
            supported_locales,
            rollback_predecessor: value.rollback_predecessor,
            inputs,
            outputs,
        })
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelStage {
    Detector,
    Recognizer,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionProfile {
    Cpu,
    Cuda,
    TensorRt,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TensorContract {
    name: String,
    data_type: TensorDataType,
    dimensions: Vec<TensorDimension>,
}

impl TensorContract {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn data_type(&self) -> TensorDataType {
        self.data_type
    }

    pub fn dimensions(&self) -> &[TensorDimension] {
        &self.dimensions
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawTensorContract {
    name: String,
    data_type: TensorDataType,
    dimensions: Vec<TensorDimension>,
}

impl TryFrom<RawTensorContract> for TensorContract {
    type Error = Error;

    fn try_from(value: RawTensorContract) -> Result<Self> {
        if !valid_tensor_name(&value.name)
            || value.dimensions.is_empty()
            || value.dimensions.len() > MAXIMUM_TENSOR_RANK
            || value.dimensions.iter().any(TensorDimension::is_invalid)
        {
            return Err(Error::InvalidModelProfile);
        }
        Ok(Self {
            name: value.name,
            data_type: value.data_type,
            dimensions: value.dimensions,
        })
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TensorDataType {
    Float32,
    Int64,
}

impl TensorDataType {
    fn byte_width(self) -> u64 {
        match self {
            Self::Float32 => 4,
            Self::Int64 => 8,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum TensorDimension {
    Fixed { value: u32 },
    Dynamic { maximum: u32 },
}

impl TensorDimension {
    fn maximum(&self) -> u64 {
        match self {
            Self::Fixed { value } => u64::from(*value),
            Self::Dynamic { maximum } => u64::from(*maximum),
        }
    }

    fn is_invalid(&self) -> bool {
        match self {
            Self::Fixed { value } => *value == 0 || *value > MAXIMUM_TENSOR_DIMENSION,
            Self::Dynamic { maximum } => *maximum == 0 || *maximum > MAXIMUM_TENSOR_DIMENSION,
        }
    }
}

fn validate_tensors(values: Vec<RawTensorContract>) -> Result<Vec<TensorContract>> {
    if values.is_empty() || values.len() > MAXIMUM_TENSORS {
        return Err(Error::InvalidModelProfile);
    }
    let tensors = values
        .into_iter()
        .map(TensorContract::try_from)
        .collect::<Result<Vec<_>>>()?;
    let mut names = tensors
        .iter()
        .map(|tensor| tensor.name.as_str())
        .collect::<Vec<_>>();
    names.sort_unstable();
    if names.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(Error::InvalidModelProfile);
    }
    Ok(tensors)
}

fn validate_tensor_envelope(inputs: &[TensorContract], outputs: &[TensorContract]) -> Result<()> {
    let mut total_bytes = 0_u64;
    for tensor in inputs.iter().chain(outputs) {
        let elements = tensor
            .dimensions
            .iter()
            .map(TensorDimension::maximum)
            .try_fold(1_u64, |total, dimension| total.checked_mul(dimension))
            .ok_or(Error::InvalidModelProfile)?;
        let bytes = elements
            .checked_mul(tensor.data_type.byte_width())
            .ok_or(Error::InvalidModelProfile)?;
        if bytes > MAXIMUM_TENSOR_BYTES {
            return Err(Error::InvalidModelProfile);
        }
        total_bytes = total_bytes
            .checked_add(bytes)
            .ok_or(Error::InvalidModelProfile)?;
        if total_bytes > MAXIMUM_PROFILE_TENSOR_BYTES {
            return Err(Error::InvalidModelProfile);
        }
    }
    Ok(())
}

fn validate_locales(values: Vec<String>) -> Result<Vec<String>> {
    if values.is_empty()
        || values.len() > MAXIMUM_TENSORS
        || values.iter().any(|value| !valid_locale(value))
    {
        return Err(Error::InvalidModelProfile);
    }
    let mut unique = values;
    unique.sort_unstable();
    if unique.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(Error::InvalidModelProfile);
    }
    Ok(unique)
}

fn sha256(value: &[u8]) -> String {
    const HEX_DIGITS: &[u8; 16] = b"0123456789abcdef";

    let mut encoded = String::with_capacity(71);
    encoded.push_str("sha256:");
    for byte in Sha256::digest(value) {
        encoded.push(HEX_DIGITS[(byte >> 4) as usize] as char);
        encoded.push(HEX_DIGITS[(byte & 0x0f) as usize] as char);
    }
    encoded
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 71
        && value.starts_with("sha256:")
        && value[7..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn valid_identifier(value: &str) -> bool {
    (1..=64).contains(&value.len())
        && value.bytes().enumerate().all(|(index, byte)| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || (index > 0 && matches!(byte, b'.' | b'_' | b'-'))
        })
        && value
            .as_bytes()
            .last()
            .is_some_and(u8::is_ascii_alphanumeric)
}

fn valid_semantic_version(value: &str) -> bool {
    let parts = value.split('.').collect::<Vec<_>>();
    parts.len() == 3
        && parts.iter().all(|part| {
            !part.is_empty()
                && part.len() <= 5
                && part.bytes().all(|byte| byte.is_ascii_digit())
                && (part == &"0" || !part.starts_with('0'))
        })
}

fn valid_license_id(value: &str) -> bool {
    (1..=128).contains(&value.len())
        && value.trim() == value
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'+' | b'(' | b')' | b' ')
        })
}

fn valid_locale(value: &str) -> bool {
    (2..=32).contains(&value.len())
        && value
            .bytes()
            .enumerate()
            .all(|(index, byte)| byte.is_ascii_alphabetic() || byte == b'-' && index > 0)
        && value.as_bytes().last().is_some_and(u8::is_ascii_alphabetic)
}

fn valid_tensor_name(value: &str) -> bool {
    (1..=64).contains(&value.len())
        && value
            .bytes()
            .enumerate()
            .all(|(index, byte)| byte.is_ascii_alphanumeric() || byte == b'_' && index > 0)
        && value
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphabetic)
}
