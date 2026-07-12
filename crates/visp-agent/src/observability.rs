//! Observability utilities for visp-agent.
//!
//! Provides helper functions for extracting OTel trace context from the
//! current tracing span and constructing [`TraceContext`] values.

use opentelemetry::trace::TraceContextExt;
use opentelemetry::trace::{SpanId, TraceFlags, TraceId, TraceState};
use std::str::FromStr;
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

/// Rebuild an OTel parent [`Context`] from a [`TraceContext`].
///
/// Returns `None` if:
/// - `tc.trace_id` cannot be parsed as hex
/// - `tc.parent_span_id` is `None` or cannot be parsed as hex
///
/// In all other cases a remote `SpanContext` is constructed — even if
/// `trace_flags` or `trace_state` are degenerate — so that the caller can
/// call `set_parent()` on a tracing span to continue the parent trace.
pub(crate) fn rebuild_parent_context(tc: &TraceContext) -> Option<opentelemetry::Context> {
    let trace_id = TraceId::from_hex(tc.trace_id.as_str()).ok()?;
    let parent_span_id = SpanId::from_hex(tc.parent_span_id.as_ref()?.as_str()).ok()?;

    let trace_state = tc
        .trace_state
        .as_ref()
        .and_then(|ts| TraceState::from_str(ts).ok())
        .unwrap_or_default();

    let flags = TraceFlags::new(tc.trace_flags);
    let sc = opentelemetry::trace::SpanContext::new(
        trace_id,
        parent_span_id,
        flags,
        /* is_remote */ true,
        trace_state,
    );
    Some(opentelemetry::Context::new().with_remote_span_context(sc))
}

#[cfg(test)]
#[path = "observability_tests.rs"]
mod tests;
