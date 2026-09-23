//! gRPC 会话传输层:唯一 Chat 流(4a)、出站封装(4b)、入站接收与断连上抛。
//!
//! V1 约束:daemon 的 `chat()` 为进程内单例(`take()`),本模块在客户端侧
//! 同样保证一条流——进程内重复建流返回错误;`GrpcSession` Drop 后允许重连
//! (daemon 重启场景)。

use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{anyhow, bail};
use futures::StreamExt;
use tokio::sync::mpsc;
use visp_proto::visp::client_message;
use visp_proto::visp::coder_daemon_client::CoderDaemonClient;
use visp_proto::visp::{Ack, Cancel, ClientMessage, ServerMessage, UserInput, UserResponse};

/// 进程内 Chat 流占用标记(与 daemon 的 V1 单流约束对齐)。
static CHAT_STREAM_TAKEN: AtomicBool = AtomicBool::new(false);

/// 出站消息发送端(可 Clone,生命周期与所属 `GrpcSession` 一致)。
#[derive(Clone)]
pub struct Outbound {
    tx: mpsc::Sender<ClientMessage>,
}

impl Outbound {
    /// 发送用户输入(`request_id` 由调用方生成,用于日志追踪)。
    pub async fn send_user_input(
        &self,
        session_id: &str,
        text: &str,
        request_id: &str,
    ) -> anyhow::Result<()> {
        self.send(ClientMessage {
            payload: Some(client_message::Payload::UserInput(UserInput {
                text: text.into(),
                session_id: session_id.into(),
                request_id: request_id.into(),
            })),
        })
        .await
    }

    /// 发送取消(会话级通知)。
    pub async fn send_cancel(&self, session_id: &str) -> anyhow::Result<()> {
        self.send(ClientMessage {
            payload: Some(client_message::Payload::Cancel(Cancel {
                session_id: session_id.into(),
            })),
        })
        .await
    }

    /// 回填审批/提问结果(`selected_index` 语义见设计 V4;-1 + text 为自定义输入)。
    pub async fn send_user_response(
        &self,
        query_id: &str,
        selected_index: i32,
        text: &str,
    ) -> anyhow::Result<()> {
        self.send(ClientMessage {
            payload: Some(client_message::Payload::UserResponse(UserResponse {
                query_id: query_id.into(),
                selected_index,
                text: text.into(),
            })),
        })
        .await
    }

    /// 发送 Ack(仅日志追踪,可选)。
    pub async fn send_ack(&self, request_id: &str) -> anyhow::Result<()> {
        self.send(ClientMessage {
            payload: Some(client_message::Payload::Ack(Ack {
                request_id: request_id.into(),
            })),
        })
        .await
    }

    async fn send(&self, msg: ClientMessage) -> anyhow::Result<()> {
        self.tx
            .send(msg)
            .await
            .map_err(|_| anyhow!("chat stream is closed"))
    }
}

/// 一条 Chat 流的会话句柄:出站 sender + 入站接收端。
pub struct GrpcSession {
    pub outbound: Outbound,
    inbound: mpsc::Receiver<ServerMessage>,
}

impl GrpcSession {
    /// 建立唯一 Chat 流。同进程重复建流返回错误(V1)。
    pub async fn connect(addr: &str) -> anyhow::Result<Self> {
        if CHAT_STREAM_TAKEN.swap(true, Ordering::SeqCst) {
            bail!("chat stream already established (V1: one stream per process)");
        }
        match Self::connect_inner(addr).await {
            Ok(session) => Ok(session),
            Err(e) => {
                CHAT_STREAM_TAKEN.store(false, Ordering::SeqCst);
                Err(e)
            }
        }
    }

    async fn connect_inner(addr: &str) -> anyhow::Result<Self> {
        let mut client = CoderDaemonClient::connect(format!("http://{addr}"))
            .await
            .map_err(|e| anyhow!("connect daemon: {e}"))?;

        let (tx, rx) = mpsc::channel::<ClientMessage>(64);
        let request_stream = tokio_stream::wrappers::ReceiverStream::new(rx);
        let response = client
            .chat(request_stream)
            .await
            .map_err(|e| anyhow!("establish chat stream: {e}"))?;

        // 入站转发:ServerMessage → inbound channel;流结束/出错 → 通道关闭
        let (in_tx, inbound) = mpsc::channel::<ServerMessage>(256);
        let mut response_stream = response.into_inner();
        tokio::spawn(async move {
            while let Some(msg) = response_stream.next().await {
                match msg {
                    Ok(m) => {
                        if in_tx.send(m).await.is_err() {
                            break;
                        }
                    }
                    Err(status) => {
                        tracing::warn!(%status, "chat stream terminated with status");
                        break;
                    }
                }
            }
            // drop(in_tx) → recv() 返回 None(流断开上抛)
        });

        Ok(Self {
            outbound: Outbound { tx },
            inbound,
        })
    }

    /// 收取下一条入站消息;`None` 表示流已断开。
    pub async fn recv(&mut self) -> Option<ServerMessage> {
        self.inbound.recv().await
    }
}

impl std::fmt::Debug for GrpcSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GrpcSession").finish_non_exhaustive()
    }
}

impl Drop for GrpcSession {
    fn drop(&mut self) {
        CHAT_STREAM_TAKEN.store(false, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tonic::{Request, Response, Status, Streaming};

    // ===== 4b:出站编码(不经 wire,直接断言 ClientMessage 字段) =====

    #[tokio::test]
    async fn outbound_encodes_user_input() {
        let (tx, mut rx) = mpsc::channel::<ClientMessage>(4);
        let ob = Outbound { tx };
        ob.send_user_input("sess-1", "你好", "r-42").await.unwrap();
        let msg = rx.recv().await.unwrap();
        match msg.payload {
            Some(client_message::Payload::UserInput(u)) => {
                assert_eq!(u.text, "你好");
                assert_eq!(u.session_id, "sess-1");
                assert_eq!(u.request_id, "r-42");
            }
            other => panic!("expected UserInput, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn outbound_encodes_cancel_user_response_ack() {
        let (tx, mut rx) = mpsc::channel::<ClientMessage>(8);
        let ob = Outbound { tx };

        ob.send_cancel("sess-1").await.unwrap();
        ob.send_user_response("q-1", 2, "").await.unwrap();
        ob.send_user_response("q-2", -1, "自定义输入")
            .await
            .unwrap();
        ob.send_ack("r-42").await.unwrap();

        let cancel = rx.recv().await.unwrap();
        assert!(matches!(
            cancel.payload,
            Some(client_message::Payload::Cancel(ref c)) if c.session_id == "sess-1"
        ));
        let resp = rx.recv().await.unwrap();
        assert!(matches!(
            resp.payload,
            Some(client_message::Payload::UserResponse(ref r))
                if r.query_id == "q-1" && r.selected_index == 2
        ));
        let resp2 = rx.recv().await.unwrap();
        assert!(matches!(
            resp2.payload,
            Some(client_message::Payload::UserResponse(ref r))
                if r.selected_index == -1 && r.text == "自定义输入"
        ));
        let ack = rx.recv().await.unwrap();
        assert!(matches!(
            ack.payload,
            Some(client_message::Payload::Ack(ref a)) if a.request_id == "r-42"
        ));
    }

    #[tokio::test]
    async fn outbound_send_fails_after_stream_closed() {
        let (tx, rx) = mpsc::channel::<ClientMessage>(4);
        drop(rx);
        let ob = Outbound { tx };
        let err = ob.send_user_input("s", "hi", "r1").await.unwrap_err();
        assert!(err.to_string().contains("closed"));
    }

    // ===== 4a:建流单例 + 入站转发(in-process stub server) =====

    use visp_proto::visp::coder_daemon_server::{CoderDaemon, CoderDaemonServer};
    use visp_proto::visp::{
        CreateSessionRequest, DeleteSessionRequest, GetSessionRequest, GetSymbolDetailsRequest,
        HealthStatus, ListSessionsResponse, ReadFileRequest, ReadFileResponse,
        SearchSymbolsRequest, SearchSymbolsResponse, Session, ShutdownRequest, SymbolDetails,
    };

    struct StubDaemon {
        responses: Vec<ServerMessage>,
        requests: Arc<Mutex<Vec<ClientMessage>>>,
    }

    type ChatStream = Pin<Box<dyn futures::Stream<Item = Result<ServerMessage, Status>> + Send>>;

    #[tonic::async_trait]
    impl CoderDaemon for StubDaemon {
        type ChatStream = ChatStream;

        async fn chat(
            &self,
            request: Request<Streaming<ClientMessage>>,
        ) -> Result<Response<Self::ChatStream>, Status> {
            let mut in_stream = request.into_inner();
            let requests = self.requests.clone();
            tokio::spawn(async move {
                while let Some(Ok(msg)) = in_stream.next().await {
                    requests.lock().unwrap().push(msg);
                }
            });
            let responses = self.responses.clone();
            let stream: Self::ChatStream =
                Box::pin(futures::stream::iter(responses.into_iter().map(Ok)));
            Ok(Response::new(stream))
        }

        async fn create_session(
            &self,
            _request: Request<CreateSessionRequest>,
        ) -> Result<Response<Session>, Status> {
            Err(Status::unimplemented("stub"))
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
        async fn health_check(
            &self,
            _request: Request<()>,
        ) -> Result<Response<HealthStatus>, Status> {
            Ok(Response::new(HealthStatus {
                alive: true,
                version: "stub".into(),
                uptime_seconds: 0,
            }))
        }
        async fn shutdown(
            &self,
            _request: Request<ShutdownRequest>,
        ) -> Result<Response<()>, Status> {
            Err(Status::unimplemented("stub"))
        }
    }

    /// 起一个 in-process stub daemon,返回 (addr, requests 收集句柄)。
    async fn spawn_stub(responses: Vec<ServerMessage>) -> (String, Arc<Mutex<Vec<ClientMessage>>>) {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stub = StubDaemon {
            responses,
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

    use std::pin::Pin;

    fn sample_message() -> ServerMessage {
        ServerMessage {
            payload: Some(visp_proto::visp::server_message::Payload::Done(
                visp_proto::visp::Done {
                    session_id: "sess-1".into(),
                },
            )),
        }
    }

    fn status_message() -> ServerMessage {
        ServerMessage {
            payload: Some(visp_proto::visp::server_message::Payload::StatusUpdate(
                visp_proto::visp::StatusUpdate {
                    message: "ready".into(),
                    session_id: "sess-1".into(),
                    user_inputs: vec![],
                    agent_name: String::new(),
                    view_only: false,
                },
            )),
        }
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn stream_roundtrip_forwards_inbound_and_outbound() {
        let responses = vec![sample_message(), status_message()];
        let (addr, requests) = spawn_stub(responses).await;

        let mut session = GrpcSession::connect(&addr).await.unwrap();
        session
            .outbound
            .send_user_input("sess-1", "hi", "r-1")
            .await
            .unwrap();

        // 出站到达 stub
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                {
                    let got = requests.lock().unwrap();
                    if got.iter().any(|m| {
                        matches!(
                            m.payload,
                            Some(client_message::Payload::UserInput(ref u)) if u.text == "hi"
                        )
                    }) {
                        break;
                    }
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();

        // 入站按序转发
        let first = session.recv().await.unwrap();
        assert!(matches!(
            first.payload,
            Some(visp_proto::visp::server_message::Payload::Done(_))
        ));
        let second = session.recv().await.unwrap();
        assert!(matches!(
            second.payload,
            Some(visp_proto::visp::server_message::Payload::StatusUpdate(_))
        ));
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn second_connect_is_rejected_and_drop_releases() {
        let (addr, _requests) = spawn_stub(vec![]).await;
        let _session = GrpcSession::connect(&addr).await.unwrap();

        let second = GrpcSession::connect(&addr).await;
        let err = second.unwrap_err();
        assert!(
            err.to_string().contains("already established"),
            "第二次建流应被拒(V1), got: {err}"
        );

        drop(_session);
        // Drop 释放后可重新建流(daemon 重启重连场景)
        let again = GrpcSession::connect(&addr).await;
        assert!(again.is_ok());
    }
}
