#![allow(dead_code)]

use std::pin::Pin;

use futures::StreamExt;
use tokio::sync::mpsc;
use tonic::transport::Channel;

use vbw_proto::vibewisp::{
    Ack, Cancel, ClientMessage, ConfigUpdate, CreateSessionRequest, LlmConfig, ServerMessage,
    Session, UserInput, UserResponse, client_message, coder_daemon_client::CoderDaemonClient,
};

pub struct VbwClient {
    client: CoderDaemonClient<Channel>,
}

pub struct ChatHandle {
    pub(crate) request_tx: mpsc::Sender<ClientMessage>,
    pub response_stream:
        Pin<Box<dyn futures::Stream<Item = Result<ServerMessage, tonic::Status>> + Send>>,
    pub(crate) session_id: String,
    next_request_id: u64,
}

impl VbwClient {
    pub async fn connect(addr: &str) -> Result<Self, String> {
        let client = CoderDaemonClient::connect(format!("http://{}", addr))
            .await
            .map_err(|e| format!("failed to connect: {}", e))?;
        Ok(Self { client })
    }

    pub async fn health_check(&mut self) -> Result<bool, String> {
        let resp = self
            .client
            .health_check(())
            .await
            .map_err(|e| format!("health check: {}", e))?;
        Ok(resp.into_inner().alive)
    }

    pub async fn create_session(
        &mut self,
        project_path: &str,
        config: Option<LlmConfig>,
    ) -> Result<Session, String> {
        let req = CreateSessionRequest {
            project_path: project_path.to_string(),
            config,
        };
        let resp = self
            .client
            .create_session(req)
            .await
            .map_err(|e| format!("create session: {}", e))?;
        Ok(resp.into_inner())
    }

    pub async fn chat(&mut self, session_id: &str) -> Result<ChatHandle, String> {
        let (tx, rx) = mpsc::channel::<ClientMessage>(16);
        let request_stream = tokio_stream::wrappers::ReceiverStream::new(rx);
        let response = self
            .client
            .chat(request_stream)
            .await
            .map_err(|e| format!("chat: {}", e))?;
        let response_stream = response.into_inner();
        Ok(ChatHandle {
            request_tx: tx,
            response_stream: Box::pin(response_stream),
            session_id: session_id.to_string(),
            next_request_id: 1,
        })
    }
}

impl ChatHandle {
    pub fn send_input(&mut self, text: &str) -> &'static str {
        let rid = self.next_request_id.to_string();
        self.next_request_id += 1;
        let msg = ClientMessage {
            payload: Some(client_message::Payload::UserInput(UserInput {
                text: text.to_string(),
                session_id: self.session_id.clone(),
                request_id: rid.clone(),
            })),
        };
        let tx = self.request_tx.clone();
        tokio::spawn(async move {
            let _ = tx.send(msg).await;
        });
        // Return a leaked string — the caller uses it immediately for the ack cycle
        Box::leak(rid.into_boxed_str())
    }

    pub fn send_ack(&self, request_id: &str) {
        let msg = ClientMessage {
            payload: Some(client_message::Payload::Ack(Ack {
                request_id: request_id.to_owned(),
            })),
        };
        let tx = self.request_tx.clone();
        tokio::spawn(async move {
            let _ = tx.send(msg).await;
        });
    }

    pub fn send_response(&self, query_id: &str, selected_index: i32, text: &str) {
        let msg = ClientMessage {
            payload: Some(client_message::Payload::UserResponse(UserResponse {
                query_id: query_id.to_string(),
                selected_index,
                text: text.to_string(),
            })),
        };
        let tx = self.request_tx.clone();
        tokio::spawn(async move {
            let _ = tx.send(msg).await;
        });
    }

    pub fn send_cancel(&self) {
        let msg = ClientMessage {
            payload: Some(client_message::Payload::Cancel(Cancel {
                session_id: self.session_id.clone(),
            })),
        };
        let tx = self.request_tx.clone();
        tokio::spawn(async move {
            let _ = tx.send(msg).await;
        });
    }

    pub fn send_config_update(&self, config: LlmConfig) {
        let msg = ClientMessage {
            payload: Some(client_message::Payload::ConfigUpdate(ConfigUpdate {
                session_id: self.session_id.clone(),
                config: Some(config),
            })),
        };
        let tx = self.request_tx.clone();
        tokio::spawn(async move {
            let _ = tx.send(msg).await;
        });
    }

    pub async fn recv(&mut self) -> Option<ServerMessage> {
        match self.response_stream.next().await {
            Some(Ok(msg)) => Some(msg),
            Some(Err(_)) => None,
            None => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_client_connect_invalid_addr() {
        let result = VbwClient::connect("invalid:0").await;
        assert!(result.is_err());
    }
}
