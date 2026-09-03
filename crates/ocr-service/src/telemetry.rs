use std::{env, net::IpAddr, time::Duration};

use opentelemetry::{global, trace::TracerProvider as _, KeyValue};
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::{propagation::TraceContextPropagator, trace::SdkTracerProvider, Resource};
use thiserror::Error;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};
use url::Url;

const OTLP_ENDPOINT: &str = "OTEL_EXPORTER_OTLP_ENDPOINT";
const OTLP_HEADERS: &str = "OTEL_EXPORTER_OTLP_HEADERS";
const OTLP_TRACE_HEADERS: &str = "OTEL_EXPORTER_OTLP_TRACES_HEADERS";
const DEPLOYMENT_ENVIRONMENT: &str = "DEPLOYMENT_ENVIRONMENT";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TelemetryConfig {
    endpoint: Option<String>,
    environment: String,
}

#[derive(Debug, Copy, Clone, Error, PartialEq, Eq)]
pub enum TelemetryConfigError {
    #[error("OTLP credentials are not accepted by the shared service")]
    CredentialsNotAllowed,
    #[error("OTLP endpoint must be a credential-free internal gateway")]
    UntrustedEndpoint,
    #[error("deployment environment is invalid")]
    InvalidEnvironment,
    #[error("telemetry environment contains invalid UTF-8")]
    InvalidEncoding,
}

#[derive(Debug, Error)]
pub enum TelemetryInitError {
    #[error("OTLP trace exporter configuration is invalid")]
    Exporter(#[from] opentelemetry_otlp::ExporterBuildError),
    #[error("global tracing subscriber is already installed")]
    Subscriber(#[from] tracing_subscriber::util::TryInitError),
    #[error("OTLP trace shutdown did not complete")]
    Shutdown(#[from] opentelemetry_sdk::error::OTelSdkError),
}

pub struct TelemetryRuntime {
    provider: Option<SdkTracerProvider>,
}

impl TelemetryConfig {
    pub fn new(endpoint: Option<&str>, environment: &str) -> Result<Self, TelemetryConfigError> {
        Self::from_values(endpoint, environment, false, false)
    }

    fn from_values(
        endpoint: Option<&str>,
        environment: &str,
        has_generic_headers: bool,
        has_trace_headers: bool,
    ) -> Result<Self, TelemetryConfigError> {
        if has_generic_headers || has_trace_headers {
            return Err(TelemetryConfigError::CredentialsNotAllowed);
        }
        if !valid_environment(environment) {
            return Err(TelemetryConfigError::InvalidEnvironment);
        }
        let endpoint = endpoint.map(validate_endpoint).transpose()?;
        Ok(Self {
            endpoint,
            environment: environment.to_owned(),
        })
    }

    pub fn from_process_environment() -> Result<Self, TelemetryConfigError> {
        let endpoint = optional_environment_value(OTLP_ENDPOINT)?;
        let environment = match optional_environment_value(DEPLOYMENT_ENVIRONMENT)? {
            Some(value) => value,
            None if endpoint.is_none() => "development".to_owned(),
            None => return Err(TelemetryConfigError::InvalidEnvironment),
        };
        Self::from_values(
            endpoint.as_deref(),
            &environment,
            optional_environment_value(OTLP_HEADERS)?.is_some(),
            optional_environment_value(OTLP_TRACE_HEADERS)?.is_some(),
        )
    }

    pub fn endpoint(&self) -> Option<&str> {
        self.endpoint.as_deref()
    }

    pub fn environment(&self) -> &str {
        &self.environment
    }
}

impl TelemetryRuntime {
    pub fn install(
        config: &TelemetryConfig,
        filter: EnvFilter,
    ) -> Result<Self, TelemetryInitError> {
        global::set_text_map_propagator(TraceContextPropagator::new());
        let provider = config
            .endpoint()
            .map(|endpoint| build_provider(endpoint, config.environment()))
            .transpose()?;
        let otel_layer = provider.as_ref().map(|provider| {
            tracing_opentelemetry::layer().with_tracer(provider.tracer("document-intelligence"))
        });
        tracing_subscriber::registry()
            .with(filter)
            .with(tracing_subscriber::fmt::layer().json())
            .with(otel_layer)
            .try_init()?;
        if let Some(provider) = &provider {
            global::set_tracer_provider(provider.clone());
        }
        Ok(Self { provider })
    }

    pub fn shutdown(mut self) -> Result<(), TelemetryInitError> {
        if let Some(provider) = self.provider.take() {
            provider.shutdown_with_timeout(Duration::from_secs(5))?;
        }
        Ok(())
    }
}

fn build_provider(
    endpoint: &str,
    environment: &str,
) -> Result<SdkTracerProvider, opentelemetry_otlp::ExporterBuildError> {
    let exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_tonic()
        .with_endpoint(endpoint)
        .with_timeout(Duration::from_secs(3))
        .build()?;
    let resource = Resource::builder()
        .with_attributes([
            KeyValue::new("service.name", "document-intelligence"),
            KeyValue::new("service.version", env!("CARGO_PKG_VERSION")),
            KeyValue::new("deployment.environment.name", environment.to_owned()),
        ])
        .build();
    Ok(SdkTracerProvider::builder()
        .with_batch_exporter(exporter)
        .with_resource(resource)
        .build())
}

fn optional_environment_value(name: &str) -> Result<Option<String>, TelemetryConfigError> {
    match env::var(name) {
        Ok(value) if value.is_empty() => Ok(None),
        Ok(value) => Ok(Some(value)),
        Err(env::VarError::NotPresent) => Ok(None),
        Err(env::VarError::NotUnicode(_)) => Err(TelemetryConfigError::InvalidEncoding),
    }
}

fn valid_environment(value: &str) -> bool {
    (1..=32).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && value
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
        && value
            .as_bytes()
            .last()
            .is_some_and(u8::is_ascii_alphanumeric)
}

fn validate_endpoint(value: &str) -> Result<String, TelemetryConfigError> {
    let endpoint = Url::parse(value).map_err(|_| TelemetryConfigError::UntrustedEndpoint)?;
    if !matches!(endpoint.scheme(), "http" | "https")
        || !endpoint.username().is_empty()
        || endpoint.password().is_some()
        || endpoint.query().is_some()
        || endpoint.fragment().is_some()
        || !matches!(endpoint.path(), "" | "/")
        || !trusted_gateway_host(&endpoint)
    {
        return Err(TelemetryConfigError::UntrustedEndpoint);
    }
    Ok(endpoint.to_string())
}

fn trusted_gateway_host(endpoint: &Url) -> bool {
    let Some(host) = endpoint.host_str() else {
        return false;
    };
    host.eq_ignore_ascii_case("localhost")
        || host.ends_with(".svc.cluster.local")
        || host
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback())
}

#[cfg(test)]
mod tests {
    use super::{TelemetryConfig, TelemetryConfigError};

    #[test]
    fn exporter_credentials_are_rejected() {
        for (generic_headers, trace_headers) in [(true, false), (false, true), (true, true)] {
            assert_eq!(
                TelemetryConfig::from_values(
                    Some("http://127.0.0.1:4317"),
                    "development",
                    generic_headers,
                    trace_headers,
                )
                .unwrap_err(),
                TelemetryConfigError::CredentialsNotAllowed
            );
        }
    }
}
