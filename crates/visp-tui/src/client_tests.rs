use super::*;

#[tokio::test]
async fn test_client_connect_invalid_addr() {
    let result = VispClient::connect("invalid:0").await;
    assert!(result.is_err());
}
