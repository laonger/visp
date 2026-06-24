// OTel attach Context POC — 验证 3 种 trace_id 继承方案
// 独立 binary，不进生产代码

use opentelemetry::Context;
use opentelemetry::trace::{
    SpanContext, SpanId, TraceContextExt, TraceFlags, TraceId, TraceState, TracerProvider,
};
use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider, SimpleSpanProcessor};
use tracing::{info, info_span};
use tracing_opentelemetry::OpenTelemetryLayer;
use tracing_subscriber::Registry;
use tracing_subscriber::prelude::*;

fn make_subscriber(
    exporter: InMemorySpanExporter,
) -> (tracing::subscriber::DefaultGuard, SdkTracerProvider) {
    let provider = SdkTracerProvider::builder()
        .with_span_processor(SimpleSpanProcessor::new(exporter.clone()))
        .build();
    let tracer = provider.tracer("otel_poc");

    let subscriber = Registry::default()
        .with(OpenTelemetryLayer::new(tracer).with_context_activation(true))
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::sink));

    let guard = tracing::subscriber::set_default(subscriber);
    (guard, provider)
}

fn make_remote_sc() -> SpanContext {
    SpanContext::new(
        TraceId::from_bytes([0xAA; 16]),
        SpanId::from_bytes([0xBB; 8]),
        TraceFlags::SAMPLED,
        true, // is_remote
        TraceState::default(),
    )
}

fn hex_trace_id(sc: &opentelemetry_sdk::trace::SpanData) -> String {
    format!("{:032x}", sc.span_context.trace_id())
}

fn hex_parent_span_id(sc: &opentelemetry_sdk::trace::SpanData) -> String {
    format!("{:016x}", sc.parent_span_id)
}

fn main() {
    // ===================== Case 1: attach Context → create span =====================
    println!();
    println!("=== Case 1: attach Context → create span ===");
    println!("方案: 通过 Context::with_remote_span_context + ctx.attach() 设置远端上下文");
    println!("       然后在 guard 存活期间创建 span");
    {
        let exporter = InMemorySpanExporter::default();
        let (_guard, provider) = make_subscriber(exporter.clone());

        // 构造远端 SpanContext (trace_id = 0xAA...AA)
        let remote_sc = make_remote_sc();
        let ctx = Context::current().with_remote_span_context(remote_sc);
        let _ctx_guard = ctx.attach();

        // 创建 span — 此时 subscriber 通过 context activation 感知到远端上下文
        let span = info_span!("case1_root");
        let _e = span.enter();
        info!("hello from case 1");
        drop(_e);
        drop(span);
        drop(_ctx_guard);

        provider.force_flush().unwrap();
        let spans = exporter.get_finished_spans().unwrap();
        for s in &spans {
            println!(
                "[case 1] trace_id={}  parent_span_id={}  name={:?}",
                hex_trace_id(s),
                hex_parent_span_id(s),
                s.name,
            );
        }
    }

    // ===================== Case 2: 不 attach，直接创建 span（对照） =====================
    println!();
    println!("=== Case 2: 不 attach，直接创建 span（对照） ===");
    println!("方案: 跳过 attach 步骤，直接创建 span");
    {
        let exporter = InMemorySpanExporter::default();
        let (_guard, provider) = make_subscriber(exporter.clone());

        // 不 attach 任何远端上下文
        let span = info_span!("case2_root");
        let _e = span.enter();
        info!("hello from case 2");
        drop(_e);
        drop(span);

        provider.force_flush().unwrap();
        let spans = exporter.get_finished_spans().unwrap();
        for s in &spans {
            println!(
                "[case 2] trace_id={}  parent_span_id={}  name={:?}",
                hex_trace_id(s),
                hex_parent_span_id(s),
                s.name,
            );
        }
    }

    // ===================== Case 3: 先创建 span 再 set_parent =====================
    println!();
    println!("=== Case 3: 先创建 span 再 set_parent ===");
    println!("方案: 先创建 span，再调用 span.set_parent(remote_ctx)");
    {
        let exporter = InMemorySpanExporter::default();
        let (_guard, provider) = make_subscriber(exporter.clone());

        let remote_sc = make_remote_sc();
        let ctx = Context::current().with_remote_span_context(remote_sc);

        // 先创建 span
        let span = info_span!("case3_root");
        // 再 set_parent — 能否覆盖 trace_id?
        use tracing_opentelemetry::OpenTelemetrySpanExt;
        let _ = span.set_parent(ctx);

        let _e = span.enter();
        info!("hello from case 3");
        drop(_e);
        drop(span);

        provider.force_flush().unwrap();
        let spans = exporter.get_finished_spans().unwrap();
        for s in &spans {
            println!(
                "[case 3] trace_id={}  parent_span_id={}  name={:?}",
                hex_trace_id(s),
                hex_parent_span_id(s),
                s.name,
            );
        }
    }
}
