//! Local MCP backend implementations that operate directly on a manifest reader/writer pair.

pub mod executor;

pub use executor::LocalManifestMcpBackend;
