//! Typed HTTP client for the Nenjo backend API.
//!
//! The [`NenjoClient`] provides convenience methods for every internal worker
//! endpoint, automatically attaching an `X-API-Key` header to every request.

mod error;
mod http;
mod types;

pub use error::ApiClientError;
pub use http::{NenjoClient, PayloadDecoder};
pub use types::*;
