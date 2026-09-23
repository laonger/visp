//! Step 2b SDK 冒烟：最小 ACP agent 的 initialize 往返、stdout 纪律、v1 守卫。
//!
//! 用内存 duplex 管道替代 stdio 驱动 agent,可精确断言「流上每行均为合法 JSON」
//! (ACP stdout 纪律)。

use agent_client_protocol::ByteStreams;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

const INIT_REQUEST: &str = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":1,"clientCapabilities":{}}}"#;

/// 启动 agent 并返回测试侧管道。
fn spawn_agent() -> (
    tokio::io::DuplexStream,
    tokio::task::JoinHandle<anyhow::Result<()>>,
) {
    let (test_side, agent_side) = tokio::io::duplex(64 * 1024);
    // SDK 的 ByteStreams 用 futures 的 AsyncRead/AsyncWrite,需 compat 桥接 tokio half
    use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};
    let (incoming, outgoing) = tokio::io::split(agent_side);
    let state = visp_acp::agent::AgentState::new_unconnected();
    let handle = tokio::spawn(async move {
        visp_acp::agent::run_agent(
            ByteStreams::new(outgoing.compat_write(), incoming.compat()),
            state,
        )
        .await
    });
    (test_side, handle)
}

async fn send<W: tokio::io::AsyncWrite + Unpin>(w: &mut W, msg: &str) {
    w.write_all(msg.as_bytes()).await.unwrap();
    w.write_all(b"\n").await.unwrap();
}

/// 读取一行并断言其为合法 JSON(ACP stdout 纪律的核心断言)。
async fn recv<R: tokio::io::AsyncRead + Unpin>(reader: &mut BufReader<R>) -> serde_json::Value {
    let mut line = String::new();
    let n = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        reader.read_line(&mut line),
    )
    .await
    .expect("read within 5s")
    .expect("stream open");
    assert!(n > 0, "EOF before a response arrived");
    serde_json::from_str(line.trim()).expect("frame must be valid JSON")
}

#[tokio::test]
async fn initialize_roundtrip_returns_v1_caps_and_agent_info() {
    let (test_side, agent_handle) = spawn_agent();
    let (r_half, mut w_half) = tokio::io::split(test_side);
    let mut reader = BufReader::new(r_half);

    send(&mut w_half, INIT_REQUEST).await;
    let v = recv(&mut reader).await;
    assert_eq!(v["jsonrpc"], "2.0");
    assert_eq!(v["id"], 1);
    assert_eq!(v["result"]["protocolVersion"], 1, "协议版本应回显 v1");
    assert_eq!(
        v["result"]["authMethods"],
        serde_json::json!([]),
        "M1 无交互式认证"
    );
    assert_eq!(
        v["result"]["agentInfo"]["name"], "visp",
        "agentInfo.name 应为 visp"
    );

    // 收尾:关闭测试侧管道,agent 事件循环应随 EOF 结束
    drop(reader);
    drop(w_half);
    agent_handle.await.unwrap().unwrap();
}

#[tokio::test]
async fn every_line_on_the_stream_is_valid_json() {
    let (test_side, agent_handle) = spawn_agent();
    let (r_half, mut w_half) = tokio::io::split(test_side);
    let mut reader = BufReader::new(r_half);

    // 连发两条请求(initialize + 未知方法),逐一读取:两帧均须为合法 JSON
    send(&mut w_half, INIT_REQUEST).await;
    send(
        &mut w_half,
        r#"{"jsonrpc":"2.0","id":2,"method":"bogus/method","params":{}}"#,
    )
    .await;

    let v1 = recv(&mut reader).await;
    assert_eq!(v1["id"], 1);
    assert!(v1["result"].is_object(), "initialize 应有 result");

    let v2 = recv(&mut reader).await;
    assert_eq!(v2["id"], 2);
    assert!(
        v2["error"].is_object(),
        "未注册方法应回 JSON-RPC error, got: {v2}"
    );

    drop(reader);
    drop(w_half);
    agent_handle.await.unwrap().unwrap();
}

#[tokio::test]
async fn session_new_before_initialize_is_rejected() {
    let (test_side, agent_handle) = spawn_agent();
    let (r_half, mut w_half) = tokio::io::split(test_side);
    let mut reader = BufReader::new(r_half);

    // 未 initialize 直接 session/new → SDK v1 守卫应拒绝
    send(
        &mut w_half,
        r#"{"jsonrpc":"2.0","id":1,"method":"session/new","params":{"cwd":"/tmp","mcpServers":[]}}"#,
    )
    .await;
    let v = recv(&mut reader).await;
    assert!(
        v["error"].is_object(),
        "initialize 前的 session/new 必须被拒绝, got: {v}"
    );

    drop(reader);
    drop(w_half);
    agent_handle.await.unwrap().unwrap();
}
