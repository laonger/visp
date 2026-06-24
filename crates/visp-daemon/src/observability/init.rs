//! init_observability — subscriber assembly for visp-daemon.
//!
//! Orchestrates EnvFilter, ParentLinkLayer, MetricsLayer, and fmt layer
//! into a single tracing subscriber stack with JSON or pretty output.
//! Refs: design §7.3, plan §Step 5.

use std::sync::Arc;

use opentelemetry::trace::TracerProvider;
use opentelemetry_sdk::trace::SdkTracerProvider;
use tracing_opentelemetry::OpenTelemetryLayer;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::prelude::*;

use crate::config::ObservabilityConfig;
use crate::observability::metrics_layer::MetricsLayer;
use crate::observability::otlp;
use crate::observability::parent_link::ParentLinkLayer;

/// Output of [`init_observability`]; held for the lifetime of the program.
// Not yet wired into main.rs; Step 5-e2e will assemble it.
#[allow(dead_code)]
pub struct ObservabilityGuard {
    pub metrics: Option<Arc<MetricsLayer>>,
    pub parent_link: Option<Arc<ParentLinkLayer>>,
    /// OTel tracer provider; shut down on drop if set.
    pub tracer_provider: Option<SdkTracerProvider>,
    _file_guard: Option<tracing_appender::non_blocking::WorkerGuard>,
    _set_default: Option<tracing::subscriber::DefaultGuard>,
}

/// Initialise the tracing subscriber stack from configuration.
///
/// Assembly order: EnvFilter → ParentLinkLayer → MetricsLayer → fmt layer.
/// Layers are always registered (they are lightweight noops when unused);
/// only the guard handles are conditional on config.
///
/// Returns a guard that unwinds the subscriber when dropped.
///
/// # Panics
/// If `set_default` fails (e.g., a subscriber was already set globally).
#[allow(dead_code)]
pub fn init_observability(cfg: &ObservabilityConfig) -> ObservabilityGuard {
    if !cfg.enabled {
        return ObservabilityGuard {
            metrics: None,
            parent_link: None,
            tracer_provider: None,
            _file_guard: None,
            _set_default: None,
        };
    }

    // 1. EnvFilter: try RUST_LOG env var, fall back to cfg.level.
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(&cfg.level));

    // 2. Always create layers (lightweight noops when unused).
    let parent_link = if cfg.otlp.enabled {
        ParentLinkLayer::with_otel_mode(true)
    } else {
        ParentLinkLayer::new()
    };
    let metrics = MetricsLayer::new();

    // 3. File output (optional).
    let _file_guard: Option<tracing_appender::non_blocking::WorkerGuard>;
    let fmt_writer: Box<dyn Fn() -> Box<dyn std::io::Write + Send> + Send + Sync>;
    if let Some(ref path) = cfg.log_file {
        let appender = tracing_appender::rolling::daily(path, "visp-daemon.log");
        let (nb, guard) = tracing_appender::non_blocking(appender);
        _file_guard = Some(guard);
        fmt_writer = Box::new(move || -> Box<dyn std::io::Write + Send> { Box::new(nb.clone()) });
    } else {
        _file_guard = None;
        fmt_writer = Box::new(|| -> Box<dyn std::io::Write + Send> { Box::new(std::io::stdout()) });
    }

    // 4. Build OTel layer conditionally.
    let tracer_provider: Option<SdkTracerProvider>;
    let otel_layer = if cfg.otlp.enabled {
        let provider = otlp::build_tracer_provider(&cfg.otlp);
        let tracer = provider.tracer("visp-daemon");
        tracer_provider = Some(provider);
        Some(OpenTelemetryLayer::new(tracer).with_context_activation(true))
    } else {
        tracer_provider = None;
        None::<OpenTelemetryLayer<_, _>>
    };

    // 5. Assemble subscriber (json vs pretty produce different concrete types).
    //
    // Assembly order: EnvFilter → OTelLayer? → ParentLinkLayer → MetricsLayer → fmt.
    // OTel layer must be outer to ParentLinkLayer (registered first = outer),
    // so ParentLinkLayer.on_enter sees the OTel-fixed SpanContext.
    let _set_default = if cfg.format == "json" {
        tracing_subscriber::registry()
            .with(filter)
            .with(otel_layer)
            .with(parent_link.clone())
            .with(metrics.clone())
            .with(
                tracing_subscriber::fmt::layer()
                    .json()
                    .with_writer(fmt_writer),
            )
            .set_default()
    } else {
        tracing_subscriber::registry()
            .with(filter)
            .with(otel_layer)
            .with(parent_link.clone())
            .with(metrics.clone())
            .with(
                tracing_subscriber::fmt::layer()
                    .pretty()
                    .with_writer(fmt_writer),
            )
            .set_default()
    };

    ObservabilityGuard {
        metrics: cfg.metrics_summary.then(|| Arc::new(metrics)),
        parent_link: cfg.parent_link.then(|| Arc::new(parent_link)),
        tracer_provider,
        _file_guard,
        _set_default: Some(_set_default),
    }
}

/// Like [`init_observability`] but accepts a custom [`MakeWriter`] for testing.
///
/// This allows integration tests to capture JSON output in memory without
/// writing to stdout or a file.  All other assembly (EnvFilter, ParentLinkLayer,
/// MetricsLayer) is identical to `init_observability`.
///
/// When `cfg.enabled` is `false`, a no-op guard is returned and the writer
/// is **not** used (mirrors the production behaviour).
///
/// # Panics
/// If `set_default` fails (e.g., a subscriber was already set globally).
#[allow(dead_code)]
pub fn init_observability_with_writer<W>(cfg: &ObservabilityConfig, writer: W) -> ObservabilityGuard
where
    W: for<'a> tracing_subscriber::fmt::MakeWriter<'a> + Send + Sync + 'static,
{
    if !cfg.enabled {
        return ObservabilityGuard {
            metrics: None,
            parent_link: None,
            tracer_provider: None,
            _file_guard: None,
            _set_default: None,
        };
    }

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(&cfg.level));
    let parent_link = if cfg.otlp.enabled {
        ParentLinkLayer::with_otel_mode(true)
    } else {
        ParentLinkLayer::new()
    };
    let metrics = MetricsLayer::new();

    let tracer_provider: Option<SdkTracerProvider>;
    let otel_layer = if cfg.otlp.enabled {
        let provider = otlp::build_tracer_provider(&cfg.otlp);
        let tracer = provider.tracer("visp-daemon");
        tracer_provider = Some(provider);
        Some(OpenTelemetryLayer::new(tracer).with_context_activation(true))
    } else {
        tracer_provider = None;
        None::<OpenTelemetryLayer<_, _>>
    };

    let _set_default = if cfg.format == "json" {
        tracing_subscriber::registry()
            .with(filter)
            .with(otel_layer)
            .with(parent_link.clone())
            .with(metrics.clone())
            .with(tracing_subscriber::fmt::layer().json().with_writer(writer))
            .set_default()
    } else {
        tracing_subscriber::registry()
            .with(filter)
            .with(otel_layer)
            .with(parent_link.clone())
            .with(metrics.clone())
            .with(
                tracing_subscriber::fmt::layer()
                    .pretty()
                    .with_writer(writer),
            )
            .set_default()
    };

    ObservabilityGuard {
        metrics: cfg.metrics_summary.then(|| Arc::new(metrics)),
        parent_link: cfg.parent_link.then(|| Arc::new(parent_link)),
        tracer_provider,
        _file_guard: None,
        _set_default: Some(_set_default),
    }
}

impl Drop for ObservabilityGuard {
    fn drop(&mut self) {
        if let Some(provider) = self.tracer_provider.take() {
            let _ = provider.shutdown();
        }
    }
}

/// Test-only entry point that accepts a custom exporter for verifying OTel integration.
///
/// Mirrors [`init_observability_with_writer`] but additionally wires an OTel
/// layer using the given exporter (via [`otlp::build_tracer_provider_with_exporter`]).
/// The exporter is expected to be an `InMemorySpanExporter` in tests.
///
/// Returns a guard whose `tracer_provider` field holds the constructed provider.
#[cfg(test)]
#[allow(dead_code)]
pub(crate) fn init_observability_with_exporter<W, E>(
    cfg: &ObservabilityConfig,
    writer: W,
    exporter: E,
) -> ObservabilityGuard
where
    W: for<'a> tracing_subscriber::fmt::MakeWriter<'a> + Send + Sync + 'static,
    E: opentelemetry_sdk::trace::SpanExporter + 'static,
{
    if !cfg.enabled {
        return ObservabilityGuard {
            metrics: None,
            parent_link: None,
            tracer_provider: None,
            _file_guard: None,
            _set_default: None,
        };
    }

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(&cfg.level));
    let parent_link = if cfg.otlp.enabled {
        ParentLinkLayer::with_otel_mode(true)
    } else {
        ParentLinkLayer::new()
    };
    let metrics = MetricsLayer::new();

    let tracer_provider: Option<SdkTracerProvider>;
    let otel_layer = if cfg.otlp.enabled {
        let provider = otlp::build_tracer_provider_with_exporter(exporter, &cfg.otlp);
        let tracer = provider.tracer("visp-daemon");
        tracer_provider = Some(provider);
        Some(OpenTelemetryLayer::new(tracer).with_context_activation(true))
    } else {
        tracer_provider = None;
        None::<OpenTelemetryLayer<_, _>>
    };

    let _set_default = if cfg.format == "json" {
        tracing_subscriber::registry()
            .with(filter)
            .with(otel_layer)
            .with(parent_link.clone())
            .with(metrics.clone())
            .with(tracing_subscriber::fmt::layer().json().with_writer(writer))
            .set_default()
    } else {
        tracing_subscriber::registry()
            .with(filter)
            .with(otel_layer)
            .with(parent_link.clone())
            .with(metrics.clone())
            .with(
                tracing_subscriber::fmt::layer()
                    .pretty()
                    .with_writer(writer),
            )
            .set_default()
    };

    ObservabilityGuard {
        metrics: cfg.metrics_summary.then(|| Arc::new(metrics)),
        parent_link: cfg.parent_link.then(|| Arc::new(parent_link)),
        tracer_provider,
        _file_guard: None,
        _set_default: Some(_set_default),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::io;
    use std::sync::Arc;
    use std::sync::Mutex;

    use serial_test::serial;
    use tracing_subscriber::fmt::MakeWriter;
    use tracing_subscriber::prelude::*;

    use super::*;
    use crate::config::{ObservabilityConfig, OtlpConfig};

    /// In-memory writer that captures bytes via `Arc<Mutex<Vec<u8>>>`.
    #[derive(Clone)]
    struct TestVecWriter {
        buf: Arc<Mutex<Vec<u8>>>,
    }

    impl TestVecWriter {
        fn new() -> Self {
            Self {
                buf: Arc::new(Mutex::new(Vec::new())),
            }
        }

        fn into_string(self) -> String {
            String::from_utf8(self.buf.lock().unwrap().clone()).unwrap_or_default()
        }
    }

    impl<'a> MakeWriter<'a> for TestVecWriter {
        type Writer = Self;

        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    impl io::Write for TestVecWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.buf.lock().unwrap().write(buf)
        }

        fn flush(&mut self) -> io::Result<()> {
            self.buf.lock().unwrap().flush()
        }
    }

    /// Build a test subscriber with custom writer (mirrors init_observability
    /// assembly logic but uses the test writer instead of stdout/file).
    fn make_test_subscriber(
        writer: TestVecWriter,
        cfg: &ObservabilityConfig,
    ) -> tracing::subscriber::DefaultGuard {
        let filter =
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(&cfg.level));

        let parent_link = ParentLinkLayer::new();
        let metrics = MetricsLayer::new();

        if cfg.format == "json" {
            tracing_subscriber::registry()
                .with(filter)
                .with(parent_link)
                .with(metrics)
                .with(tracing_subscriber::fmt::layer().json().with_writer(writer))
                .set_default()
        } else {
            tracing_subscriber::registry()
                .with(filter)
                .with(parent_link)
                .with(metrics)
                .with(
                    tracing_subscriber::fmt::layer()
                        .pretty()
                        .with_writer(writer),
                )
                .set_default()
        }
    }

    // ------------------------------------------------------------------
    // Red + Green: disabled / noop guard
    // ------------------------------------------------------------------

    #[test]
    #[serial]
    fn test_init_observability_disabled_returns_noop_guard() {
        let cfg = ObservabilityConfig {
            enabled: false,
            ..Default::default()
        };
        let guard = init_observability(&cfg);

        assert!(guard.metrics.is_none());
        assert!(guard.parent_link.is_none());

        // Calling init_observability again with disabled should not panic.
        let guard2 = init_observability(&cfg);
        assert!(guard2.metrics.is_none());
        assert!(guard2.parent_link.is_none());
    }

    // ------------------------------------------------------------------
    // Red + Green: handles (guard fields)
    // ------------------------------------------------------------------

    #[test]
    #[serial]
    fn test_init_observability_returns_metrics_layer_handle() {
        let cfg = ObservabilityConfig {
            enabled: true,
            metrics_summary: true,
            parent_link: false,
            ..Default::default()
        };
        let guard = init_observability(&cfg);
        assert!(guard.metrics.is_some(), "expected metrics handle");
        assert!(guard.parent_link.is_none());
    }

    #[test]
    #[serial]
    fn test_init_observability_returns_parent_link_handle() {
        let cfg = ObservabilityConfig {
            enabled: true,
            metrics_summary: false,
            parent_link: true,
            ..Default::default()
        };
        let guard = init_observability(&cfg);
        assert!(guard.parent_link.is_some(), "expected parent_link handle");
        assert!(guard.metrics.is_none());
    }

    #[test]
    #[serial]
    fn test_init_observability_layers_disabled_individually() {
        let cfg = ObservabilityConfig {
            enabled: true,
            parent_link: false,
            metrics_summary: false,
            ..Default::default()
        };
        let guard = init_observability(&cfg);
        assert!(
            guard.parent_link.is_none(),
            "parent_link should be None when disabled"
        );
        assert!(
            guard.metrics.is_none(),
            "metrics should be None when disabled"
        );
    }

    // ------------------------------------------------------------------
    // Red + Green: JSON format output is valid JSON
    // ------------------------------------------------------------------

    #[test]
    #[serial]
    fn test_init_observability_json_format() {
        let writer = TestVecWriter::new();
        let cfg = ObservabilityConfig {
            enabled: true,
            format: "json".into(),
            parent_link: false,
            metrics_summary: false,
            log_file: None,
            ..Default::default()
        };
        let _guard = make_test_subscriber(writer.clone(), &cfg);

        tracing::info!("hello json");

        let output = writer.into_string();
        assert!(!output.is_empty(), "expected non-empty JSON output");

        // Each line must be valid JSON.
        for line in output.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let parsed: serde_json::Value =
                serde_json::from_str(line).expect("each output line should be valid JSON");
            assert!(parsed.is_object(), "each JSON line should be an object");
        }
    }

    // ------------------------------------------------------------------
    // Red + Green: pretty format output (weak assertion: >1 line or msg)
    // ------------------------------------------------------------------

    #[test]
    #[serial]
    fn test_init_observability_pretty_format() {
        let writer = TestVecWriter::new();
        let cfg = ObservabilityConfig {
            enabled: true,
            format: "pretty".into(),
            parent_link: false,
            metrics_summary: false,
            log_file: None,
            ..Default::default()
        };
        let _guard = make_test_subscriber(writer.clone(), &cfg);

        tracing::info!("hello pretty");

        let output = writer.into_string();
        assert!(!output.is_empty(), "expected non-empty pretty output");
        assert!(
            output.contains("hello pretty") || output.lines().count() > 1,
            "pretty output should contain the message or span multiple lines"
        );
    }

    // ------------------------------------------------------------------
    // Red + Green: EnvFilter level applies
    // ------------------------------------------------------------------

    #[test]
    #[serial]
    fn test_init_observability_level_applies_env_filter() {
        let writer = TestVecWriter::new();
        let cfg = ObservabilityConfig {
            enabled: true,
            level: "debug".into(),
            format: "json".into(),
            parent_link: false,
            metrics_summary: false,
            log_file: None,
            ..Default::default()
        };
        let _guard = make_test_subscriber(writer.clone(), &cfg);

        tracing::debug!("this is a debug message");
        tracing::error!("this is an error message");

        let output = writer.into_string();
        let line_count = output.lines().filter(|l| !l.trim().is_empty()).count();
        assert!(
            line_count >= 2,
            "expected at least 2 events (debug + error), got {} lines: {:?}",
            line_count,
            output
        );
    }

    // ------------------------------------------------------------------
    // Red + Green: end-to-end JSON includes trace_id from ParentLinkLayer
    // ------------------------------------------------------------------

    #[test]
    #[serial]
    fn test_init_observability_json_includes_trace_id_field_for_subagent_span() {
        let writer = TestVecWriter::new();
        let parent_link_layer = ParentLinkLayer::new();

        let _guard = tracing_subscriber::registry()
            .with(tracing_subscriber::EnvFilter::new("info"))
            .with(parent_link_layer.clone())
            .with(
                tracing_subscriber::fmt::layer()
                    .json()
                    .with_writer(writer.clone()),
            )
            .set_default();

        // Create a visp.subagent.spawn span with trace context fields declared
        // and recorded via span field passing (matches orchestrator approach).
        let span = tracing::info_span!(
            "visp.subagent.spawn",
            visp.subagent.name = "test",
            trace_id = tracing::field::Empty,
            parent_span_id = tracing::field::Empty,
        );
        // Record trace context fields (ParentLinkLayer.on_record writes
        // ParentLinkFields into the extension).
        span.record("trace_id", "0af7651916cd43dd8448eb211c80319c");
        span.record("parent_span_id", "aaaaaaaaaaaaaaaa");
        {
            let _enter = span.enter();
            // on_enter triggers ParentLinkLayer to record fields from the
            // ParentLinkFields extension onto the span for JSON output.
            tracing::info!("inside span");
        }
        drop(span);

        let output = writer.into_string();
        assert!(!output.is_empty(), "expected JSON output");

        let trace_id_found = output.contains("trace_id");
        let parent_span_id_found = output.contains("parent_span_id");

        assert!(
            trace_id_found,
            "expected trace_id in JSON output.\nOutput:\n{}",
            output
        );
        assert!(
            parent_span_id_found,
            "expected parent_span_id in JSON output.\nOutput:\n{}",
            output
        );
    }

    // ------------------------------------------------------------------
    // W2-S2-2 红: 测试钩子 — 编译通过即视为通过
    // ------------------------------------------------------------------

    #[test]
    #[serial]
    fn test_init_with_exporter_function_exists() {
        // Simply verifying that `init_observability_with_exporter` exists and
        // compiles is sufficient for this test.
        let cfg = ObservabilityConfig {
            enabled: false,
            ..Default::default()
        };
        let writer = TestVecWriter::new();
        let exporter = opentelemetry_sdk::trace::InMemorySpanExporter::default();
        let guard = init_observability_with_exporter(&cfg, writer, exporter);
        assert!(guard.tracer_provider.is_none());
    }

    // ------------------------------------------------------------------
    // W2-S2-1: OTel 装配链单元测试
    // ------------------------------------------------------------------

    #[test]
    #[serial]
    fn test_otlp_disabled_no_otel_layer() {
        let cfg = ObservabilityConfig {
            enabled: true,
            otlp: OtlpConfig {
                enabled: false,
                ..Default::default()
            },
            format: "json".into(),
            parent_link: false,
            metrics_summary: false,
            log_file: None,
            ..Default::default()
        };
        let guard = init_observability(&cfg);
        assert!(
            guard.tracer_provider.is_none(),
            "tracer_provider should be None when OTel is disabled"
        );
    }

    #[test]
    #[serial]
    fn test_otlp_enabled_attaches_otel_layer() {
        let exporter = opentelemetry_sdk::trace::InMemorySpanExporter::default();
        let writer = TestVecWriter::new();
        let cfg = ObservabilityConfig {
            enabled: true,
            otlp: OtlpConfig {
                enabled: true,
                ..Default::default()
            },
            format: "json".into(),
            parent_link: false,
            metrics_summary: false,
            log_file: None,
            ..Default::default()
        };
        let guard = init_observability_with_exporter(&cfg, writer, exporter.clone());

        assert!(
            guard.tracer_provider.is_some(),
            "expected tracer_provider when OTel is enabled"
        );

        // Emit a span; with SimpleSpanProcessor it is exported synchronously.
        let span = tracing::info_span!("test_otel_span");
        drop(span);

        let finished = exporter
            .get_finished_spans()
            .expect("get_finished_spans should succeed");
        assert!(
            !finished.is_empty(),
            "expected at least one finished span with OTel enabled"
        );
        assert_eq!(
            finished[0].name.as_ref(),
            "test_otel_span",
            "span name should match"
        );
    }

    #[test]
    #[serial]
    fn test_otlp_resource_has_service_name() {
        let exporter = opentelemetry_sdk::trace::InMemorySpanExporter::default();
        let writer = TestVecWriter::new();
        let cfg = ObservabilityConfig {
            enabled: true,
            otlp: OtlpConfig {
                enabled: true,
                ..Default::default()
            },
            format: "json".into(),
            parent_link: false,
            metrics_summary: false,
            log_file: None,
            ..Default::default()
        };
        let _guard = init_observability_with_exporter(&cfg, writer, exporter.clone());

        let span = tracing::info_span!("otel_resource_test");
        drop(span);

        let finished = exporter
            .get_finished_spans()
            .expect("get_finished_spans should succeed");
        assert!(!finished.is_empty(), "expected at least one finished span");

        // Verify resource attributes via build_resource() directly,
        // since SpanData in opentelemetry_sdk 0.32 does not carry a `resource`
        // field on individual spans (resource is attached at the exporter level).
        let resource = otlp::build_resource();
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
    }

    #[test]
    #[serial]
    fn test_otlp_w1_layers_still_attached_when_enabled() {
        let exporter = opentelemetry_sdk::trace::InMemorySpanExporter::default();
        let writer = TestVecWriter::new();
        let cfg = ObservabilityConfig {
            enabled: true,
            otlp: OtlpConfig {
                enabled: true,
                ..Default::default()
            },
            format: "json".into(),
            parent_link: true,
            metrics_summary: false, // No metrics layer handle needed
            log_file: None,
            ..Default::default()
        };
        let guard = init_observability_with_exporter(&cfg, writer.clone(), exporter);

        // W1: ParentLinkLayer handle is still available.
        assert!(
            guard.parent_link.is_some(),
            "parent_link handle should be Some when parent_link=true"
        );

        // Emit a span with visp.span.w3c_id field to exercise ParentLinkLayer.
        let span = tracing::info_span!(
            "visp.subagent.spawn",
            visp.span.w3c_id = tracing::field::Empty,
            trace_id = tracing::field::Empty,
            parent_span_id = tracing::field::Empty,
        );
        span.record("visp.span.w3c_id", "w2_w1_test_12345678");
        span.record("trace_id", "0af7651916cd43dd8448eb211c80319c");
        span.record("parent_span_id", "aaaaaaaaaaaaaaaa");
        {
            let _enter = span.enter();
            tracing::info!("inside W1+OTel span");
        }
        drop(span);

        // Verify fmt JSON output still contains ParentLinkLayer fields.
        let output = writer.into_string();
        assert!(!output.is_empty(), "expected JSON output");

        // visp.span.w3c_id should appear in JSON fmt output
        // (ParentLinkLayer records it onto the span during on_enter).
        assert!(
            output.contains("visp.span.w3c_id"),
            "visp.span.w3c_id should appear in JSON output when ParentLinkLayer is attached.\nOutput:\n{}",
            output
        );
    }
}
