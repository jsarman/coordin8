use aws_sdk_dynamodb::{
    error::SdkError,
    operation::create_table::CreateTableError,
    types::{
        AttributeDefinition, BillingMode, GlobalSecondaryIndex, KeySchemaElement, KeyType,
        Projection, ProjectionType, ScalarAttributeType, TimeToLiveSpecification,
    },
    Client,
};
use tracing::{info, warn};

pub const LEASE_TABLE: &str = "coordin8_leases";
pub const RESOURCE_GSI: &str = "resource_id-index";

pub const REGISTRY_TABLE: &str = "coordin8_registry";
pub const REGISTRY_LEASE_GSI: &str = "lease_id-index";

pub const TXN_TABLE: &str = "coordin8_txn";

pub const EVENT_SUB_TABLE: &str = "coordin8_event_subscriptions";
pub const EVENT_SUB_LEASE_GSI: &str = "lease_id-index";
pub const EVENT_MAILBOX_TABLE: &str = "coordin8_event_mailbox";

pub const SPACE_TABLE: &str = "coordin8_space";
pub const SPACE_LEASE_GSI: &str = "lease_id-index";
pub const SPACE_UNCOMMITTED_TABLE: &str = "coordin8_space_uncommitted";
pub const SPACE_TXN_TAKEN_TABLE: &str = "coordin8_space_txn_taken";
pub const SPACE_WATCHES_TABLE: &str = "coordin8_space_watches";
pub const SPACE_WATCHES_LEASE_GSI: &str = "lease_id-index";

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

    handle_create_result(result, table_name)?;

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
            info!(
                table = table_name,
                "DynamoDB TTL enabled on 'ttl' attribute"
            );
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

    handle_create_result(result, table_name)?;
    Ok(())
}

/// Ensure the `coordin8_txn` table exists. PK = txn_id. No GSI needed.
pub async fn ensure_txn_table(client: &Client, table_name: &str) -> Result<(), String> {
    let result = client
        .create_table()
        .table_name(table_name)
        .billing_mode(BillingMode::PayPerRequest)
        .attribute_definitions(
            AttributeDefinition::builder()
                .attribute_name("txn_id")
                .attribute_type(ScalarAttributeType::S)
                .build()
                .unwrap(),
        )
        .key_schema(
            KeySchemaElement::builder()
                .attribute_name("txn_id")
                .key_type(KeyType::Hash)
                .build()
                .unwrap(),
        )
        .send()
        .await;

    handle_create_result(result, table_name)?;
    Ok(())
}

/// Ensure the event subscriptions table exists. PK = registration_id, GSI on lease_id.
pub async fn ensure_event_sub_table(client: &Client, table_name: &str) -> Result<(), String> {
    let result = client
        .create_table()
        .table_name(table_name)
        .billing_mode(BillingMode::PayPerRequest)
        .attribute_definitions(
            AttributeDefinition::builder()
                .attribute_name("registration_id")
                .attribute_type(ScalarAttributeType::S)
                .build()
                .unwrap(),
        )
        .key_schema(
            KeySchemaElement::builder()
                .attribute_name("registration_id")
                .key_type(KeyType::Hash)
                .build()
                .unwrap(),
        )
        .attribute_definitions(
            AttributeDefinition::builder()
                .attribute_name("lease_id")
                .attribute_type(ScalarAttributeType::S)
                .build()
                .unwrap(),
        )
        .global_secondary_indexes(
            GlobalSecondaryIndex::builder()
                .index_name(EVENT_SUB_LEASE_GSI)
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

    handle_create_result(result, table_name)?;
    Ok(())
}

/// Ensure the event mailbox table exists. PK = registration_id, SK = seq_num.
pub async fn ensure_event_mailbox_table(client: &Client, table_name: &str) -> Result<(), String> {
    let result = client
        .create_table()
        .table_name(table_name)
        .billing_mode(BillingMode::PayPerRequest)
        .attribute_definitions(
            AttributeDefinition::builder()
                .attribute_name("registration_id")
                .attribute_type(ScalarAttributeType::S)
                .build()
                .unwrap(),
        )
        .attribute_definitions(
            AttributeDefinition::builder()
                .attribute_name("seq_num")
                .attribute_type(ScalarAttributeType::N)
                .build()
                .unwrap(),
        )
        .key_schema(
            KeySchemaElement::builder()
                .attribute_name("registration_id")
                .key_type(KeyType::Hash)
                .build()
                .unwrap(),
        )
        .key_schema(
            KeySchemaElement::builder()
                .attribute_name("seq_num")
                .key_type(KeyType::Range)
                .build()
                .unwrap(),
        )
        .send()
        .await;

    handle_create_result(result, table_name)?;
    Ok(())
}

/// Ensure the space tuples table exists. PK = tuple_id, GSI on lease_id.
pub async fn ensure_space_table(client: &Client, table_name: &str) -> Result<(), String> {
    let result = client
        .create_table()
        .table_name(table_name)
        .billing_mode(BillingMode::PayPerRequest)
        .attribute_definitions(
            AttributeDefinition::builder()
                .attribute_name("tuple_id")
                .attribute_type(ScalarAttributeType::S)
                .build()
                .unwrap(),
        )
        .key_schema(
            KeySchemaElement::builder()
                .attribute_name("tuple_id")
                .key_type(KeyType::Hash)
                .build()
                .unwrap(),
        )
        .attribute_definitions(
            AttributeDefinition::builder()
                .attribute_name("lease_id")
                .attribute_type(ScalarAttributeType::S)
                .build()
                .unwrap(),
        )
        .global_secondary_indexes(
            GlobalSecondaryIndex::builder()
                .index_name(SPACE_LEASE_GSI)
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

    handle_create_result(result, table_name)?;
    Ok(())
}

/// Ensure the space uncommitted buffer table exists. PK = txn_id, SK = tuple_id.
pub async fn ensure_space_uncommitted_table(
    client: &Client,
    table_name: &str,
) -> Result<(), String> {
    let result = client
        .create_table()
        .table_name(table_name)
        .billing_mode(BillingMode::PayPerRequest)
        .attribute_definitions(
            AttributeDefinition::builder()
                .attribute_name("txn_id")
                .attribute_type(ScalarAttributeType::S)
                .build()
                .unwrap(),
        )
        .attribute_definitions(
            AttributeDefinition::builder()
                .attribute_name("tuple_id")
                .attribute_type(ScalarAttributeType::S)
                .build()
                .unwrap(),
        )
        .key_schema(
            KeySchemaElement::builder()
                .attribute_name("txn_id")
                .key_type(KeyType::Hash)
                .build()
                .unwrap(),
        )
        .key_schema(
            KeySchemaElement::builder()
                .attribute_name("tuple_id")
                .key_type(KeyType::Range)
                .build()
                .unwrap(),
        )
        .send()
        .await;

    handle_create_result(result, table_name)?;
    Ok(())
}

/// Ensure the space txn-taken buffer table exists. PK = txn_id, SK = tuple_id.
pub async fn ensure_space_txn_taken_table(client: &Client, table_name: &str) -> Result<(), String> {
    let result = client
        .create_table()
        .table_name(table_name)
        .billing_mode(BillingMode::PayPerRequest)
        .attribute_definitions(
            AttributeDefinition::builder()
                .attribute_name("txn_id")
                .attribute_type(ScalarAttributeType::S)
                .build()
                .unwrap(),
        )
        .attribute_definitions(
            AttributeDefinition::builder()
                .attribute_name("tuple_id")
                .attribute_type(ScalarAttributeType::S)
                .build()
                .unwrap(),
        )
        .key_schema(
            KeySchemaElement::builder()
                .attribute_name("txn_id")
                .key_type(KeyType::Hash)
                .build()
                .unwrap(),
        )
        .key_schema(
            KeySchemaElement::builder()
                .attribute_name("tuple_id")
                .key_type(KeyType::Range)
                .build()
                .unwrap(),
        )
        .send()
        .await;

    handle_create_result(result, table_name)?;
    Ok(())
}

/// Ensure the space watches table exists. PK = watch_id, GSI on lease_id.
pub async fn ensure_space_watches_table(client: &Client, table_name: &str) -> Result<(), String> {
    let result = client
        .create_table()
        .table_name(table_name)
        .billing_mode(BillingMode::PayPerRequest)
        .attribute_definitions(
            AttributeDefinition::builder()
                .attribute_name("watch_id")
                .attribute_type(ScalarAttributeType::S)
                .build()
                .unwrap(),
        )
        .key_schema(
            KeySchemaElement::builder()
                .attribute_name("watch_id")
                .key_type(KeyType::Hash)
                .build()
                .unwrap(),
        )
        .attribute_definitions(
            AttributeDefinition::builder()
                .attribute_name("lease_id")
                .attribute_type(ScalarAttributeType::S)
                .build()
                .unwrap(),
        )
        .global_secondary_indexes(
            GlobalSecondaryIndex::builder()
                .index_name(SPACE_WATCHES_LEASE_GSI)
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

    handle_create_result(result, table_name)?;
    Ok(())
}

/// Shared helper for create_table result handling.
fn handle_create_result(
    result: Result<
        aws_sdk_dynamodb::operation::create_table::CreateTableOutput,
        SdkError<CreateTableError>,
    >,
    table_name: &str,
) -> Result<(), String> {
    match result {
        Ok(_) => {
            info!(table = table_name, "DynamoDB table created");
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
