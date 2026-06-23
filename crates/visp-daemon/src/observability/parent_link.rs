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

use visp_core::TraceContext;
use visp_core::trace_context::SpanW3CId;

/// Custom fields inserted into the span extension when a TraceContext is found.
// Not yet used by production code; Step 5-sub will wire it into the fmt layer.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct ParentLinkFields {
    pub trace_id: String,
    pub parent_span_id: Option<String>,
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
// W3C ID field extractor (used in on_record)
// ---------------------------------------------------------------------------

struct W3cIdExtractor {
    w3c_id: Option<String>,
}

impl Visit for W3cIdExtractor {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "visp.span.w3c_id" {
            self.w3c_id = Some(value.to_string());
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

        // Register mapping from SpanW3CId if present.
        let mut span_w3c_id: Option<String> = None;
        if let Some(sw) = span_ref.extensions().get::<SpanW3CId>().cloned() {
            span_w3c_id = Some(sw.0.clone());
            self.inner.mapping.insert(sw.0.clone(), id.clone());
            self.inner.reverse.insert(id.clone(), sw.0);
        }

        // Process TraceContext if present.
        let tc = match span_ref.extensions().get::<TraceContext>().cloned() {
            Some(tc) => tc,
            None => {
                // No TraceContext — nothing more to do.
                return;
            }
        };

        // Register the span_id from TraceContext (may differ from SpanW3CId).
        if span_w3c_id.as_deref() != Some(&tc.span_id) {
            self.inner.mapping.insert(tc.span_id.clone(), id.clone());
            self.inner.reverse.insert(id.clone(), tc.span_id.clone());
        }

        // Check parent_span_id from TraceContext.
        let parent_sid: Option<String> = match tc.parent_span_id {
            Some(ref psid) if !self.inner.mapping.contains_key(psid) => {
                self.inner.unmatched_count.fetch_add(1, Ordering::Relaxed);
                Some(psid.clone())
            }
            Some(psid) => Some(psid),
            None => None,
        };

        // Write ParentLinkFields.
        span_ref.extensions_mut().insert(ParentLinkFields {
            trace_id: tc.trace_id,
            parent_span_id: parent_sid,
        });
    }

    /// Detect `visp.span.w3c_id` field recordings and register the W3C ID
    /// in the mapping.  This handles spans created by `visp-core` which
    /// records the field (but cannot write extensions directly).
    fn on_record(
        &self,
        id: &tracing::span::Id,
        values: &tracing::span::Record<'_>,
        ctx: Context<'_, S>,
    ) {
        let mut extractor = W3cIdExtractor { w3c_id: None };
        values.record(&mut extractor);
        let w3c_id = match extractor.w3c_id {
            Some(id) => id,
            None => return,
        };

        // Write SpanW3CId into the extension so other layers (fmt etc.)
        // can also consume it.
        if let Some(span_ref) = ctx.span(id) {
            span_ref.extensions_mut().insert(SpanW3CId(w3c_id.clone()));
        }

        self.inner.mapping.insert(w3c_id.clone(), id.clone());
        self.inner.reverse.insert(id.clone(), w3c_id);
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
    use tracing::span::{Attributes, Id};
    use tracing_subscriber::Layer;
    use tracing_subscriber::layer::Context;
    use tracing_subscriber::prelude::*;
    use tracing_subscriber::registry::LookupSpan;

    use super::*;

    /// Injector layer that places a `TraceContext` into every new span's
    /// extension. Used in tests where we want the ParentLinkLayer to see it.
    #[derive(Clone)]
    struct TestTcInjector {
        tc: TraceContext,
        filter_name: Option<String>,
    }

    impl TestTcInjector {
        fn all(tc: TraceContext) -> Self {
            Self {
                tc,
                filter_name: None,
            }
        }

        #[allow(dead_code)]
        fn named(tc: TraceContext, name: &str) -> Self {
            Self {
                tc,
                filter_name: Some(name.to_string()),
            }
        }
    }

    impl<S> Layer<S> for TestTcInjector
    where
        S: Subscriber + for<'a> LookupSpan<'a>,
    {
        fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
            if let Some(ref name) = self.filter_name
                && attrs.metadata().name() != name
            {
                return;
            }
            if let Some(span_ref) = ctx.span(id) {
                span_ref.extensions_mut().insert(self.tc.clone());
            }
        }
    }

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
    /// `ParentLinkFields`.
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
    }

    fn make_tc(span_id: &str) -> TraceContext {
        TraceContext::new(
            "0af7651916cd43dd8448eb211c80319c".to_string(),
            span_id.to_string(),
            1,
            None,
            None,
        )
        .unwrap()
    }

    fn make_tc_with_parent(span_id: &str, parent_span_id: &str) -> TraceContext {
        TraceContext::new(
            "0af7651916cd43dd8448eb211c80319c".to_string(),
            span_id.to_string(),
            1,
            None,
            Some(parent_span_id.to_string()),
        )
        .unwrap()
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

    // ── W1-S4a-3 (rewritten): extension fields ────────────────────────────

    #[test]
    #[serial]
    fn test_parent_link_layer_inserts_parent_link_fields_extension() {
        let tc = make_tc("b7ad6b7169203331");
        let collector = TestFieldsCollector::new();

        let _guard = tracing_subscriber::registry()
            .with(TestTcInjector::all(tc.clone()))
            .with(ParentLinkLayer::new())
            .with(collector.clone())
            .set_default();

        let span = tracing::info_span!("visp.subagent.spawn");
        drop(span);

        let seen = collector.seen.lock().unwrap();
        assert_eq!(
            seen.len(),
            1,
            "expected exactly one span with ParentLinkFields"
        );
        assert_eq!(seen[0].trace_id, "0af7651916cd43dd8448eb211c80319c");
        // parent_span_id is None because the TraceContext has no parent_span_id
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

    // ── P0-3: parent_span_id via mapping ──────────────────────────────────

    #[test]
    #[serial]
    fn test_parent_link_writes_parent_span_id_when_parent_in_mapping() {
        let layer = ParentLinkLayer::new();
        let collector = TestFieldsCollector::new();

        // Parent span is injected with SpanW3CId → registered in mapping.
        // Child span is injected with TraceContext that has parent_span_id
        // pointing to the parent's W3C ID.
        // Parent MUST stay alive while child is created (in real usage the
        // iteration span is alive when the subagent.spawn span is created).
        let parent_w3c = "aaaaaaaaaaaaaaaa";
        let child_tc = make_tc_with_parent("bbbbbbbbbbbbbbbb", parent_w3c);

        let _guard = tracing_subscriber::registry()
            .with(TestW3CIdInjector::new(parent_w3c))
            .with(TestTcInjector::named(child_tc, "child"))
            .with(layer.clone())
            .with(collector.clone())
            .set_default();

        // Create parent span → registers "aaa..." in mapping.
        // Keep it alive while child is being created.
        let parent_span = tracing::info_span!("parent");
        let _parent_enter = parent_span.enter();

        // Create child span → TraceContext has parent_span_id="aaa..."
        // which IS in mapping → ParentLinkFields.parent_span_id = Some("aaa...")
        let child_span = tracing::info_span!("child");
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

        // Child has parent_span_id="aaa..." but no span registered it.
        let child_tc = make_tc_with_parent("bbbbbbbbbbbbbbbb", "aaaaaaaaaaaaaaaa");

        let _guard = tracing_subscriber::registry()
            .with(TestTcInjector::named(child_tc, "child"))
            .with(layer.clone())
            .set_default();

        assert_eq!(layer.unmatched_count(), 0);

        let child_span = tracing::info_span!("child");
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

        // Inject SpanW3CId into a span → layer registers it in mapping.
        // Then inject a child TraceContext that references it.
        // Parent must stay alive while child is created.
        let parent_w3c = "cccccccccccccccc";
        let child_tc = make_tc_with_parent("dddddddddddddddd", parent_w3c);

        let _guard = tracing_subscriber::registry()
            .with(TestW3CIdInjector::new(parent_w3c))
            .with(TestTcInjector::named(child_tc, "child"))
            .with(layer.clone())
            .with(collector.clone())
            .set_default();

        // Parent span creates mapping via SpanW3CId extension
        let parent_span = tracing::info_span!("parent");
        let _parent_enter = parent_span.enter();

        // Child span resolves parent via mapping
        let child_span = tracing::info_span!("child");
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

        let tc_parent = make_tc("eeeeeeeeeeeeeeee");
        let tc_child = make_tc_with_parent("ffffffffffffffff", "eeeeeeeeeeeeeeee");

        let _guard = tracing_subscriber::registry()
            .with(TestTcInjector::named(tc_parent, "parent"))
            .with(TestTcInjector::named(tc_child, "child"))
            .with(layer.clone())
            .set_default();

        // Create and drop parent → mapping has "eeee..."
        let parent_span = tracing::info_span!("parent");
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

    // ── W1-S4a-5: unmatched parent + metric ──────────────────────────────

    #[test]
    #[serial]
    fn test_parent_link_layer_unmatched_parent_recorded() {
        let layer = ParentLinkLayer::new();
        let layer_clone = layer.clone();
        // Child TraceContext has parent_span_id="aaa..." pointing to parent.
        let child_tc = make_tc_with_parent("bbbbbbbbbbbbbbbb", "aaaaaaaaaaaaaaaa");

        // Only inject into "child" spans (no parent injector).
        let _guard = tracing_subscriber::registry()
            .with(TestTcInjector::named(child_tc, "child"))
            .with(layer_clone)
            .set_default();

        assert_eq!(layer.unmatched_count(), 0);

        // Create child span. Its TraceContext has parent_span_id="aaa..."
        // but "aaa..." was never registered in the mapping → unmatched_count++.
        let child_span = tracing::info_span!("child");
        drop(child_span);
        drop(_guard);

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
}
