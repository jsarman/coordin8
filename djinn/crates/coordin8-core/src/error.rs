use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("lease not found: {0}")]
    LeaseNotFound(String),

    #[error("lease expired: {0}")]
    LeaseExpired(String),

    #[error("resource not found: {0}")]
    ResourceNotFound(String),

    #[error("no match found for template")]
    NoMatch,

    #[error("storage error: {0}")]
    Storage(String),

    #[error("subscription not found: {0}")]
    SubscriptionNotFound(String),

    #[error("subscription expired: {0}")]
    SubscriptionExpired(String),

    #[error("internal error: {0}")]
    Internal(String),
}
