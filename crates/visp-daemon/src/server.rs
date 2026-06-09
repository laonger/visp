use crate::service::CoderDaemonService;
use tonic::transport::Server;

pub async fn start_server(
    addr: &str,
    service: CoderDaemonService,
) -> Result<(), Box<dyn std::error::Error>> {
    let addr = addr.parse()?;
    Server::builder()
        .add_service(visp_proto::visp::coder_daemon_server::CoderDaemonServer::new(service))
        .serve(addr)
        .await?;
    Ok(())
}
