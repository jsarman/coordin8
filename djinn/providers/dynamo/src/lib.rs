mod client;
mod lease_store;
mod registry_store;
mod table;

pub use client::make_dynamo_client;
pub use lease_store::DynamoLeaseStore;
pub use registry_store::DynamoRegistryStore;
pub use table::{ensure_lease_table, ensure_registry_table};
