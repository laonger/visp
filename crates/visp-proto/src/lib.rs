//! visp-proto —— visp 系统的 gRPC 协议定义层。
//!
//! 本 crate 通过 tonic-build 在编译时将 `proto/visp.proto` 自动生成 Rust 代码，
//! 定义了 CLI 与 Daemon 之间的 gRPC 服务 [`CoderDaemon`] 及双向流 Chat 协议。
//!
//! 生成的代码包含：
//! - 请求/响应消息类型（Session、StatusUpdate 等）
//! - gRPC server/client trait（`coder_daemon_server::CoderDaemon` /
//!   `coder_daemon_client::CoderDaemonClient`）

pub mod visp {
    tonic::include_proto!("visp");
}

#[cfg(test)]
mod tests {
    use super::*;
    use prost::Message;

    #[test]
    fn status_update_default_view_only_is_false() {
        let status = visp::StatusUpdate::default();
        assert!(
            !status.view_only,
            "view_only should default to false in proto3"
        );
    }

    #[test]
    fn status_update_with_view_only_true() {
        let status = visp::StatusUpdate {
            view_only: true,
            ..Default::default()
        };
        let encoded = status.encode_to_vec();
        let decoded = visp::StatusUpdate::decode(encoded.as_slice()).unwrap();
        assert!(
            decoded.view_only,
            "view_only should be true after round-trip"
        );
    }
}
