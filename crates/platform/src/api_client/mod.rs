//! Typed HTTP client for the Nenjo backend API.
//!
//! The [`ApiClient`] provides convenience methods for every internal worker
//! endpoint, automatically attaching an `X-API-Key` header to every request.

mod error;
mod http;
mod types;

pub use error::ApiClientError;
pub use http::{ApiClient, NoopPayloadCodec, PayloadCodec};
pub use types::*;
