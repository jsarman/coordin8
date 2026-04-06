mod client;
mod event_store;
mod lease_store;
mod registry_store;
mod space_store;
mod table;
mod txn_store;

/// Returns `true` if `COORDIN8_AUTO_CREATE_TABLES` is set to `"true"` or `"1"`
/// (case-insensitive). All store `init()` methods check this before calling
/// `create_table`. In production the tables are provisioned by CloudFormation;
/// set this env var to `"true"` in local dev or integration tests.
pub(crate) fn auto_create_enabled() -> bool {
    std::env::var("COORDIN8_AUTO_CREATE_TABLES")
        .map(|v| matches!(v.to_lowercase().as_str(), "true" | "1"))
        .unwrap_or(false)
}

pub use client::make_dynamo_client;
pub use event_store::DynamoEventStore;
pub use lease_store::DynamoLeaseStore;
pub use registry_store::DynamoRegistryStore;
pub use space_store::DynamoSpaceStore;
pub use table::{
    ensure_event_mailbox_table, ensure_event_sub_table, ensure_lease_table, ensure_registry_table,
    ensure_space_table, ensure_space_txn_taken_table, ensure_space_uncommitted_table,
    ensure_space_watches_table, ensure_txn_table,
};
pub use txn_store::DynamoTxnStore;
