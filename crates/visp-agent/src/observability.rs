//! Observability utilities for visp-agent.
//!
//! Provides helper functions for extracting OTel trace context from the
//! current tracing span and constructing [`TraceContext`] values.

use opentelemetry::trace::TraceContextExt;
use visp_core::TraceContext;

/// Extract a [`TraceContext`] from the current tracing span's OTel context.
///
/// If the current span has a valid OTel `SpanContext` (i.e. an
/// `OpenTelemetryLayer` is active in the tracing subscriber), uses OTel
/// trace/span IDs.  Otherwise falls back to UUID-based W3C IDs (W1 behavior).
///
/// # OTel active path
///
/// | Field             | Source                                      |
/// |-------------------|---------------------------------------------|
/// | `trace_id`        | Current OTel trace ID (32-hex lowercase)     |
/// | `span_id`         | Current OTel span ID (16-hex)               |
/// | `parent_span_id`  | Current OTel span ID (same, for Step 5)     |
/// | `trace_flags`     | Preserved from OTel (sampled bit)           |
///
/// # Fallback (OTel inactive)
///
/// | Field             | Source                                      |
/// |-------------------|---------------------------------------------|
/// | `trace_id`        | Random 32-hex from UUID v4                  |
/// | `span_id`         | Random 16-hex W3C span ID                   |
/// | `parent_span_id`  | Random 16-hex W3C span ID                   |
/// | `trace_flags`     | `1` (sampled)                               |
pub(crate) fn extract_trace_context() -> TraceContext {
    use tracing_opentelemetry::OpenTelemetrySpanExt;

    let current_span = tracing::Span::current();
    let otel_ctx = current_span.context();
    let otel_span = otel_ctx.span();
    let span_context = otel_span.span_context();

    if span_context.is_valid() {
        let trace_id = span_context.trace_id().to_string();
        let span_id = span_context.span_id().to_string();
        let trace_flags = span_context.trace_flags().to_u8();

        TraceContext::new(trace_id, span_id.clone(), trace_flags, None, Some(span_id))
            .expect("OTel trace_id and span_id are always valid hex strings")
    } else {
        // W1 fallback: UUID-based W3C IDs
        let trace_id = uuid::Uuid::new_v4().to_string().replace('-', "");
        let span_id = visp_core::trace_context::generate_w3c_span_id();
        let parent_span_id = visp_core::trace_context::generate_w3c_span_id();

        TraceContext::new(
            trace_id,
            span_id,
            1, // sampled
            None,
            Some(parent_span_id),
        )
        .expect("UUID-based IDs are always valid hex strings")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use opentelemetry::trace::TracerProvider;
    use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider, SimpleSpanProcessor};
    use tracing_opentelemetry::OpenTelemetrySpanExt;
    use tracing_subscriber::Registry;
    use tracing_subscriber::layer::SubscriberExt;

    /// Set up a minimal OTel subscriber with an in-memory exporter.
    /// Returns a guard that must be held for the subscriber to remain active.
    fn setup_otel_subscriber() -> tracing::subscriber::DefaultGuard {
        let exporter = InMemorySpanExporter::default();
        let provider = SdkTracerProvider::builder()
            .with_span_processor(SimpleSpanProcessor::new(exporter.clone()))
            .build();
        let tracer = provider.tracer("visp_agent_test");

        let subscriber = Registry::default().with(
            tracing_opentelemetry::OpenTelemetryLayer::new(tracer).with_context_activation(true),
        );

        tracing::subscriber::set_default(subscriber)
    }

    // ── W2-S4-1a: OTel active → trace_id + span_id are proper hex ─────────

    #[test]
    fn test_spawn_trace_context_carries_otel_ids_when_otel_active() {
        let _guard = setup_otel_subscriber();

        let parent_span = tracing::info_span!("parent");
        let tc = parent_span.in_scope(extract_trace_context);
        drop(parent_span);

        // trace_id must be 32-hex lowercase, non-zero
        assert_eq!(tc.trace_id.len(), 32);
        assert!(
            tc.trace_id.chars().all(|c| c.is_ascii_hexdigit()),
            "trace_id should be all hex chars, got: {tid}",
            tid = tc.trace_id
        );
        assert_ne!(
            tc.trace_id, "00000000000000000000000000000000",
            "trace_id should not be all zeros"
        );

        // span_id must be 16-hex lowercase, non-zero
        assert_eq!(tc.span_id.len(), 16);
        assert!(
            tc.span_id.chars().all(|c| c.is_ascii_hexdigit()),
            "span_id should be all hex chars, got: {sid}",
            sid = tc.span_id
        );
        assert_ne!(
            tc.span_id, "0000000000000000",
            "span_id should not be all zeros"
        );
    }

    // ── W2-S4-1b: parent_span_id == current OTel span_id (Oracle W1) ──────

    #[test]
    fn test_spawn_trace_context_parent_span_id_matches_current_otel_span() {
        let _guard = setup_otel_subscriber();

        let parent_span = tracing::info_span!("parent");
        let (tc, expected_span_id) = parent_span.in_scope(|| {
            let ctx = tracing::Span::current().context();
            let otel_span = ctx.span();
            let sc = otel_span.span_context();
            let expected = sc.span_id().to_string();
            (extract_trace_context(), expected)
        });
        drop(parent_span);

        assert_eq!(
            tc.parent_span_id,
            Some(expected_span_id),
            "parent_span_id should match the current OTel span_id"
        );
    }

    // ── W2-S4-1c: OTel inactive → UUID fallback ─────────────────────────

    #[test]
    fn test_spawn_trace_context_carries_uuid_when_otel_inactive() {
        // No OTel subscriber — tracing default is not OTel-aware
        let tc = extract_trace_context();

        // trace_id must be 32-hex lowercase, non-zero
        assert_eq!(tc.trace_id.len(), 32);
        assert!(
            tc.trace_id.chars().all(|c| c.is_ascii_hexdigit()),
            "trace_id should be all hex chars, got: {tid}",
            tid = tc.trace_id
        );
        assert_ne!(
            tc.trace_id, "00000000000000000000000000000000",
            "trace_id should not be all zeros"
        );

        // span_id must be 16-hex lowercase, non-zero
        assert_eq!(tc.span_id.len(), 16);
        assert!(
            tc.span_id.chars().all(|c| c.is_ascii_hexdigit()),
            "span_id should be all hex chars, got: {sid}",
            sid = tc.span_id
        );
        assert_ne!(
            tc.span_id, "0000000000000000",
            "span_id should not be all zeros"
        );

        // parent_span_id must be Some(16-hex)
        let psid = tc
            .parent_span_id
            .expect("fallback TraceContext should have parent_span_id");
        assert_eq!(psid.len(), 16);
        assert!(
            psid.chars().all(|c| c.is_ascii_hexdigit()),
            "parent_span_id should be all hex chars, got: {psid}"
        );
    }

    // ── W2-S4-1d: current span (not root) is used ──────────────────────────

    #[test]
    fn test_spawn_trace_context_uses_current_otel_span_not_root() {
        let _guard = setup_otel_subscriber();

        let outer = tracing::info_span!("outer");
        outer.in_scope(|| {
            let inner = tracing::info_span!("inner");
            inner.in_scope(|| {
                // Get the inner span's OTel span_id
                let ctx = tracing::Span::current().context();
                let inner_otel_span = ctx.span();
                let sc = inner_otel_span.span_context();
                let inner_otel_id = sc.span_id().to_string();

                let tc = extract_trace_context();

                assert_eq!(
                    tc.span_id, inner_otel_id,
                    "span_id should match the INNER (current) OTel span, not outer"
                );
                assert_eq!(
                    tc.parent_span_id.as_deref(),
                    Some(inner_otel_id.as_str()),
                    "parent_span_id should match the inner OTel span_id"
                );
            });
        });
        drop(outer);
    }
}
