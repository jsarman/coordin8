mod client;
mod event_store;
mod lease_store;
mod registry_store;
mod space_store;
mod table;
mod txn_store;

pub use client::make_dynamo_client;
pub use event_store::DynamoEventStore;
pub use lease_store::DynamoLeaseStore;
pub use registry_store::DynamoRegistryStore;
pub use space_store::DynamoSpaceStore;
pub use txn_store::DynamoTxnStore;
pub use table::{
    ensure_event_mailbox_table, ensure_event_sub_table, ensure_lease_table, ensure_registry_table,
    ensure_space_table, ensure_space_txn_taken_table, ensure_space_uncommitted_table,
    ensure_space_watches_table, ensure_txn_table,
};
