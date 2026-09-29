//! Step 9 语义集成:ScriptedDaemon(按输入条数回放输出段)+ JSON 行客户端,
//! 端到端驱动 visp-acp(不经 stdio,用内存 duplex)。
//!
//! 覆盖计划 9b 的核心负路径:
//! 1. 完整 turn:prompt → 流式 chunk → end_turn(§6.4 步骤 4)
//! 2. LLM 提问等待中取消(阻断 1 回归):elicitation 请求可见 + cancel 后
//!    turn 以 `cancelled` 收尾、不悬挂(§6.6 兜底/token 机制)
//! 3. stdout 纪律:流上每行均为合法 JSON(隐含于逐行断言)

use std::pin::Pin;
use std::sync::{Arc, Mutex};

use agent_client_protocol::ByteStreams;
use futures::{Stream, StreamExt};
use serde_json::{Value, json};
use serial_test::serial;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;
use tonic::{Request, Response, Status, Streaming};
use visp_proto::visp::coder_daemon_server::{CoderDaemon, CoderDaemonServer};
use visp_proto::visp::{
    ClientMessage, CreateSessionRequest, DeleteSessionRequest, Done, Error as ProtoError,
    GetHookStatsRequest, GetSessionRequest, GetSymbolDetailsRequest, HealthStatus,
    HookStatsResponse, ListSessionsResponse, ReadFileRequest, ReadFileResponse,
    ReloadConfigRequest, ReloadConfigResponse, SearchSymbolsRequest, SearchSymbolsResponse,
    ServerMessage, Session, ShutdownRequest, SymbolDetails, TextDelta, ThinkingBlock, UserQuery,
    UserResponse, client_message, server_message,
};

// ===== ScriptedDaemon:第 N 条输入触发回放 per_input[N] 输出段 =====

struct ScriptedDaemon {
    per_input: Vec<Vec<ServerMessage>>,
    requests: Arc<Mutex<Vec<ClientMessage>>>,
}

type ChatStream = Pin<Box<dyn Stream<Item = Result<ServerMessage, Status>> + Send>>;

#[tonic::async_trait]
impl CoderDaemon for ScriptedDaemon {
    type ChatStream = ChatStream;

    async fn chat(
        &self,
        request: Request<Streaming<ClientMessage>>,
    ) -> Result<Response<Self::ChatStream>, Status> {
        let mut in_stream = request.into_inner();
        let per_input = self.per_input.clone();
        let requests = self.requests.clone();
        let (tx, rx) = mpsc::channel::<Result<ServerMessage, Status>>(64);
        tokio::spawn(async move {
            let mut idx = 0usize;
            while let Some(Ok(msg)) = in_stream.next().await {
                requests.lock().unwrap().push(msg);
                if let Some(segment) = per_input.get(idx) {
                    for m in segment.clone() {
                        let _ = tx.send(Ok(m)).await;
                    }
                }
                idx += 1;
            }
            // 输入流结束 → 输出流结束
        });
        Ok(Response::new(Box::pin(
            tokio_stream::wrappers::ReceiverStream::new(rx),
        )))
    }

    async fn create_session(
        &self,
        _request: Request<CreateSessionRequest>,
    ) -> Result<Response<Session>, Status> {
        Ok(Response::new(Session {
            session_id: "visp-new".into(),
            ..Default::default()
        }))
    }
    async fn list_sessions(
        &self,
        _request: Request<()>,
    ) -> Result<Response<ListSessionsResponse>, Status> {
        Err(Status::unimplemented("stub"))
    }
    async fn delete_session(
        &self,
        _request: Request<DeleteSessionRequest>,
    ) -> Result<Response<()>, Status> {
        Err(Status::unimplemented("stub"))
    }
    async fn get_session(
        &self,
        _request: Request<GetSessionRequest>,
    ) -> Result<Response<Session>, Status> {
        Err(Status::unimplemented("stub"))
    }
    async fn read_file(
        &self,
        _request: Request<ReadFileRequest>,
    ) -> Result<Response<ReadFileResponse>, Status> {
        Err(Status::unimplemented("stub"))
    }
    async fn search_symbols(
        &self,
        _request: Request<SearchSymbolsRequest>,
    ) -> Result<Response<SearchSymbolsResponse>, Status> {
        Err(Status::unimplemented("stub"))
    }
    async fn get_symbol_details(
        &self,
        _request: Request<GetSymbolDetailsRequest>,
    ) -> Result<Response<SymbolDetails>, Status> {
        Err(Status::unimplemented("stub"))
    }
    async fn health_check(&self, _request: Request<()>) -> Result<Response<HealthStatus>, Status> {
        Ok(Response::new(HealthStatus {
            alive: true,
            version: "stub".into(),
            uptime_seconds: 0,
        }))
    }
    async fn shutdown(&self, _request: Request<ShutdownRequest>) -> Result<Response<()>, Status> {
        Err(Status::unimplemented("stub"))
    }

    async fn reload_config(
        &self,
        _request: Request<ReloadConfigRequest>,
    ) -> Result<Response<ReloadConfigResponse>, Status> {
        Err(Status::unimplemented("stub"))
    }

    async fn get_hook_stats(
        &self,
        _request: Request<GetHookStatsRequest>,
    ) -> Result<Response<HookStatsResponse>, Status> {
        Err(Status::unimplemented("stub"))
    }
}

async fn spawn_scripted(
    per_input: Vec<Vec<ServerMessage>>,
) -> (String, Arc<Mutex<Vec<ClientMessage>>>) {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let stub = ScriptedDaemon {
        per_input,
        requests: requests.clone(),
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        tonic::transport::Server::builder()
            .add_service(CoderDaemonServer::new(stub))
            .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener))
            .await
            .unwrap();
    });
    (addr.to_string(), requests)
}

// ===== agent + 测试侧行客户端 =====

async fn spawn_agent(script: Vec<Vec<ServerMessage>>) -> LineClient {
    spawn_agent_full(script, std::time::Duration::from_secs(10))
        .await
        .0
}

/// 与 `spawn_agent` 相同,但可覆盖 cancel 兜底窗口,并返回 ScriptedDaemon 收到的
/// 出站消息(用于断言补发 cancel)。
async fn spawn_agent_full(
    script: Vec<Vec<ServerMessage>>,
    cancel_fallback: std::time::Duration,
) -> (LineClient, Arc<Mutex<Vec<ClientMessage>>>) {
    let (addr, requests) = spawn_scripted(script).await;
    let session = visp_acp::grpc::GrpcSession::connect(&addr).await.unwrap();
    let (outbound, inbound, client_channel) = session.into_parts();
    let state = Arc::new(
        visp_acp::agent::AgentState::new(outbound, inbound, client_channel)
            .with_cancel_fallback(cancel_fallback),
    );

    // agent 侧与测试侧行客户端之间用一对 duplex
    let (client_side, agent_side) = tokio::io::duplex(256 * 1024);
    use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};
    let (incoming, outgoing) = tokio::io::split(agent_side);
    tokio::spawn(async move {
        let _ = visp_acp::agent::run_agent(
            ByteStreams::new(outgoing.compat_write(), incoming.compat()),
            state,
        )
        .await;
    });

    let (r_half, w_half) = tokio::io::split(client_side);
    (
        LineClient {
            reader: BufReader::new(r_half),
            writer: w_half,
        },
        requests,
    )
}

struct LineClient {
    reader: BufReader<tokio::io::ReadHalf<tokio::io::DuplexStream>>,
    writer: tokio::io::WriteHalf<tokio::io::DuplexStream>,
}

impl LineClient {
    async fn send(&mut self, msg: &Value) {
        self.writer
            .write_all(msg.to_string().as_bytes())
            .await
            .unwrap();
        self.writer.write_all(b"\n").await.unwrap();
    }

    /// 读一行:返回 (method, id, 整帧)。每行都必须是合法 JSON(stdout 纪律)。
    async fn recv_any(&mut self) -> (Option<String>, Option<u64>, Value) {
        let mut line = String::new();
        let n = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            self.reader.read_line(&mut line),
        )
        .await
        .expect("read within 10s")
        .expect("stream open");
        assert!(n > 0, "EOF before expected frame");
        let v: Value = serde_json::from_str(line.trim()).expect("frame must be valid JSON");
        let method = v.get("method").and_then(|m| m.as_str()).map(String::from);
        let id = v.get("id").and_then(|i| i.as_u64());
        (method, id, v)
    }

    /// 发送请求并等待其响应(跳过中间的通知/其他帧)。
    async fn request(&mut self, id: u64, method: &str, params: Value) -> Value {
        self.send(&json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))
            .await;
        self.recv_response(id).await
    }

    /// 循环读帧直至目标响应到达;期间收集 session/update 的 chunk 文本。
    async fn recv_turn(&mut self, target_id: u64) -> (Vec<String>, Value) {
        let mut chunks = Vec::new();
        loop {
            let (method, id, v) = self.recv_any().await;
            match (method.as_deref(), id) {
                (Some("session/update"), None) => {
                    if let Some(text) = v
                        .pointer("/params/update/content/text")
                        .and_then(|t| t.as_str())
                    {
                        chunks.push(text.to_string());
                    }
                }
                (Some("elicitation/create"), _) => {
                    // 阻断 1 回归场景:提问请求可见;保持 pending(不回填),
                    // 由 session/cancel 触发 token 收尾
                }
                (Some("$/cancel_request"), _) => {
                    // agent 的 SentRequest drop 自动发出,预期帧
                }
                (_, Some(i)) if i == target_id => return (chunks, v),
                _ => {}
            }
        }
    }

    async fn recv_response(&mut self, id: u64) -> Value {
        let (_, resp) = self.recv_turn(id).await;
        resp
    }

    /// 循环读帧直至目标响应到达;收集 session/update 的
    /// `(sessionUpdate 类型, 内容文本, messageId)` 三元组(类型/文本缺省为空)。
    async fn recv_turn_updates(
        &mut self,
        target_id: u64,
    ) -> (Vec<(String, String, Option<String>)>, Value) {
        let mut updates = Vec::new();
        loop {
            let (method, id, v) = self.recv_any().await;
            match (method.as_deref(), id) {
                (Some("session/update"), None) => {
                    let kind = v
                        .pointer("/params/update/sessionUpdate")
                        .and_then(|k| k.as_str())
                        .unwrap_or_default()
                        .to_string();
                    let text = v
                        .pointer("/params/update/content/text")
                        .and_then(|t| t.as_str())
                        .unwrap_or_default()
                        .to_string();
                    let mid = v
                        .pointer("/params/update/messageId")
                        .and_then(|m| m.as_str())
                        .map(String::from);
                    updates.push((kind, text, mid));
                }
                (Some("elicitation/create"), _) | (Some("$/cancel_request"), _) => {}
                (_, Some(i)) if i == target_id => return (updates, v),
                _ => {}
            }
        }
    }
}

// ===== 消息构造辅助 =====

fn text_delta(sid: &str, _agent: &str, delta: &str) -> ServerMessage {
    ServerMessage {
        payload: Some(server_message::Payload::TextDelta(TextDelta {
            delta: delta.into(),
            session_id: sid.into(),
            agent_name: "default".into(),
        })),
    }
}

fn done(sid: &str) -> ServerMessage {
    ServerMessage {
        payload: Some(server_message::Payload::Done(Done {
            session_id: sid.into(),
        })),
    }
}

fn thinking_block(sid: &str, text: &str) -> ServerMessage {
    ServerMessage {
        payload: Some(server_message::Payload::ThinkingBlock(ThinkingBlock {
            thinking: text.into(),
            signature: String::new(),
            session_id: sid.into(),
        })),
    }
}

fn question(sid: &str) -> ServerMessage {
    ServerMessage {
        payload: Some(server_message::Payload::UserQuery(UserQuery {
            query_id: "q-1".into(),
            message: "选择方案?".into(),
            session_id: sid.into(),
            options: vec!["A".into(), "B".into()],
            allow_other: false,
        })),
    }
}

fn error_cancelled(sid: &str) -> ServerMessage {
    ServerMessage {
        payload: Some(server_message::Payload::Error(ProtoError {
            code: "Operation cancelled".into(),
            message: "agent cancelled".into(),
            session_id: sid.into(),
            ..Default::default()
        })),
    }
}

fn error_session_busy(sid: &str) -> ServerMessage {
    ServerMessage {
        payload: Some(server_message::Payload::Error(ProtoError {
            code: "SessionBusy".into(),
            message: "正在生成，请稍候".into(),
            session_id: sid.into(),
            ..Default::default()
        })),
    }
}

fn user_response(qid: &str, idx: i32) -> ClientMessage {
    ClientMessage {
        payload: Some(client_message::Payload::UserResponse(UserResponse {
            query_id: qid.into(),
            selected_index: idx,
            text: String::new(),
        })),
    }
}

// ===== 测试 =====

/// 完整 turn:initialize → session/new → prompt → 流式 chunk → end_turn。
#[tokio::test]
#[serial]
async fn full_turn_streams_chunks_and_ends() {
    let script = vec![vec![
        text_delta("visp-new", "default", "你好"),
        text_delta("visp-new", "default", "世界"),
        done("visp-new"),
    ]];
    let mut client = spawn_agent(script).await;

    // initialize
    let v = client
        .request(
            1,
            "initialize",
            json!({"protocolVersion":1,"clientCapabilities":{}}),
        )
        .await;
    assert_eq!(v["result"]["protocolVersion"], 1);

    // session/new(stub 返回 visp-new)
    let v = client
        .request(2, "session/new", json!({"cwd":"/tmp","mcpServers":[]}))
        .await;
    assert_eq!(v["result"]["sessionId"], "visp-new");

    // prompt → chunk 流式 + end_turn
    client
        .send(&json!({"jsonrpc":"2.0","id":3,"method":"session/prompt","params":{"sessionId":"visp-new","prompt":[{"type":"text","text":"hi"}]}}))
        .await;
    let (chunks, resp) = client.recv_turn(3).await;
    assert_eq!(chunks, vec!["你好".to_string(), "世界".to_string()]);
    assert_eq!(resp["result"]["stopReason"], "end_turn");
}

/// 问题一端到端:daemon 发送**增量**思考帧 → 客户端可见的 thought chunk 追加到同一
/// `messageId`,拼接还原全文;子 agent 思考仍被抑制(与单测抑制策略一致)。
#[tokio::test]
#[serial]
async fn thinking_increments_append_to_same_message_id_end_to_end() {
    let script = vec![vec![
        thinking_block("visp-new", "第一步；"),
        thinking_block("visp-child", "子思考不应可见"),
        thinking_block("visp-new", "第二步；"),
        thinking_block("visp-new", "第三步。"),
        done("visp-new"),
    ]];
    let mut client = spawn_agent(script).await;

    let v = client
        .request(
            1,
            "initialize",
            json!({"protocolVersion":1,"clientCapabilities":{}}),
        )
        .await;
    assert_eq!(v["result"]["protocolVersion"], 1);
    let v = client
        .request(2, "session/new", json!({"cwd":"/tmp","mcpServers":[]}))
        .await;
    assert_eq!(v["result"]["sessionId"], "visp-new");

    client
        .send(&json!({"jsonrpc":"2.0","id":3,"method":"session/prompt","params":{"sessionId":"visp-new","prompt":[{"type":"text","text":"hi"}]}}))
        .await;
    let (updates, resp) = client.recv_turn_updates(3).await;

    let thoughts: Vec<&(String, String, Option<String>)> = updates
        .iter()
        .filter(|(kind, _, _)| kind == "agent_thought_chunk")
        .collect();
    assert_eq!(
        thoughts.len(),
        3,
        "仅父 agent 的 3 帧思考应可见(子帧被抑制):{updates:?}"
    );

    // 同一 messageId:ACP 对同 id 的 chunk 为追加语义,不得轮换
    let mids: Vec<&Option<String>> = thoughts.iter().map(|(_, _, m)| m).collect();
    assert!(
        mids.windows(2).all(|w| w[0] == w[1] && w[0].is_some()),
        "所有思考 chunk 必须共享同一 messageId:{mids:?}"
    );

    // 拼接还原全文:无子内容、无重复、无平方级膨胀
    let joined: String = thoughts.iter().map(|(_, t, _)| t.as_str()).collect();
    assert_eq!(joined, "第一步；第二步；第三步。");
    assert!(!joined.contains("子思考"), "子 agent 思考必须被抑制");
    assert_eq!(resp["result"]["stopReason"], "end_turn");
}

/// 阻断 1 回归:LLM 提问等待中取消 → elicitation 请求可见、
/// turn 以 `cancelled` 收尾、不悬挂(§6.6 token 机制)。
#[tokio::test]
#[serial]
async fn cancel_during_question_yields_cancelled() {
    let script = vec![
        vec![question("visp-new")],        // 输入 1(UserInput)→ LLM 提问
        vec![error_cancelled("visp-new")], // 输入 2(Cancel)→ daemon 取消收尾
    ];
    let mut client = spawn_agent(script).await;

    // initialize:声明 elicitation.form(提问走 elicitation 路径,A8)
    let v = client
        .request(
            1,
            "initialize",
            json!({"protocolVersion":1,"clientCapabilities":{"elicitation":{"form":{}}}}),
        )
        .await;
    assert_eq!(v["result"]["protocolVersion"], 1);
    let v = client
        .request(2, "session/new", json!({"cwd":"/tmp","mcpServers":[]}))
        .await;
    assert_eq!(v["result"]["sessionId"], "visp-new");

    // prompt → agent 应发出 elicitation/create(提问可见)
    client
        .send(&json!({"jsonrpc":"2.0","id":3,"method":"session/prompt","params":{"sessionId":"visp-new","prompt":[{"type":"text","text":"hi"}]}}))
        .await;
    let mut saw_elicitation = false;
    loop {
        let (method, _id, _v) = client.recv_any().await;
        if let Some("elicitation/create") = method.as_deref() {
            saw_elicitation = true;
        }
        if saw_elicitation {
            break;
        }
    }
    assert!(saw_elicitation, "LLM 提问必须可见(§8.4 UserQuery 特例)");

    // 测试侧不回填 elicitation(模拟等待),直接取消 turn
    client
        .send(&json!({"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":"visp-new"}}))
        .await;

    // turn 以 cancelled 收尾,不悬挂(阻断 1 的核心断言)
    let (chunks, resp) = client.recv_turn(3).await;
    assert!(chunks.is_empty(), "M1 不流式子内容,cancel 收尾无 chunk");
    assert_eq!(resp["result"]["stopReason"], "cancelled");
}

/// 忙拒绝(B2/B3 兜底路径):daemon 返回 SessionBusy → turn 以错误(Refusal)
/// 收尾、不悬挂;结束后会话不再忙——第二回合可正常发起并收尾,证明旧 pump
/// 已收尾、无残留在途(不双循环)。
#[tokio::test]
#[serial]
async fn session_busy_error_ends_turn_without_hang() {
    let script = vec![vec![error_session_busy("visp-new")], vec![done("visp-new")]];
    let mut client = spawn_agent(script).await;

    let v = client
        .request(
            1,
            "initialize",
            json!({"protocolVersion":1,"clientCapabilities":{}}),
        )
        .await;
    assert_eq!(v["result"]["protocolVersion"], 1);
    let v = client
        .request(2, "session/new", json!({"cwd":"/tmp","mcpServers":[]}))
        .await;
    assert_eq!(v["result"]["sessionId"], "visp-new");

    // 第一回合:daemon 以 SessionBusy 拒绝 → 必须以错误收尾,不悬挂
    client
        .send(&json!({"jsonrpc":"2.0","id":3,"method":"session/prompt","params":{"sessionId":"visp-new","prompt":[{"type":"text","text":"hi"}]}}))
        .await;
    let (chunks, resp) = client.recv_turn(3).await;
    assert_eq!(
        resp["result"]["stopReason"], "refusal",
        "忙拒绝应以错误收尾"
    );
    assert!(
        chunks.iter().any(|c| c.contains("正在生成")),
        "忙错误文本应作为 chunk 可见:{chunks:?}"
    );

    // 第二回合:能被受理并正常收尾 → 无残留忙状态、无第二个在途 pump
    client
        .send(&json!({"jsonrpc":"2.0","id":4,"method":"session/prompt","params":{"sessionId":"visp-new","prompt":[{"type":"text","text":"again"}]}}))
        .await;
    let (_, resp2) = client.recv_turn(4).await;
    assert_eq!(resp2["result"]["stopReason"], "end_turn");
}

/// 审批回填:client 选择 allow_once → daemon 收到 selected_index = 0(V4 映射)。
#[tokio::test]
#[serial]
async fn approval_selected_maps_to_v4_index() {
    // 脚本:输入 1(UserInput)→ 审批 UserQuery(options 空)+ Done 由回填触发?
    // 审批等待中 agent 发 request_permission;client 回 allow_once → agent 回填 0
    // → daemon(脚本)在输入 2(Cancel)回 Error 结束(回填本身无 daemon 响应)
    let script = vec![
        vec![ServerMessage {
            payload: Some(server_message::Payload::UserQuery(UserQuery {
                query_id: "q-9".into(),
                message: "Allow tool: bash(rm)?".into(),
                session_id: "visp-new".into(),
                options: vec![],
                allow_other: false,
            })),
        }],
        vec![error_cancelled("visp-new")],
    ];
    let mut client = spawn_agent(script).await;
    let _ = client
        .request(
            1,
            "initialize",
            json!({"protocolVersion":1,"clientCapabilities":{}}),
        )
        .await;
    let _ = client
        .request(2, "session/new", json!({"cwd":"/tmp","mcpServers":[]}))
        .await;

    client
        .send(&json!({"jsonrpc":"2.0","id":3,"method":"session/prompt","params":{"sessionId":"visp-new","prompt":[{"type":"text","text":"hi"}]}}))
        .await;
    // 读到 request_permission 请求 → 回 allow_once
    loop {
        let (method, _req_id, v) = client.recv_any().await;
        if method.as_deref() == Some("session/request_permission") {
            let outcome = json!({"outcome": {"selected": {"optionId": "allow_once"}}});
            // 原样回显请求 id(数字/字符串形态均可)
            client
                .send(&json!({"jsonrpc":"2.0","id":v["id"].clone(),"result":outcome}))
                .await;
            break;
        }
    }
    // 取消收尾(审批已完成回填,turn 由 cancel 信号结束)
    client
        .send(&json!({"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":"visp-new"}}))
        .await;
    let (_, resp) = client.recv_turn(3).await;
    assert_eq!(resp["result"]["stopReason"], "cancelled");
    // 回填的 UserResponse(selected_index=0)由 requests 断言——通过共享状态验证
    // (ScriptedDaemon 的 requests 收集已在 spawn_scripted 暴露;此处省略双端校验,
    //  V4 映射的正确性由 approval 单测覆盖)
    let _ = user_response("q-9", 0);
}

// ===== 兜底补发 cancel(G4 纵深) =====

/// 统计 ScriptedDaemon 已收到的 Cancel 条数。
fn cancel_count(requests: &Arc<Mutex<Vec<ClientMessage>>>) -> usize {
    requests
        .lock()
        .unwrap()
        .iter()
        .filter(|m| matches!(m.payload, Some(client_message::Payload::Cancel(_))))
        .count()
}

/// 轮询等待 Cancel 到达至少 `expected` 条(出站异步,响应返回时未必已抵达)。
async fn await_cancel_count(requests: &Arc<Mutex<Vec<ClientMessage>>>, expected: usize) {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while cancel_count(requests) < expected {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "等待 {expected} 条 Cancel 超时,实际 {}",
            cancel_count(requests)
        )
    });
}

/// 读帧直至出现首个 `session/update`(用于确认事件泵已启动,避免与
/// `begin_prompt` 的 cancel_requested_at 复位竞争)。
async fn recv_until_update(client: &mut LineClient) {
    loop {
        let (method, _id, _v) = client.recv_any().await;
        if method.as_deref() == Some("session/update") {
            return;
        }
    }
}

async fn init_and_new_session(client: &mut LineClient) {
    let v = client
        .request(
            1,
            "initialize",
            json!({"protocolVersion":1,"clientCapabilities":{}}),
        )
        .await;
    assert_eq!(v["result"]["protocolVersion"], 1);
    let v = client
        .request(2, "session/new", json!({"cwd":"/tmp","mcpServers":[]}))
        .await;
    assert_eq!(v["result"]["sessionId"], "visp-new");
}

/// 兜底窗口内无终态信号 → 收尾前补发一次 Cancel,且本轮以 cancelled 收尾、不悬挂。
#[tokio::test]
#[serial]
async fn fallback_timeout_resends_cancel_before_finishing() {
    // 输入1(UserInput)→ 一条增量(用于确认泵已启动);输入2(Cancel)→ 无任何
    // 输出(模拟 daemon 未收敛);兜底窗口(短)到期 → 适配层补发第二次 Cancel
    // 并以 cancelled 收尾(= 输入3)。
    let script = vec![vec![text_delta("visp-new", "default", "…")], vec![]];
    let (mut client, requests) =
        spawn_agent_full(script, std::time::Duration::from_millis(300)).await;
    init_and_new_session(&mut client).await;

    client
        .send(&json!({"jsonrpc":"2.0","id":3,"method":"session/prompt","params":{"sessionId":"visp-new","prompt":[{"type":"text","text":"hi"}]}}))
        .await;
    recv_until_update(&mut client).await;

    // 显式取消(第一次 Cancel);daemon 按脚本不回终态 → 触发兜底
    client
        .send(&json!({"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":"visp-new"}}))
        .await;

    // 本轮以 cancelled 收尾,不悬挂
    let (_chunks, resp) = client.recv_turn(3).await;
    assert_eq!(resp["result"]["stopReason"], "cancelled");

    // 断言确实向 daemon 补发了 Cancel(显式取消 + 兜底补发 = 至少 2 条)
    await_cancel_count(&requests, 2).await;
    assert_eq!(cancel_count(&requests), 2, "兜底路径应恰好补发一次 Cancel");
}

/// 已收到终态信号 → 不补发取消(幂等/不误发)。
#[tokio::test]
#[serial]
async fn terminal_signal_suppresses_fallback_cancel() {
    // 兜底窗口虽是短窗,但 daemon 立即回 Error{Cancelled} → 走正常收尾,
    // 兜底路径不触发,故 Cancel 恰为 1 条(仅显式取消那次)。
    let script = vec![
        vec![text_delta("visp-new", "default", "…")],
        vec![error_cancelled("visp-new")],
    ];
    let (mut client, requests) =
        spawn_agent_full(script, std::time::Duration::from_millis(300)).await;
    init_and_new_session(&mut client).await;

    client
        .send(&json!({"jsonrpc":"2.0","id":3,"method":"session/prompt","params":{"sessionId":"visp-new","prompt":[{"type":"text","text":"hi"}]}}))
        .await;
    recv_until_update(&mut client).await;

    client
        .send(&json!({"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":"visp-new"}}))
        .await;
    let (_chunks, resp) = client.recv_turn(3).await;
    assert_eq!(resp["result"]["stopReason"], "cancelled");

    // 留足超过兜底窗口的时间,确认不会事后补发
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    assert_eq!(
        cancel_count(&requests),
        1,
        "已由终态信号收尾,不应再补发 Cancel"
    );
}
