//! OTLP export pipeline (feature `otlp`).
//!
//! A turnkey installer: build an OTLP/gRPC span exporter, wrap it in an
//! [`SdkTracerProvider`], register it + the W3C propagator globally, and install
//! a `tracing` subscriber whose `tracing-opentelemetry` layer turns the spans
//! opened by [`TraceContextLayer`](crate::TraceContextLayer) into exported OTLP
//! traces. Call it once at startup, inside a Tokio runtime, and keep the
//! returned [`TelemetryGuard`] alive for the process lifetime (its `Drop`
//! flushes pending spans).

use opentelemetry::global;
use opentelemetry::trace::TracerProvider as _;
use opentelemetry_otlp::{SpanExporter, WithExportConfig};
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::propagation::TraceContextPropagator;
use opentelemetry_sdk::trace::SdkTracerProvider;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, fmt};

use crate::TelemetryError;

/// Configuration for the OTLP pipeline.
#[derive(Debug, Clone)]
pub struct OtlpConfig {
    /// `service.name` resource attribute (how this server appears in traces).
    pub service_name: String,
    /// OTLP/gRPC collector endpoint. `None` uses the exporter default
    /// (`http://localhost:4317`).
    pub endpoint: Option<String>,
}

impl OtlpConfig {
    /// A config for `service_name` against the default local collector.
    #[must_use]
    pub fn new(service_name: impl Into<String>) -> Self {
        Self {
            service_name: service_name.into(),
            endpoint: None,
        }
    }

    /// Point at a specific OTLP/gRPC collector endpoint.
    #[must_use]
    pub fn endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.endpoint = Some(endpoint.into());
        self
    }
}

/// Keeps the tracer provider alive; flushes pending spans on drop. Hold it for
/// the process lifetime.
#[must_use = "dropping the guard shuts the exporter down and stops trace export"]
pub struct TelemetryGuard {
    provider: SdkTracerProvider,
}

impl Drop for TelemetryGuard {
    fn drop(&mut self) {
        // Best-effort flush; nothing actionable if the collector is already gone.
        let _ = self.provider.shutdown();
    }
}

/// Install the OTLP export pipeline and a `tracing` subscriber wired to it.
///
/// Registers a global tracer provider + W3C trace-context propagator and a
/// global subscriber (env-filter + fmt + the OpenTelemetry layer). Returns the
/// [`TelemetryGuard`] to hold for the process lifetime.
///
/// # Errors
/// - [`TelemetryError::Exporter`] if the OTLP exporter can't be built.
/// - [`TelemetryError::Subscriber`] if a global subscriber is already installed.
pub fn init_otlp(config: OtlpConfig) -> Result<TelemetryGuard, TelemetryError> {
    let mut builder = SpanExporter::builder().with_tonic();
    if let Some(endpoint) = &config.endpoint {
        builder = builder.with_endpoint(endpoint.clone());
    }
    let exporter = builder
        .build()
        .map_err(|e| TelemetryError::Exporter(e.to_string()))?;

    let resource = Resource::builder()
        .with_service_name(config.service_name)
        .build();
    let provider = SdkTracerProvider::builder()
        .with_batch_exporter(exporter)
        .with_resource(resource)
        .build();

    let tracer = provider.tracer("turbomcp");
    global::set_tracer_provider(provider.clone());
    global::set_text_map_propagator(TraceContextPropagator::new());

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer())
        .with(tracing_opentelemetry::layer().with_tracer(tracer))
        .try_init()
        .map_err(|e| TelemetryError::Subscriber(e.to_string()))?;

    Ok(TelemetryGuard { provider })
}
