//! Djinn service boot functions, exposed for integration testing.
//!
//! Each `run_*` function starts the service and blocks until the server shuts
//! down. Call them inside a `tokio::spawn` to run in the background.

pub mod services;
