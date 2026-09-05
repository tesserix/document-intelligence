use std::{collections::HashMap, env, time::Duration};

use axum::{
    extract::Request,
    http::{HeaderMap, Method, Uri},
    middleware::{from_fn_with_state, Next},
    response::{IntoResponse, Response},
    Router,
};
use hmac::{Hmac, KeyInit, Mac};
use ocr_domain::{ProductId, TenantId};
use sha2::Sha256;
use thiserror::Error;
use time::OffsetDateTime;

use crate::{request_id_from_headers, ApiError, TrustedIdentity};

const KEY_ID_HEADER: &str = "x-ocr-key-id";
const TENANT_ID_HEADER: &str = "x-ocr-tenant-id";
const TIMESTAMP_HEADER: &str = "x-ocr-timestamp";
const SIGNATURE_HEADER: &str = "x-ocr-signature";
const MINIMUM_KEY_BYTES: usize = 32;
const WORKLOAD_IDENTITY_KEYS: &str = "OCR_WORKLOAD_IDENTITY_KEYS";
const WORKLOAD_IDENTITY_CLOCK_SKEW_SECONDS: &str = "OCR_WORKLOAD_IDENTITY_CLOCK_SKEW_SECONDS";

type HmacSha256 = Hmac<Sha256>;

#[derive(Debug, Copy, Clone, Error, PartialEq, Eq)]
pub enum WorkloadIdentityConfigurationError {
    #[error("workload identity key set is empty")]
    EmptyKeySet,
    #[error("workload identity key identifier is invalid")]
    InvalidKeyId,
    #[error("workload identity product is invalid")]
    InvalidProduct,
    #[error("workload identity key is too short")]
    KeyTooShort,
    #[error("workload identity key identifier is duplicated")]
    DuplicateKeyId,
    #[error("workload identity key encoding is invalid")]
    InvalidKeyEncoding,
    #[error("workload identity clock skew is invalid")]
    InvalidClockSkew,
    #[error("workload identity keys are missing")]
    MissingKeys,
    #[error("workload identity configuration contains invalid UTF-8")]
    InvalidEncoding,
}

#[derive(Clone)]
struct SigningKey {
    product_id: ProductId,
    secret: Vec<u8>,
}

impl std::fmt::Debug for SigningKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SigningKey")
            .field("product_id", &self.product_id)
            .field("secret", &"[redacted]")
            .finish()
    }
}

#[derive(Debug, Clone)]
pub struct WorkloadIdentityVerifier {
    keys: HashMap<String, SigningKey>,
    maximum_clock_skew: Duration,
}

impl WorkloadIdentityVerifier {
    pub fn from_process_environment() -> Result<Self, WorkloadIdentityConfigurationError> {
        let keys = env::var(WORKLOAD_IDENTITY_KEYS).map_err(|error| match error {
            env::VarError::NotPresent => WorkloadIdentityConfigurationError::MissingKeys,
            env::VarError::NotUnicode(_) => WorkloadIdentityConfigurationError::InvalidEncoding,
        })?;
        let maximum_clock_skew = match env::var(WORKLOAD_IDENTITY_CLOCK_SKEW_SECONDS) {
            Ok(value) => value
                .parse::<u64>()
                .ok()
                .filter(|seconds| (1..=300).contains(seconds))
                .map(Duration::from_secs)
                .ok_or(WorkloadIdentityConfigurationError::InvalidClockSkew)?,
            Err(env::VarError::NotPresent) => Duration::from_secs(60),
            Err(env::VarError::NotUnicode(_)) => {
                return Err(WorkloadIdentityConfigurationError::InvalidEncoding)
            }
        };
        Self::from_encoded_keys(&keys, maximum_clock_skew)
    }

    pub fn from_encoded_keys(
        value: &str,
        maximum_clock_skew: Duration,
    ) -> Result<Self, WorkloadIdentityConfigurationError> {
        let mut keys = Vec::new();
        for entry in value.split(',') {
            let (key_id, product_and_secret) = entry
                .split_once('=')
                .ok_or(WorkloadIdentityConfigurationError::InvalidKeyEncoding)?;
            let (product_id, encoded_secret) = product_and_secret
                .split_once(':')
                .ok_or(WorkloadIdentityConfigurationError::InvalidKeyEncoding)?;
            let secret = decode_hex_value(encoded_secret)
                .ok_or(WorkloadIdentityConfigurationError::InvalidKeyEncoding)?;
            keys.push((key_id, product_id, secret));
        }
        Self::new(
            keys.iter()
                .map(|(key_id, product_id, secret)| (*key_id, *product_id, secret.as_slice())),
            maximum_clock_skew,
        )
    }

    pub fn new<'a>(
        keys: impl IntoIterator<Item = (&'a str, &'a str, &'a [u8])>,
        maximum_clock_skew: Duration,
    ) -> Result<Self, WorkloadIdentityConfigurationError> {
        if maximum_clock_skew.is_zero() {
            return Err(WorkloadIdentityConfigurationError::InvalidClockSkew);
        }

        let mut configured_keys = HashMap::new();
        for (key_id, product_id, secret) in keys {
            if !valid_key_id(key_id) {
                return Err(WorkloadIdentityConfigurationError::InvalidKeyId);
            }
            let product_id = ProductId::new(product_id)
                .map_err(|_| WorkloadIdentityConfigurationError::InvalidProduct)?;
            if secret.len() < MINIMUM_KEY_BYTES {
                return Err(WorkloadIdentityConfigurationError::KeyTooShort);
            }
            if configured_keys
                .insert(
                    key_id.to_owned(),
                    SigningKey {
                        product_id,
                        secret: secret.to_vec(),
                    },
                )
                .is_some()
            {
                return Err(WorkloadIdentityConfigurationError::DuplicateKeyId);
            }
        }

        if configured_keys.is_empty() {
            return Err(WorkloadIdentityConfigurationError::EmptyKeySet);
        }

        Ok(Self {
            keys: configured_keys,
            maximum_clock_skew,
        })
    }

    fn verify(
        &self,
        headers: &HeaderMap,
        method: &Method,
        uri: &Uri,
        now: OffsetDateTime,
    ) -> Option<TrustedIdentity> {
        let key_id = header_value(headers, KEY_ID_HEADER)?;
        let key = self.keys.get(key_id)?;
        let tenant_id = TenantId::new(header_value(headers, TENANT_ID_HEADER)?).ok()?;
        let timestamp = header_value(headers, TIMESTAMP_HEADER)?
            .parse::<i64>()
            .ok()?;
        let timestamp = OffsetDateTime::from_unix_timestamp(timestamp).ok()?;
        if (now - timestamp).unsigned_abs() > self.maximum_clock_skew {
            return None;
        }
        let supplied_signature = decode_hex(header_value(headers, SIGNATURE_HEADER)?)?;
        let mut mac = HmacSha256::new_from_slice(&key.secret).ok()?;
        mac.update(
            canonical_message(
                key_id,
                tenant_id.as_str(),
                timestamp.unix_timestamp(),
                method,
                uri,
            )
            .as_bytes(),
        );
        mac.verify_slice(&supplied_signature).ok()?;
        Some(TrustedIdentity {
            product_id: key.product_id.clone(),
            tenant_id,
        })
    }
}

pub fn with_workload_identity(router: Router, verifier: WorkloadIdentityVerifier) -> Router {
    router.layer(from_fn_with_state(verifier, verify_workload_identity))
}

async fn verify_workload_identity(
    verifier: axum::extract::State<WorkloadIdentityVerifier>,
    mut request: Request,
    next: Next,
) -> Response {
    if matches!(request.uri().path(), "/healthz" | "/readyz") {
        return next.run(request).await;
    }
    let identity = verifier.verify(
        request.headers(),
        request.method(),
        request.uri(),
        OffsetDateTime::now_utc(),
    );
    match identity {
        Some(identity) => {
            request.extensions_mut().insert(identity);
            next.run(request).await
        }
        None => ApiError::authentication_required(request_id_from_headers(request.headers()))
            .into_response(),
    }
}

fn canonical_message(
    key_id: &str,
    tenant_id: &str,
    timestamp: i64,
    method: &Method,
    uri: &Uri,
) -> String {
    format!(
        "{key_id}\n{tenant_id}\n{timestamp}\n{}\n{}",
        method.as_str(),
        uri.path_and_query().map_or("/", |value| value.as_str())
    )
}

fn header_value<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name)?.to_str().ok()
}

fn valid_key_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn decode_hex(value: &str) -> Option<Vec<u8>> {
    if value.len() != 64 {
        return None;
    }
    decode_hex_value(value)
}

fn decode_hex_value(value: &str) -> Option<Vec<u8>> {
    if value.is_empty()
        || !value.len().is_multiple_of(2)
        || !value.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return None;
    }
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            std::str::from_utf8(pair)
                .ok()
                .and_then(|pair| u8::from_str_radix(pair, 16).ok())
        })
        .collect()
}
