//! Test: verify that deferred span.record() calls propagate to OTel span attributes
//! with correct types (i64 for integers, not String) when using
//! OpenTelemetryLayer with context_activation(true).
//!
//! Root cause: tracing-opentelemetry 0.33's SpanAttributeVisitor only implements
//! `record_i64`, not `record_u64`. When u32/u64 values are passed to
//! `span.record()`, they fall through to `record_debug` and get exported as
//! String("100") instead of I64(100). The fix is to cast to i64 before recording.

use opentelemetry::trace::TracerProvider;
use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider};
use tracing_opentelemetry::OpenTelemetryLayer;
use tracing_subscriber::prelude::*;

#[test]
fn test_deferred_record_with_context_activation() {
    let exporter = InMemorySpanExporter::default();
    let provider = SdkTracerProvider::builder()
        .with_simple_exporter(exporter.clone())
        .build();
    let tracer = provider.tracer("test");
    let otel_layer = OpenTelemetryLayer::new(tracer).with_context_activation(true);

    let subscriber = tracing_subscriber::registry().with(otel_layer);

    tracing::subscriber::with_default(subscriber, || {
        let span = tracing::info_span!(
            "gen_ai.client.operation",
            gen_ai.system = tracing::field::Empty,
            gen_ai.request.model = "gpt-4o",
            gen_ai.provider.name = "openai",
            gen_ai.usage.input_tokens = tracing::field::Empty,
            gen_ai.usage.output_tokens = tracing::field::Empty,
            gen_ai.response.model = tracing::field::Empty,
            visp.llm.cost_usd = tracing::field::Empty,
        );

        span.record("gen_ai.system", "openai");
        // Cast to i64 — this is the fix
        span.record("gen_ai.usage.input_tokens", 100i64);
        span.record("gen_ai.usage.output_tokens", 50i64);
        span.record("gen_ai.response.model", "gpt-4o-2024-08-06");
        span.record("visp.llm.cost_usd", 0.0015f64);

        let _guard = span.enter();
        drop(_guard);
        drop(span);
    });

    let _ = provider.force_flush();
    let spans = exporter.get_finished_spans().expect("no spans");
    assert_eq!(spans.len(), 1, "expected exactly 1 span");

    let span = &spans[0];
    let attrs: std::collections::HashMap<String, opentelemetry::Value> = span
        .attributes
        .iter()
        .map(|kv| (kv.key.to_string(), kv.value.clone()))
        .collect();

    assert_eq!(
        attrs.get("gen_ai.system"),
        Some(&opentelemetry::Value::String("openai".into())),
        "gen_ai.system should be 'openai'"
    );
    assert_eq!(
        attrs.get("gen_ai.request.model"),
        Some(&opentelemetry::Value::String("gpt-4o".into())),
        "gen_ai.request.model should be 'gpt-4o'"
    );
    assert_eq!(
        attrs.get("gen_ai.provider.name"),
        Some(&opentelemetry::Value::String("openai".into())),
        "gen_ai.provider.name should be 'openai'"
    );
    assert_eq!(
        attrs.get("gen_ai.usage.input_tokens"),
        Some(&opentelemetry::Value::I64(100)),
        "gen_ai.usage.input_tokens should be I64(100), got: {:?}",
        attrs.get("gen_ai.usage.input_tokens")
    );
    assert_eq!(
        attrs.get("gen_ai.usage.output_tokens"),
        Some(&opentelemetry::Value::I64(50)),
        "gen_ai.usage.output_tokens should be I64(50), got: {:?}",
        attrs.get("gen_ai.usage.output_tokens")
    );
    assert_eq!(
        attrs.get("gen_ai.response.model"),
        Some(&opentelemetry::Value::String("gpt-4o-2024-08-06".into())),
        "gen_ai.response.model should be 'gpt-4o-2024-08-06'"
    );
    assert_eq!(
        attrs.get("visp.llm.cost_usd"),
        Some(&opentelemetry::Value::F64(0.0015)),
        "visp.llm.cost_usd should be 0.0015"
    );

    let _ = provider.shutdown();
}

/// Verify that u64 values (without the i64 cast) are exported as String.
/// This test documents the bug — it should pass with the WRONG behavior
/// to prove the root cause.
#[test]
fn test_u64_recorded_as_string_bug() {
    let exporter = InMemorySpanExporter::default();
    let provider = SdkTracerProvider::builder()
        .with_simple_exporter(exporter.clone())
        .build();
    let tracer = provider.tracer("test");
    let otel_layer = OpenTelemetryLayer::new(tracer).with_context_activation(true);

    let subscriber = tracing_subscriber::registry().with(otel_layer);

    tracing::subscriber::with_default(subscriber, || {
        let span = tracing::info_span!(
            "gen_ai.client.operation",
            gen_ai.usage.input_tokens = tracing::field::Empty,
        );

        // u32 value — triggers the bug (u32 → u64 → record_debug → String)
        span.record("gen_ai.usage.input_tokens", 100u32);

        let _guard = span.enter();
        drop(_guard);
        drop(span);
    });

    let _ = provider.force_flush();
    let spans = exporter.get_finished_spans().expect("no spans");
    assert_eq!(spans.len(), 1);

    let span = &spans[0];
    let attrs: std::collections::HashMap<String, opentelemetry::Value> = span
        .attributes
        .iter()
        .map(|kv| (kv.key.to_string(), kv.value.clone()))
        .collect();

    // BUG: u32 is exported as String("100") instead of I64(100)
    assert_eq!(
        attrs.get("gen_ai.usage.input_tokens"),
        Some(&opentelemetry::Value::String("100".into())),
        "u32 value should be exported as String (the bug), got: {:?}",
        attrs.get("gen_ai.usage.input_tokens")
    );

    let _ = provider.shutdown();
}

/// Verify that i64 values are exported correctly as I64.
#[test]
fn test_i64_recorded_correctly() {
    let exporter = InMemorySpanExporter::default();
    let provider = SdkTracerProvider::builder()
        .with_simple_exporter(exporter.clone())
        .build();
    let tracer = provider.tracer("test");
    let otel_layer = OpenTelemetryLayer::new(tracer).with_context_activation(true);

    let subscriber = tracing_subscriber::registry().with(otel_layer);

    tracing::subscriber::with_default(subscriber, || {
        let span = tracing::info_span!(
            "gen_ai.client.operation",
            gen_ai.usage.input_tokens = tracing::field::Empty,
        );

        // i64 value — works correctly
        span.record("gen_ai.usage.input_tokens", 100i64);

        let _guard = span.enter();
        drop(_guard);
        drop(span);
    });

    let _ = provider.force_flush();
    let spans = exporter.get_finished_spans().expect("no spans");
    assert_eq!(spans.len(), 1);

    let span = &spans[0];
    let attrs: std::collections::HashMap<String, opentelemetry::Value> = span
        .attributes
        .iter()
        .map(|kv| (kv.key.to_string(), kv.value.clone()))
        .collect();

    assert_eq!(
        attrs.get("gen_ai.usage.input_tokens"),
        Some(&opentelemetry::Value::I64(100)),
        "i64 value should be exported as I64(100), got: {:?}",
        attrs.get("gen_ai.usage.input_tokens")
    );

    let _ = provider.shutdown();
}
