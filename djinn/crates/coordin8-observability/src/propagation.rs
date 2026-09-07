//! W3C Trace Context propagation adapters over HTTP headers / gRPC
//! metadata — the [`opentelemetry::propagation::Extractor`]/[`Injector`]
//! implementations the global propagator needs to read/write a
//! `traceparent` header on plain [`http::HeaderMap`] (server side, before
//! tonic parses the request) and [`tonic::metadata::MetadataMap`] (client
//! side, when building an outgoing request).

use opentelemetry::propagation::{Extractor, Injector};

pub struct HeaderExtractor<'a>(pub &'a http::HeaderMap);

impl Extractor for HeaderExtractor<'_> {
    fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).and_then(|v| v.to_str().ok())
    }

    fn keys(&self) -> Vec<&str> {
        self.0.keys().map(|k| k.as_str()).collect()
    }
}

pub struct MetadataInjector<'a>(pub &'a mut tonic::metadata::MetadataMap);

impl Injector for MetadataInjector<'_> {
    fn set(&mut self, key: &str, value: String) {
        let Ok(key) = tonic::metadata::MetadataKey::from_bytes(key.as_bytes()) else {
            return;
        };
        let Ok(value) = value.parse() else {
            return;
        };
        self.0.insert(key, value);
    }
}
