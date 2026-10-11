use crate::Error;
use reqwest::Url;
use std::{env, time::Duration};

pub const PRODUCT: &str = "ocr-sandbox";
pub const MAX_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_PIXELS: u64 = 12_000_000;

pub struct Config {
    pub origin: Url,
    pub upload_api: Url,
    pub job_api: Url,
    pub database_url: String,
    pub key_id: String,
    pub(crate) secret: Vec<u8>,
    pub buckets: [String; 4],
    pub daily_uploads: i64,
    pub admission_enabled: bool,
}

impl Config {
    pub fn from_env() -> Result<Self, Error> {
        let required = |key| env::var(key).map_err(|_| Error::Configuration);
        let origin = endpoint(&required("SANDBOX_ORIGIN")?, false)?;
        let upload_api = endpoint(&required("SANDBOX_UPLOAD_API")?, true)?;
        let job_api = endpoint(&required("SANDBOX_JOB_API")?, true)?;
        let (key_id, secret) = Self::validate_identity(&required("SANDBOX_WORKLOAD_IDENTITY")?)?;
        let buckets: [String; 4] = required("SANDBOX_BUCKETS")?
            .split(',')
            .map(str::to_owned)
            .collect::<Vec<_>>()
            .try_into()
            .map_err(|_| Error::Configuration)?;
        for (index, bucket) in buckets.iter().enumerate() {
            if !bucket.starts_with("ocr-sandbox-")
                || bucket.len() > 63
                || !bucket
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
                || bucket.ends_with('-')
                || buckets[..index].contains(bucket)
            {
                return Err(Error::Configuration);
            }
        }
        let daily_uploads = env::var("SANDBOX_DAILY_UPLOADS")
            .unwrap_or_else(|_| "100".into())
            .parse::<i64>()
            .ok()
            .filter(|n| (1..=100).contains(n))
            .ok_or(Error::Configuration)?;
        Ok(Self {
            admission_enabled: match env::var("SANDBOX_ADMISSION_ENABLED").as_deref() {
                Ok("true") => true,
                Ok("false") | Err(env::VarError::NotPresent) => false,
                _ => return Err(Error::Configuration),
            },
            origin,
            upload_api,
            job_api,
            database_url: required("SANDBOX_DATABASE_URL")?,
            key_id,
            secret,
            buckets,
            daily_uploads,
        })
    }

    pub fn validate_identity(value: &str) -> Result<(String, Vec<u8>), Error> {
        let (id, rest) = value.split_once('=').ok_or(Error::Configuration)?;
        let (product, hex) = rest.split_once(':').ok_or(Error::Configuration)?;
        if product != PRODUCT
            || id.is_empty()
            || id.len() > 64
            || !id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
            || hex.len() != 64
            || !hex.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err(Error::Configuration);
        }
        let secret = hex
            .as_bytes()
            .chunks_exact(2)
            .map(|pair| {
                let s = std::str::from_utf8(pair).map_err(|_| Error::Configuration)?;
                u8::from_str_radix(s, 16).map_err(|_| Error::Configuration)
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok((id.to_owned(), secret))
    }

    pub fn client(&self) -> Result<reqwest::Client, Error> {
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(20))
            .build()
            .map_err(|_| Error::Configuration)
    }
}

fn endpoint(value: &str, private: bool) -> Result<Url, Error> {
    let url = Url::parse(value).map_err(|_| Error::Configuration)?;
    let local = matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
    if !(url.scheme() == "https" || (url.scheme() == "http" && (private || local)))
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != "/"
    {
        return Err(Error::Configuration);
    }
    Ok(url)
}
