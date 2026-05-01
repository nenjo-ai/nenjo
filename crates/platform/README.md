# nenjo-platform

Platform-backed manifest and REST API operations for Nenjo workers and tooling.

## Overview

This crate bridges local Nenjo manifest state with platform APIs. It provides:

- `PlatformManifestClient` for HTTP access to platform manifest, project document, task, and execution endpoints
- `ManifestMcpContract` and manifest MCP tool definitions for exposing manifest operations as tools
- `PlatformManifestBackend` for read-through/write-through manifest synchronization
- `LocalManifestMcpBackend` for in-process tests and local manifest execution
- Scope and access-policy helpers for validating platform resource permissions
- Sensitive payload encoder hooks for encrypted prompt and document content

## Common entry points

| Type | Purpose |
|------|---------|
| `PlatformManifestClient` | Thin authenticated HTTP client for platform routes |
| `PlatformManifestBackend` | MCP backend backed by local manifests plus platform persistence |
| `LocalManifestMcpBackend` | MCP backend backed only by local manifest reader/writer traits |
| `ManifestMcpContract` | Static tool registry and dispatcher for manifest MCP calls |
| `ManifestAccessPolicy` | Scope-based filtering and write validation |
| `SensitivePayloadEncoder` | Hook for encrypting/decrypting sensitive manifest payloads |

## Example

```rust,ignore
use nenjo_platform::{ManifestMcpContract, PlatformManifestClient};

let client = PlatformManifestClient::new("https://api.example.com", "api-key")?;
let bootstrap = client.fetch_bootstrap().await?;

let tools = ManifestMcpContract::tools();
```

## License

Licensed under the Apache License, Version 2.0. See [LICENSE](../../LICENSE) for details.
