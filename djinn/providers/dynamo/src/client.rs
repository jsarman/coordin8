use aws_sdk_dynamodb::Client;

/// Build a DynamoDB client. When `DYNAMODB_ENDPOINT` is set (e.g. for MiniStack),
/// that URL is used as the endpoint override. Otherwise falls through to the
/// standard AWS credential chain and region config.
pub async fn make_dynamo_client() -> Client {
    let mut config_loader = aws_config::defaults(aws_config::BehaviorVersion::latest());

    if let Ok(endpoint) = std::env::var("DYNAMODB_ENDPOINT") {
        config_loader = config_loader.endpoint_url(endpoint);
    }

    let config = config_loader.load().await;
    Client::new(&config)
}
