use aws_sdk_dynamodb::{
    Client,
    error::SdkError,
    operation::create_table::CreateTableError,
    types::{
        AttributeDefinition, BillingMode, GlobalSecondaryIndex, KeySchemaElement, KeyType,
        Projection, ProjectionType, ScalarAttributeType, TimeToLiveSpecification,
    },
};
use tracing::{info, warn};

pub const LEASE_TABLE: &str = "coordin8_leases";
pub const RESOURCE_GSI: &str = "resource_id-index";

pub const REGISTRY_TABLE: &str = "coordin8_registry";
pub const REGISTRY_LEASE_GSI: &str = "lease_id-index";

/// Ensure the `coordin8_leases` table (or a custom-named one) exists.
/// Creates it on first call; ignores `ResourceInUseException` on subsequent calls.
/// Also enables DynamoDB native TTL on the `ttl` attribute.
pub async fn ensure_lease_table(client: &Client, table_name: &str) -> Result<(), String> {
    let result = client
        .create_table()
        .table_name(table_name)
        .billing_mode(BillingMode::PayPerRequest)
        // PK
        .attribute_definitions(
            AttributeDefinition::builder()
                .attribute_name("lease_id")
                .attribute_type(ScalarAttributeType::S)
                .build()
                .unwrap(),
        )
        .key_schema(
            KeySchemaElement::builder()
                .attribute_name("lease_id")
                .key_type(KeyType::Hash)
                .build()
                .unwrap(),
        )
        // GSI attribute
        .attribute_definitions(
            AttributeDefinition::builder()
                .attribute_name("resource_id")
                .attribute_type(ScalarAttributeType::S)
                .build()
                .unwrap(),
        )
        // GSI: resource_id-index
        .global_secondary_indexes(
            GlobalSecondaryIndex::builder()
                .index_name(RESOURCE_GSI)
                .key_schema(
                    KeySchemaElement::builder()
                        .attribute_name("resource_id")
                        .key_type(KeyType::Hash)
                        .build()
                        .unwrap(),
                )
                .projection(
                    Projection::builder()
                        .projection_type(ProjectionType::All)
                        .build(),
                )
                .build()
                .unwrap(),
        )
        .send()
        .await;

    match result {
        Ok(_) => {
            info!(table = table_name, "DynamoDB table created");
        }
        Err(SdkError::ServiceError(se)) if matches!(se.err(), CreateTableError::ResourceInUseException(_)) => {
            // Table already exists — that's fine.
        }
        Err(e) => {
            return Err(format!("create_table failed: {e}"));
        }
    }

    // Enable TTL (idempotent — safe to call even if already enabled).
    let ttl_result = client
        .update_time_to_live()
        .table_name(table_name)
        .time_to_live_specification(
            TimeToLiveSpecification::builder()
                .attribute_name("ttl")
                .enabled(true)
                .build()
                .unwrap(),
        )
        .send()
        .await;

    match ttl_result {
        Ok(_) => {
            info!(table = table_name, "DynamoDB TTL enabled on 'ttl' attribute");
        }
        Err(e) => {
            // TTL errors are non-fatal — log and continue.
            warn!(table = table_name, error = %e, "Failed to enable TTL (non-fatal)");
        }
    }

    Ok(())
}

/// Ensure the `coordin8_registry` table (or a custom-named one) exists.
/// Creates it on first call; ignores `ResourceInUseException` on subsequent calls.
/// No TTL needed for the registry table.
pub async fn ensure_registry_table(client: &Client, table_name: &str) -> Result<(), String> {
    let result = client
        .create_table()
        .table_name(table_name)
        .billing_mode(BillingMode::PayPerRequest)
        // PK
        .attribute_definitions(
            AttributeDefinition::builder()
                .attribute_name("capability_id")
                .attribute_type(ScalarAttributeType::S)
                .build()
                .unwrap(),
        )
        .key_schema(
            KeySchemaElement::builder()
                .attribute_name("capability_id")
                .key_type(KeyType::Hash)
                .build()
                .unwrap(),
        )
        // GSI attribute
        .attribute_definitions(
            AttributeDefinition::builder()
                .attribute_name("lease_id")
                .attribute_type(ScalarAttributeType::S)
                .build()
                .unwrap(),
        )
        // GSI: lease_id-index
        .global_secondary_indexes(
            GlobalSecondaryIndex::builder()
                .index_name(REGISTRY_LEASE_GSI)
                .key_schema(
                    KeySchemaElement::builder()
                        .attribute_name("lease_id")
                        .key_type(KeyType::Hash)
                        .build()
                        .unwrap(),
                )
                .projection(
                    Projection::builder()
                        .projection_type(ProjectionType::All)
                        .build(),
                )
                .build()
                .unwrap(),
        )
        .send()
        .await;

    match result {
        Ok(_) => {
            info!(table = table_name, "DynamoDB registry table created");
        }
        Err(SdkError::ServiceError(se))
            if matches!(se.err(), CreateTableError::ResourceInUseException(_)) =>
        {
            // Table already exists — that's fine.
        }
        Err(e) => {
            return Err(format!("create_table failed: {e}"));
        }
    }

    Ok(())
}
