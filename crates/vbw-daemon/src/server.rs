use tonic::transport::Server;
use crate::service::CoderDaemonService;

pub async fn start_server(
    addr: &str,
    service: CoderDaemonService,
) -> Result<(), Box<dyn std::error::Error>> {
    let addr = addr.parse()?;
    Server::builder()
        .add_service(vbw_proto::vibewisp::coder_daemon_server::CoderDaemonServer::new(service))
        .serve(addr)
        .await?;
    Ok(())
}
