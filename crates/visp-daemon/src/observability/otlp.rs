//! OTel tracer provider construction for visp-daemon.
//!
//! Provides `build_tracer_provider` (production gRPC-OTLP), a test-friendly
//! `build_tracer_provider_with_exporter`, and resource construction.

use std::time::Duration;

use opentelemetry::KeyValue;
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::trace::{Sampler, SdkTracerProvider, SpanExporter};

use crate::config::OtlpConfig;

/// Build a production OTLP tracer provider via gRPC-tonic.
pub(crate) fn build_tracer_provider(cfg: &OtlpConfig) -> SdkTracerProvider {
    use opentelemetry_otlp::WithExportConfig;

    // Headers: set via env var so the tonic exporter picks them up.
    // This avoids direct tonic MetadataMap manipulation which would require
    // aligning tonic versions between our workspace (0.13) and
    // opentelemetry-otlp's internal tonic (0.14).
    if !cfg.headers.is_empty() {
        let header_str = cfg
            .headers
            .iter()
            .map(|(k, v)| format!("{}={}", k, v))
            .collect::<Vec<_>>()
            .join(",");
        // Safety: thread-local side effect only affects the OTLP exporter
        // construction on this thread; no other code reads this var.
        unsafe { std::env::set_var("OTEL_EXPORTER_OTLP_HEADERS", header_str) };
    }

    let exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_tonic()
        .with_endpoint(&cfg.endpoint)
        .with_timeout(Duration::from_secs(cfg.timeout_secs))
        .build()
        .expect("failed to build OTLP span exporter");

    let processor = opentelemetry_sdk::trace::BatchSpanProcessor::builder(exporter).build();

    SdkTracerProvider::builder()
        .with_span_processor(processor)
        .with_sampler(Sampler::ParentBased(Box::new(Sampler::TraceIdRatioBased(
            cfg.sample_rate,
        ))))
        .with_resource(build_resource())
        .build()
}

/// Build a tracer provider with a caller-supplied exporter (e.g., InMemorySpanExporter).
/// Uses SimpleSpanProcessor for synchronous export (test friendliness).
#[allow(dead_code)]
pub(crate) fn build_tracer_provider_with_exporter<E>(
    exporter: E,
    cfg: &OtlpConfig,
) -> SdkTracerProvider
where
    E: SpanExporter + 'static,
{
    let processor = opentelemetry_sdk::trace::SimpleSpanProcessor::new(exporter);

    SdkTracerProvider::builder()
        .with_span_processor(processor)
        .with_sampler(Sampler::ParentBased(Box::new(Sampler::TraceIdRatioBased(
            cfg.sample_rate,
        ))))
        .with_resource(build_resource())
        .build()
}

/// Build the OTel Resource with service identity and host metadata.
pub(crate) fn build_resource() -> Resource {
    Resource::builder()
        .with_attributes([
            KeyValue::new("service.name", "visp-daemon"),
            KeyValue::new("service.version", env!("CARGO_PKG_VERSION")),
            KeyValue::new(
                "host.name",
                std::env::var("HOSTNAME").unwrap_or_else(|_| "unknown".into()),
            ),
            KeyValue::new("process.pid", std::process::id() as i64),
        ])
        .build()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_resource_has_service_name() {
        let resource = build_resource();
        let attrs: Vec<_> = resource
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();

        assert!(
            attrs
                .iter()
                .any(|(k, v)| k == "service.name" && v == "visp-daemon"),
            "expected service.name='visp-daemon' in resource, got: {:?}",
            attrs
        );
        assert!(
            attrs
                .iter()
                .any(|(k, v)| k == "service.version" && v == env!("CARGO_PKG_VERSION")),
            "expected service.version='{}' in resource, got: {:?}",
            env!("CARGO_PKG_VERSION"),
            attrs
        );
        assert!(
            attrs.iter().any(|(k, _)| k == "host.name"),
            "expected host.name in resource, got: {:?}",
            attrs
        );
        assert!(
            attrs.iter().any(|(k, _)| k == "process.pid"),
            "expected process.pid in resource, got: {:?}",
            attrs
        );
    }
}
