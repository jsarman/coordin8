pub mod local_resolver;
pub mod manager;
pub mod service;

pub use local_resolver::LocalCapabilityResolver;
pub use manager::{ProxyConfig, ProxyManager};
pub use service::ProxyServiceImpl;
