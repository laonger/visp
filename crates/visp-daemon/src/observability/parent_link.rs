//! ParentLinkLayer — cross-mpsc span parent_span_id field completion.
//!
//! Wave 1: field complement only, does **not** modify the tracing tree parent.
//! Refs: design §7.3, plan §Step 4a.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use dashmap::DashMap;
use tracing::Subscriber;
use tracing_subscriber::layer::{Context, Layer};
use tracing_subscriber::registry::LookupSpan;

use visp_core::TraceContext;

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
    mapping: DashMap<String, tracing::span::Id>,
    unmatched_count: AtomicU64,
}

/// A tracing [`Layer`] that detects [`TraceContext`] in span extensions and
/// writes [`ParentLinkFields`] (trace_id + parent_span_id) into the span
/// extension for JSON field completion by a downstream fmt layer.
///
/// # Wave 1 scope
/// - Maintains a bidirectional mapping: W3C span_id ↔ tracing `span::Id`.
/// - Counts unmatched parent span IDs (for debugging / Wave 2 validation).
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
    pub fn remove_from_mapping(&self, span_id: &str) {
        self.inner.mapping.remove(span_id);
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
        // Phase 1: Read TraceContext, register mapping, compute parent_span_id.
        let (tc, parent_sid) = {
            let span_ref = match ctx.span(id) {
                Some(s) => s,
                None => return,
            };

            let tc = match span_ref.extensions().get::<TraceContext>().cloned() {
                Some(tc) => tc,
                None => return,
            };

            self.inner.mapping.insert(tc.span_id.clone(), id.clone());

            let parent_sid: Option<String> = match span_ref.parent() {
                Some(parent_ref) => parent_ref
                    .extensions()
                    .get::<TraceContext>()
                    .map(|ptc| ptc.span_id.clone()),
                None => None,
            };

            (tc, parent_sid)
        }; // span_ref dropped here — no outstanding borrows

        // Phase 2: Check parent presence in mapping (outside the read borrow).
        if let Some(ref psid) = parent_sid
            && !self.inner.mapping.contains_key(psid)
        {
            self.inner.unmatched_count.fetch_add(1, Ordering::Relaxed);
        }

        // Phase 3: Insert ParentLinkFields into the span extension.
        if let Some(span_ref) = ctx.span(id) {
            span_ref.extensions_mut().insert(ParentLinkFields {
                trace_id: tc.trace_id,
                parent_span_id: parent_sid,
            });
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

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
        )
        .unwrap()
    }

    // ── W1-S4a-1: Layer skeleton ──────────────────────────────────────────

    #[test]
    fn test_parent_link_layer_compiles_and_registers() {
        let _guard = tracing_subscriber::registry()
            .with(ParentLinkLayer::new())
            .set_default();
        // Compilation + registration is sufficient for this test.
    }

    #[test]
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
        // parent_span_id is None because there's no parent span with TraceContext
        assert_eq!(seen[0].parent_span_id, None);
    }

    #[test]
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

    // ── W1-S4a-5: unmatched parent + metric ──────────────────────────────

    #[test]
    fn test_parent_link_layer_unmatched_parent_recorded() {
        // Strategy: Use two selective injectors (one per span name) so that
        // parent gets TraceContext_A (span_id="aaa...") and child gets
        // TraceContext_B (span_id="bbb..."). Register parent, then remove it
        // from mapping. Create child — its on_new_span reads parent's
        // TraceContext from extension, computes parent_span_id="aaa...",
        // looks it up in mapping → not found → unmatched_count++.
        let layer = ParentLinkLayer::new();
        let layer_clone = layer.clone();
        let parent_tc = make_tc("aaaaaaaaaaaaaaaa");
        let child_tc = make_tc("bbbbbbbbbbbbbbbb");

        let _guard = tracing_subscriber::registry()
            .with(TestTcInjector::named(parent_tc, "parent"))
            .with(TestTcInjector::named(child_tc, "child"))
            .with(layer_clone)
            .set_default();

        // Create parent → registered with span_id "aaa..." in mapping.
        let parent_span = tracing::info_span!("parent");
        assert_eq!(layer.unmatched_count(), 0);

        // Remove parent's mapping entry to trigger a miss.
        layer.remove_from_mapping("aaaaaaaaaaaaaaaa");

        // Create child with explicit parent reference so that tracing tree
        // has parent←child relationship. Injector injects child_tc into the
        // child. ParentLinkLayer registers "bbb...", then reads parent's
        // extension → finds parent_tc → parent_span_id = "aaa...".
        // But "aaa..." was removed from mapping → unmatched_count++.
        let child_span = tracing::info_span!(parent: &parent_span, "child");
        drop(child_span);
        drop(parent_span);
        drop(_guard);

        assert!(
            layer.unmatched_count() > 0,
            "expected unmatched_count > 0 after parent was removed from mapping"
        );
    }

    #[test]
    fn test_parent_link_layer_exposes_unmatched_count() {
        let layer = ParentLinkLayer::new();
        // Fresh layer: count must be 0.
        assert_eq!(layer.unmatched_count(), 0);
    }
}
