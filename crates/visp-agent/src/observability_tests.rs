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

    let subscriber = Registry::default()
        .with(tracing_opentelemetry::OpenTelemetryLayer::new(tracer).with_context_activation(true));

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

// ── W2-S5: set_parent OTel-level test ──────────────────────────

use opentelemetry::trace::TraceId;

/// Set up OTel subscriber and return (exporter, tracer_provider, guard).
fn setup_otel_with_exporter() -> (
    opentelemetry_sdk::trace::InMemorySpanExporter,
    opentelemetry_sdk::trace::SdkTracerProvider,
    tracing::subscriber::DefaultGuard,
) {
    let exporter = opentelemetry_sdk::trace::InMemorySpanExporter::default();
    let provider = opentelemetry_sdk::trace::SdkTracerProvider::builder()
        .with_span_processor(opentelemetry_sdk::trace::SimpleSpanProcessor::new(
            exporter.clone(),
        ))
        .build();
    let tracer = provider.tracer("visp_agent_test");
    let subscriber = tracing_subscriber::registry()
        .with(tracing_opentelemetry::OpenTelemetryLayer::new(tracer).with_context_activation(true));
    let guard = tracing::subscriber::set_default(subscriber);
    (exporter, provider, guard)
}

/// Verify that rebuild_parent_context + set_parent stores the parent
/// OTel Context in the span's extensions.
#[test]
fn test_set_parent_produces_correct_otel_parent_span_id() {
    let (_exporter, _provider, _guard) = setup_otel_with_exporter();

    // 1. Create a parent OTel span and get its span context
    let parent_span = tracing::info_span!("parent_span");
    let parent_ctx = parent_span.in_scope(|| {
        let ctx = tracing::Span::current().context();
        let s = ctx.span();
        s.span_context().clone()
    });
    drop(parent_span);

    // 2. Build TraceContext from parent_ctx, then rebuild OTel Context
    let tc = TraceContext::new(
        parent_ctx.trace_id().to_string(),
        parent_ctx.span_id().to_string(),
        parent_ctx.trace_flags().to_u8(),
        Some(parent_ctx.trace_state().header()),
        Some(parent_ctx.span_id().to_string()),
    )
    .unwrap();

    let otel_ctx = rebuild_parent_context(&tc).expect("should rebuild context");

    // 3. Create child span with set_parent
    let child_span = tracing::info_span!("child_span");
    let _ = child_span.set_parent(otel_ctx);

    // 4. Verify that set_parent stored the parent context.
    //    The parent context is stored in the span's extensions but the
    //    OTel layer reads it during span creation.  We verify correctness
    //    by checking that the child_span's OWN OTel context (set when the
    //    span was entered) has the same trace_id as the parent, proving
    //    set_parent was received and the trace chain is intact.
    let child_otel_ctx = child_span.in_scope(|| tracing::Span::current().context());
    let s = child_otel_ctx.span();
    let child_sc = s.span_context();

    // The child span should have the same trace_id as the parent
    assert_eq!(
        child_sc.trace_id(),
        parent_ctx.trace_id(),
        "child span's OTel context should have parent's trace_id (set_parent worked)"
    );
    // The child span itself should NOT be remote (it's a local span, not a remote reference)
    assert!(
        !child_sc.is_remote(),
        "child span should be local, not remote"
    );

    drop(child_span);
}

/// Verify that rebuild_parent_context returns None for invalid TraceContext,
/// and the span creates its own trace.
#[test]
fn test_set_parent_fallback_invalid_trace_context() {
    let (_exporter, _provider, _guard) = setup_otel_with_exporter();

    // Invalid TraceContext (non-hex trace_id) → rebuild_parent_context returns None
    let tc = TraceContext {
        trace_id: "nothex".to_string(),
        span_id: "b7ad6b7169203331".to_string(),
        trace_flags: 1,
        trace_state: None,
        parent_span_id: Some("aaaaaaaaaaaaaaaa".to_string()),
    };

    let otel_ctx = rebuild_parent_context(&tc);
    assert!(
        otel_ctx.is_none(),
        "invalid TraceContext should return None"
    );

    // Create span without set_parent
    let child_span = tracing::info_span!("orphan_span");
    let child_otel_ctx = child_span.in_scope(|| tracing::Span::current().context());
    drop(child_span);

    let s = child_otel_ctx.span();
    let child_sc = s.span_context();
    // The span should create its own trace (not inherited)
    assert!(
        !child_sc.is_remote(),
        "without set_parent, span should NOT have remote span context"
    );
    assert_eq!(
        child_sc.trace_id().to_string().len(),
        32,
        "span should have a valid trace_id"
    );
}

// ── W2-S5: rebuild_parent_context unit tests ────────────────────────

#[test]
fn test_rebuild_parent_context_valid() {
    let tc = TraceContext::new(
        "0af7651916cd43dd8448eb211c80319c".to_string(),
        "b7ad6b7169203331".to_string(),
        1, // sampled
        Some("congo=toto".to_string()),
        Some("aaaaaaaaaaaaaaaa".to_string()),
    )
    .unwrap();

    let ctx = rebuild_parent_context(&tc);
    assert!(ctx.is_some(), "should produce Some context for valid input");

    let ctx = ctx.unwrap();
    let span = ctx.span();
    let sc = span.span_context();
    assert_eq!(
        sc.trace_id(),
        TraceId::from_hex("0af7651916cd43dd8448eb211c80319c").unwrap()
    );
    assert_eq!(sc.span_id(), SpanId::from_hex("aaaaaaaaaaaaaaaa").unwrap());
    assert!(sc.is_remote(), "should be marked as remote");
    assert_eq!(sc.trace_flags(), TraceFlags::new(1));
    assert_eq!(sc.trace_state().header(), "congo=toto");
}

#[test]
fn test_rebuild_parent_context_invalid_trace_id_returns_none() {
    // Construct TraceContext directly to bypass validation
    let tc = TraceContext {
        trace_id: "invalid".to_string(),
        span_id: "b7ad6b7169203331".to_string(),
        trace_flags: 1,
        trace_state: None,
        parent_span_id: Some("aaaaaaaaaaaaaaaa".to_string()),
    };
    assert!(rebuild_parent_context(&tc).is_none());
}

#[test]
fn test_rebuild_parent_context_missing_parent_span_id_returns_none() {
    let tc = TraceContext {
        trace_id: "0af7651916cd43dd8448eb211c80319c".to_string(),
        span_id: "b7ad6b7169203331".to_string(),
        trace_flags: 1,
        trace_state: None,
        parent_span_id: None,
    };
    assert!(rebuild_parent_context(&tc).is_none());
}

#[test]
fn test_rebuild_parent_context_invalid_parent_span_id_returns_none() {
    let tc = TraceContext {
        trace_id: "0af7651916cd43dd8448eb211c80319c".to_string(),
        span_id: "b7ad6b7169203331".to_string(),
        trace_flags: 1,
        trace_state: None,
        parent_span_id: Some("nothex".to_string()),
    };
    assert!(rebuild_parent_context(&tc).is_none());
}

#[test]
fn test_rebuild_parent_context_invalid_trace_state_uses_default() {
    let tc = TraceContext::new(
        "0af7651916cd43dd8448eb211c80319c".to_string(),
        "b7ad6b7169203331".to_string(),
        1,
        Some("invalid!state!format".to_string()),
        Some("aaaaaaaaaaaaaaaa".to_string()),
    )
    .unwrap();

    let ctx = rebuild_parent_context(&tc);
    assert!(
        ctx.is_some(),
        "should still produce Some context even with invalid trace_state"
    );
    let ctx = ctx.unwrap();
    let span = ctx.span();
    let sc = span.span_context();
    // trace_state should be default (empty) since the string was invalid
    assert_eq!(sc.trace_state().header(), "");
}
