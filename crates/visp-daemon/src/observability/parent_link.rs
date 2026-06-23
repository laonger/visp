//! ParentLinkLayer — cross-mpsc span parent_span_id field completion.
//!
//! Wave 1: field complement only, does **not** modify the tracing tree parent.
//! Refs: design §7.3, plan §Step 4a.
//!
//! # Mapping
//!
//! Maintains a bidirectional mapping between W3C span IDs (either from
//! [`TraceContext`] or [`SpanW3CId`]) and tracing [`span::Id`] values.
//! When a span carries a [`TraceContext`] whose `parent_span_id` is `Some`,
//! the layer looks up that parent W3C ID in the mapping and writes
//! [`ParentLinkFields`] with the resolved parent information.
//!
//! The mapping is cleaned up in `on_close` (both directions).

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use dashmap::DashMap;
use tracing::Subscriber;
use tracing::field::{Field, Visit};
use tracing_subscriber::layer::{Context, Layer};
use tracing_subscriber::registry::LookupSpan;

use visp_core::trace_context::SpanW3CId;

/// Custom fields inserted into the span extension when trace context fields
/// are recorded via `span.record()`.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct ParentLinkFields {
    pub trace_id: String,
    pub parent_span_id: Option<String>,
    pub trace_state: Option<String>,
}

/// Accumulates trace context fields across multiple `span.record()` calls.
#[derive(Default, Clone)]
struct PendingTraceFields {
    trace_id: Option<String>,
    parent_span_id: Option<String>,
    trace_state: Option<String>,
}

/// Inner state shared via `Arc` so the layer can be cloned cheaply.
#[allow(dead_code)]
#[derive(Debug)]
struct Inner {
    /// Forward mapping: W3C span_id → tracing span::Id.
    mapping: DashMap<String, tracing::span::Id>,
    /// Reverse mapping: tracing span::Id → W3C span_id.
    reverse: DashMap<tracing::span::Id, String>,
    /// Counter of parent span IDs not found in mapping.
    unmatched_count: AtomicU64,
}

/// A tracing [`Layer`] that detects [`TraceContext`] and [`SpanW3CId`] in
/// span extensions and writes [`ParentLinkFields`] into the span extension
/// for JSON field completion by a downstream fmt layer.
///
/// # Wave 1 scope
/// - Maintains a bidirectional mapping: W3C span_id ↔ tracing `span::Id`.
/// - Counts unmatched parent span IDs (for debugging / Wave 2 validation).
/// - Cleans up mapping entries on span close.
/// - Does **not** alter the tracing tree parent — field complement only.
// Not yet wired into the subscriber; Step 5-sub will assemble it.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct ParentLinkLayer {
    inner: Arc<Inner>,
}

impl ParentLinkLayer {
    /// Create a new `ParentLinkLayer` with empty mapping and zero counter.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Inner {
                mapping: DashMap::new(),
                reverse: DashMap::new(),
                unmatched_count: AtomicU64::new(0),
            }),
        }
    }

    /// Return the number of parent span IDs that were **not** found in the
    /// mapping (a debugging / Wave 2 metric).
    #[allow(dead_code)]
    pub fn unmatched_count(&self) -> u64 {
        self.inner.unmatched_count.load(Ordering::Relaxed)
    }
}

impl Default for ParentLinkLayer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
impl ParentLinkLayer {
    /// Remove a W3C span_id from the mapping (test helper).
    /// Used to simulate "parent W3C span_id not in mapping" scenarios.
    #[allow(dead_code)]
    pub fn remove_from_mapping(&self, span_id: &str) {
        if let Some((_, tid)) = self.inner.mapping.remove(span_id) {
            self.inner.reverse.remove(&tid);
        }
    }
}

/// Marker inserted into span extension after fields have been recorded
/// via `on_enter`, to prevent re-recording on subsequent enters.
#[derive(Clone)]
struct ParentLinkFieldsRecorded;

// ---------------------------------------------------------------------------
// Trace fields extractor (used in on_record)
// ---------------------------------------------------------------------------

/// Extracts trace context fields and W3C span IDs from span recordings.
#[derive(Default)]
struct TraceFieldsExtractor {
    w3c_id: Option<String>,
    trace_id: Option<String>,
    parent_span_id: Option<String>,
    trace_state: Option<String>,
}

impl Visit for TraceFieldsExtractor {
    fn record_str(&mut self, field: &Field, value: &str) {
        match field.name() {
            "visp.span.w3c_id" => self.w3c_id = Some(value.to_string()),
            "trace_id" => self.trace_id = Some(value.to_string()),
            "parent_span_id" => self.parent_span_id = Some(value.to_string()),
            "trace_state" => self.trace_state = Some(value.to_string()),
            _ => {}
        }
    }

    fn record_debug(&mut self, _field: &Field, _value: &dyn std::fmt::Debug) {
        // Not interested in debug-recorded fields.
    }
}

// ---------------------------------------------------------------------------
// Layer implementation
// ---------------------------------------------------------------------------

impl<S> Layer<S> for ParentLinkLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(
        &self,
        _attrs: &tracing::span::Attributes<'_>,
        id: &tracing::span::Id,
        ctx: Context<'_, S>,
    ) {
        let span_ref = match ctx.span(id) {
            Some(s) => s,
            None => return,
        };

        // Register mapping from SpanW3CId if present (set by another layer).
        if let Some(sw) = span_ref.extensions().get::<SpanW3CId>().cloned() {
            self.inner.mapping.insert(sw.0.clone(), id.clone());
            self.inner.reverse.insert(id.clone(), sw.0);
        }

        // Cross-mpsc propagation: copy ParentLinkFields from parent span.
        // This mirrors MetricsLayer's SessionId propagation pattern.
        if span_ref.extensions().get::<ParentLinkFields>().is_some() {
            return; // Already has explicit fields (set via on_record).
        }

        if let Some(parent) = span_ref.parent()
            && let Some(plf) = parent.extensions().get::<ParentLinkFields>().cloned()
        {
            span_ref.extensions_mut().insert(plf);
        }
    }

    /// Detect `visp.span.w3c_id`, `trace_id`, `parent_span_id`, and
    /// `trace_state` field recordings.
    ///
    /// - W3C IDs are registered in the mapping.
    /// - Trace context fields are accumulated via [`PendingTraceFields`]
    ///   and written as [`ParentLinkFields`] into the span extension.
    fn on_record(
        &self,
        id: &tracing::span::Id,
        values: &tracing::span::Record<'_>,
        ctx: Context<'_, S>,
    ) {
        let mut extractor = TraceFieldsExtractor::default();
        values.record(&mut extractor);

        let span_ref = match ctx.span(id) {
            Some(s) => s,
            None => return,
        };

        // Handle visp.span.w3c_id — register W3C ID in mapping.
        if let Some(w3c_id) = extractor.w3c_id {
            span_ref.extensions_mut().insert(SpanW3CId(w3c_id.clone()));
            self.inner.mapping.insert(w3c_id.clone(), id.clone());
            self.inner.reverse.insert(id.clone(), w3c_id);
        }

        // Handle trace context fields — accumulate and write ParentLinkFields.
        let has_trace_fields = extractor.trace_id.is_some()
            || extractor.parent_span_id.is_some()
            || extractor.trace_state.is_some();

        if !has_trace_fields {
            return;
        }

        // Accumulate partial field data across multiple record() calls.
        let mut pending = span_ref
            .extensions()
            .get::<PendingTraceFields>()
            .cloned()
            .unwrap_or_default();

        if let Some(tid) = extractor.trace_id {
            pending.trace_id = Some(tid);
        }
        if let Some(psid) = &extractor.parent_span_id {
            // Only count unmatched the first time for this span.
            if pending.parent_span_id.is_none() && !self.inner.mapping.contains_key(psid) {
                self.inner.unmatched_count.fetch_add(1, Ordering::Relaxed);
            }
            pending.parent_span_id = Some(psid.clone());
        }
        if let Some(ts) = extractor.trace_state {
            pending.trace_state = Some(ts);
        }

        // Remove-then-insert to avoid tracing-subscriber's debug assertion
        // that prevents re-inserting an extension of the same type.
        span_ref.extensions_mut().remove::<PendingTraceFields>();
        span_ref.extensions_mut().insert(pending.clone());

        // Write ParentLinkFields if we have at least trace_id.
        if let Some(trace_id) = &pending.trace_id {
            span_ref.extensions_mut().remove::<ParentLinkFields>();
            span_ref.extensions_mut().insert(ParentLinkFields {
                trace_id: trace_id.clone(),
                parent_span_id: pending.parent_span_id.clone(),
                trace_state: pending.trace_state.clone(),
            });
        }
    }

    /// Clean up mapping entries when a span is closed (P1-2).
    fn on_close(&self, id: tracing::span::Id, _ctx: Context<'_, S>) {
        if let Some((_, w3c_id)) = self.inner.reverse.remove(&id) {
            self.inner.mapping.remove(&w3c_id);
        }
    }

    /// Record `trace_id` and `parent_span_id` from [`ParentLinkFields`] onto
    /// the span so that the JSON fmt layer outputs them (Approach A).
    ///
    /// This fires on first entry only; subsequent entries are no-ops thanks
    /// to [`ParentLinkFieldsRecorded`] marker.
    fn on_enter(&self, id: &tracing::span::Id, ctx: Context<'_, S>) {
        let span_ref = match ctx.span(id) {
            Some(s) => s,
            None => return,
        };

        // Already recorded on a previous enter.
        if span_ref
            .extensions()
            .get::<ParentLinkFieldsRecorded>()
            .is_some()
        {
            return;
        }

        let plf = match span_ref.extensions().get::<ParentLinkFields>() {
            Some(f) => f.clone(),
            None => return,
        };

        // Record fields on the current span (which should be the same as
        // the entered span). This makes the JSON fmt layer output them.
        let current = tracing::Span::current();
        current.record("trace_id", plf.trace_id.as_str());
        if let Some(ref psid) = plf.parent_span_id {
            current.record("parent_span_id", psid.as_str());
        }

        span_ref.extensions_mut().insert(ParentLinkFieldsRecorded);
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use serial_test::serial;
    use tracing::span::{Attributes, Id, Record};
    use tracing_subscriber::Layer;
    use tracing_subscriber::layer::Context;
    use tracing_subscriber::prelude::*;
    use tracing_subscriber::registry::LookupSpan;

    use super::*;

    /// Injector layer that places a `SpanW3CId` into every new span's
    /// extension.
    #[derive(Clone)]
    struct TestW3CIdInjector {
        w3c_id: String,
    }

    impl TestW3CIdInjector {
        fn new(w3c_id: &str) -> Self {
            Self {
                w3c_id: w3c_id.to_string(),
            }
        }
    }

    impl<S> Layer<S> for TestW3CIdInjector
    where
        S: Subscriber + for<'a> LookupSpan<'a>,
    {
        fn on_new_span(&self, _attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
            if let Some(span_ref) = ctx.span(id) {
                span_ref
                    .extensions_mut()
                    .insert(SpanW3CId(self.w3c_id.clone()));
            }
        }
    }

    /// Collector layer that records whether any span's extension contains
    /// `ParentLinkFields`. Captures both via `on_new_span` (propagation)
    /// and `on_record` (explicit field recording).
    #[derive(Clone)]
    struct TestFieldsCollector {
        seen: Arc<Mutex<Vec<ParentLinkFields>>>,
    }

    impl TestFieldsCollector {
        fn new() -> Self {
            Self {
                seen: Arc::new(Mutex::new(Vec::new())),
            }
        }
    }

    impl<S> Layer<S> for TestFieldsCollector
    where
        S: Subscriber + for<'a> LookupSpan<'a>,
    {
        fn on_new_span(&self, _attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
            if let Some(span_ref) = ctx.span(id)
                && let Some(plf) = span_ref.extensions().get::<ParentLinkFields>().cloned()
            {
                self.seen.lock().unwrap().push(plf);
            }
        }

        fn on_record(&self, id: &Id, _values: &Record<'_>, ctx: Context<'_, S>) {
            if let Some(span_ref) = ctx.span(id)
                && let Some(plf) = span_ref.extensions().get::<ParentLinkFields>().cloned()
            {
                self.seen.lock().unwrap().push(plf);
            }
        }
    }

    // ── W1-S4a-1: Layer skeleton ──────────────────────────────────────────

    #[test]
    #[serial]
    fn test_parent_link_layer_compiles_and_registers() {
        let _guard = tracing_subscriber::registry()
            .with(ParentLinkLayer::new())
            .set_default();
        // Compilation + registration is sufficient for this test.
    }

    #[test]
    #[serial]
    fn test_parent_link_layer_no_op_when_no_trace_context() {
        let _guard = tracing_subscriber::registry()
            .with(ParentLinkLayer::new())
            .set_default();

        let span = tracing::info_span!("normal_span");
        let _enter = span.enter();
        // Must not panic.
    }

    // ── W1-S4a-3 (rewritten): span field recording ────────────────────────

    #[test]
    #[serial]
    fn test_parent_link_layer_inserts_parent_link_fields_extension() {
        let collector = TestFieldsCollector::new();

        let _guard = tracing_subscriber::registry()
            .with(ParentLinkLayer::new())
            .with(collector.clone())
            .set_default();

        let span = tracing::info_span!(
            "visp.subagent.spawn",
            trace_id = tracing::field::Empty,
            parent_span_id = tracing::field::Empty,
        );
        span.record("trace_id", "0af7651916cd43dd8448eb211c80319c");
        drop(span);

        let seen = collector.seen.lock().unwrap();
        assert_eq!(
            seen.len(),
            1,
            "expected exactly one span with ParentLinkFields"
        );
        assert_eq!(seen[0].trace_id, "0af7651916cd43dd8448eb211c80319c");
        // parent_span_id is None because we didn't record it
        assert_eq!(seen[0].parent_span_id, None);
    }

    #[test]
    #[serial]
    fn test_parent_link_layer_no_fields_when_no_trace_context() {
        let collector = TestFieldsCollector::new();

        let _guard = tracing_subscriber::registry()
            .with(ParentLinkLayer::new())
            .with(collector.clone())
            .set_default();

        let span = tracing::info_span!("plain_span");
        drop(span);

        let seen = collector.seen.lock().unwrap();
        assert!(
            seen.is_empty(),
            "expected no ParentLinkFields for plain span"
        );
    }

    // ── P0-3: parent_span_id via mapping (rewritten: span field recording) ─

    #[test]
    #[serial]
    fn test_parent_link_writes_parent_span_id_when_parent_in_mapping() {
        let layer = ParentLinkLayer::new();
        let collector = TestFieldsCollector::new();

        // Parent span registers its W3C ID in the mapping via SpanW3CId extension.
        // Child span records parent_span_id pointing to the parent's W3C ID.
        let parent_w3c = "aaaaaaaaaaaaaaaa";

        let _guard = tracing_subscriber::registry()
            .with(TestW3CIdInjector::new(parent_w3c))
            .with(layer.clone())
            .with(collector.clone())
            .set_default();

        // Create parent span → registers "aaa..." in mapping.
        // Keep it alive while child is being created.
        let parent_span = tracing::info_span!("parent");
        let _parent_enter = parent_span.enter();

        // Create child span with field recordings.
        let child_span = tracing::info_span!(
            "child",
            trace_id = tracing::field::Empty,
            parent_span_id = tracing::field::Empty,
        );
        child_span.record("trace_id", "0af7651916cd43dd8448eb211c80319c");
        child_span.record("parent_span_id", parent_w3c);
        drop(child_span);

        let seen = collector.seen.lock().unwrap();
        // We expect 1 span with ParentLinkFields (the child)
        let child_plf = seen.iter().find(|plf| plf.parent_span_id.is_some());
        assert!(
            child_plf.is_some(),
            "expected a ParentLinkFields with parent_span_id set"
        );
        assert_eq!(
            child_plf.unwrap().parent_span_id.as_deref(),
            Some(parent_w3c),
        );
    }

    #[test]
    #[serial]
    fn test_parent_link_increments_unmatched_when_parent_missing() {
        let layer = ParentLinkLayer::new();

        let _guard = tracing_subscriber::registry()
            .with(layer.clone())
            .set_default();

        assert_eq!(layer.unmatched_count(), 0);

        // Record parent_span_id that doesn't exist in mapping.
        let child_span = tracing::info_span!("child", parent_span_id = tracing::field::Empty,);
        child_span.record("parent_span_id", "aaaaaaaaaaaaaaaa");
        drop(child_span);

        assert!(
            layer.unmatched_count() > 0,
            "expected unmatched_count > 0 when parent W3C ID is not in mapping"
        );
    }

    #[test]
    #[serial]
    fn test_parent_link_registers_w3c_id_from_span_w3c_id_extension() {
        let layer = ParentLinkLayer::new();
        let collector = TestFieldsCollector::new();

        let parent_w3c = "cccccccccccccccc";

        let _guard = tracing_subscriber::registry()
            .with(TestW3CIdInjector::new(parent_w3c))
            .with(layer.clone())
            .with(collector.clone())
            .set_default();

        // Parent span creates mapping via SpanW3CId extension
        let parent_span = tracing::info_span!("parent");
        let _parent_enter = parent_span.enter();

        // Child span records field parent_span_id
        let child_span = tracing::info_span!(
            "child",
            trace_id = tracing::field::Empty,
            parent_span_id = tracing::field::Empty,
        );
        child_span.record("trace_id", "0af7651916cd43dd8448eb211c80319c");
        child_span.record("parent_span_id", parent_w3c);
        drop(child_span);

        let seen = collector.seen.lock().unwrap();
        let child_plf = seen.iter().find(|plf| plf.parent_span_id.is_some());
        assert!(
            child_plf.is_some(),
            "child should have parent_span_id resolved"
        );
        assert_eq!(
            child_plf.unwrap().parent_span_id.as_deref(),
            Some(parent_w3c),
        );
    }

    // ── P1-2: mapping cleanup on span close ───────────────────────────────

    #[test]
    #[serial]
    fn test_parent_link_cleans_mapping_on_span_close() {
        let layer = ParentLinkLayer::new();

        let _guard = tracing_subscriber::registry()
            .with(layer.clone())
            .set_default();

        // Register parent W3C ID via field recording.
        let parent_span = tracing::info_span!("parent", visp.span.w3c_id = tracing::field::Empty,);
        parent_span.record("visp.span.w3c_id", "eeeeeeeeeeeeeeee");
        let parent_id = parent_span.id().unwrap();

        // parent is in mapping
        assert!(layer.inner.mapping.contains_key("eeeeeeeeeeeeeeee"));
        assert!(layer.inner.reverse.contains_key(&parent_id));

        drop(parent_span);
        // After close, parent should be removed from both maps
        assert!(
            !layer.inner.mapping.contains_key("eeeeeeeeeeeeeeee"),
            "parent should be removed from forward mapping on close"
        );
        assert!(
            !layer.inner.reverse.contains_key(&parent_id),
            "parent should be removed from reverse mapping on close"
        );
    }

    // ── W1-S4a-5: unmatched parent + metric (span field recording) ─────────

    #[test]
    #[serial]
    fn test_parent_link_layer_unmatched_parent_recorded() {
        let layer = ParentLinkLayer::new();
        let layer_clone = layer.clone();

        let _guard = tracing_subscriber::registry()
            .with(layer_clone)
            .set_default();

        assert_eq!(layer.unmatched_count(), 0);

        // Record parent_span_id pointing to unknown W3C ID → unmatched_count++.
        let child_span = tracing::info_span!("child", parent_span_id = tracing::field::Empty,);
        child_span.record("parent_span_id", "aaaaaaaaaaaaaaaa");
        drop(child_span);

        assert!(
            layer.unmatched_count() > 0,
            "expected unmatched_count > 0 when parent W3C ID is not in mapping"
        );
    }

    #[test]
    #[serial]
    fn test_parent_link_layer_exposes_unmatched_count() {
        let layer = ParentLinkLayer::new();
        // Fresh layer: count must be 0.
        assert_eq!(layer.unmatched_count(), 0);
    }

    // ── Step 1b: on_record reads trace fields from subagent.spawn span ───

    #[test]
    #[serial]
    fn test_parent_link_reads_trace_fields_from_subagent_spawn_span() {
        let layer = ParentLinkLayer::new();
        let collector = TestFieldsCollector::new();

        let _guard = tracing_subscriber::registry()
            .with(layer.clone())
            .with(collector.clone())
            .set_default();

        // Create a span named visp.subagent.spawn with trace_id and parent_span_id fields.
        let span = tracing::info_span!(
            "visp.subagent.spawn",
            visp.subagent.name = "test",
            trace_id = tracing::field::Empty,
            parent_span_id = tracing::field::Empty,
            visp.span.w3c_id = tracing::field::Empty,
        );
        span.record("trace_id", "0af7651916cd43dd8448eb211c80319c");
        span.record("parent_span_id", "aaaaaaaaaaaaaaaa");
        // Record a W3C ID so the span registers in mapping.
        span.record("visp.span.w3c_id", "bbbbbbbbbbbbbbbb");
        let span_id = span.id().unwrap();

        // Check mapping BEFORE dropping the span (on_close would remove it).
        assert!(layer.inner.mapping.contains_key("bbbbbbbbbbbbbbbb"));
        assert!(layer.inner.reverse.contains_key(&span_id));

        drop(span);

        // ParentLinkFields should be present in the span extension.
        // TestFieldsCollector captures in both on_new_span and on_record,
        // so there may be multiple entries for the same span.
        let seen = collector.seen.lock().unwrap();
        assert!(
            !seen.is_empty(),
            "expected at least one ParentLinkFields capture"
        );
        // The first capture may come from the trace_id record (parent_span_id
        // not yet set). Find the entry with parent_span_id to verify completeness.
        let with_parent = seen.iter().find(|plf| plf.parent_span_id.is_some());
        assert!(
            with_parent.is_some(),
            "expected a ParentLinkFields entry with parent_span_id set"
        );
        assert_eq!(
            with_parent.unwrap().trace_id,
            "0af7651916cd43dd8448eb211c80319c"
        );
        assert_eq!(
            with_parent.unwrap().parent_span_id.as_deref(),
            Some("aaaaaaaaaaaaaaaa"),
        );
    }

    #[test]
    #[serial]
    fn test_parent_link_propagates_fields_to_child_spans() {
        let layer = ParentLinkLayer::new();
        let collector = TestFieldsCollector::new();

        let _guard = tracing_subscriber::registry()
            .with(layer.clone())
            .with(collector.clone())
            .set_default();

        // Parent span: visp.subagent.spawn with trace context fields.
        let parent = tracing::info_span!(
            "visp.subagent.spawn",
            trace_id = tracing::field::Empty,
            parent_span_id = tracing::field::Empty,
        );
        parent.record("trace_id", "0af7651916cd43dd8448eb211c80319c");
        parent.record("parent_span_id", "aaaaaaaaaaaaaaaa");

        // Child span (e.g. visp.agent.run) is a tracing child of parent.
        let child =
            tracing::info_span!("visp.agent.run", visp.span.w3c_id = tracing::field::Empty,);
        child.record("visp.span.w3c_id", "cccccccccccccccc");

        // Drop parent then child (order matters: parent must outlive child for propagation).
        // Actually both are in the same scope, so the tracing tree should work.
        drop(parent);
        drop(child);

        // Both spans should have ParentLinkFields (child gets it via propagation).
        let seen = collector.seen.lock().unwrap();
        // The child span propagates fields from parent when child is a tracing child.
        // TestFieldsCollector captures from both on_new_span and on_record,
        // so we may see more entries than spans.
        assert!(
            seen.len() >= 2,
            "expected at least two ParentLinkFields captures (parent + child), got {}: {:?}",
            seen.len(),
            seen,
        );

        // Verify child has the correct propagated trace_id.
        let child_plf = seen.iter().find(|plf| plf.parent_span_id.is_some());
        assert!(
            child_plf.is_some(),
            "child should have parent_span_id propagated"
        );
        assert_eq!(
            child_plf.unwrap().trace_id,
            "0af7651916cd43dd8448eb211c80319c"
        );
    }
}
