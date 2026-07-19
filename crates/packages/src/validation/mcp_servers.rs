use anyhow::Context;

use crate::{PackageKind, ResolvedModule};

pub(crate) fn validate_mcp_server_manifest(module: &ResolvedModule) -> anyhow::Result<()> {
    if module.kind != PackageKind::McpServer {
        return Ok(());
    }

    let manifest = module
        .manifest
        .manifest
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("{} MCP manifest must be an object", module.path))?;
    validate_managed_connector_metadata(manifest, &module.path)?;
    let transport = required_non_empty_string(manifest, "transport", &module.path)?;

    match transport {
        "stdio" => validate_stdio(manifest, &module.path),
        "http" => validate_http(manifest, &module.path),
        other => anyhow::bail!(
            "{} manifest.transport '{}' is unsupported; expected 'stdio' or 'http'",
            module.path,
            other
        ),
    }
}

fn validate_managed_connector_metadata(
    manifest: &serde_json::Map<String, serde_json::Value>,
    module_path: &str,
) -> anyhow::Result<()> {
    let Some(metadata) = manifest.get("metadata").filter(|value| !value.is_null()) else {
        return Ok(());
    };
    if metadata.pointer("/runtime/connector").is_some() {
        anyhow::bail!(
            "{module_path} manifest.metadata.runtime.connector is no longer supported; use manifest.metadata.nenjo.managed_connector"
        );
    }
    let Some(nenjo) = metadata.get("nenjo") else {
        return Ok(());
    };
    let nenjo = nenjo.as_object().ok_or_else(|| {
        anyhow::anyhow!("{module_path} manifest.metadata.nenjo must be an object")
    })?;
    let Some(connector) = nenjo.get("managed_connector") else {
        return Ok(());
    };
    if connector.as_str().map(str::trim).is_none_or(str::is_empty) {
        anyhow::bail!(
            "{module_path} manifest.metadata.nenjo.managed_connector must be a non-empty string"
        );
    }
    Ok(())
}

fn validate_stdio(
    manifest: &serde_json::Map<String, serde_json::Value>,
    module_path: &str,
) -> anyhow::Result<()> {
    required_non_empty_string(manifest, "command", module_path)?;
    reject_present_field(manifest, "url", "stdio", module_path)?;

    if let Some(args) = manifest.get("args").filter(|value| !value.is_null()) {
        let args = args.as_array().ok_or_else(|| {
            anyhow::anyhow!("{module_path} manifest.args must be an array of strings")
        })?;
        for (index, arg) in args.iter().enumerate() {
            if !arg.is_string() {
                anyhow::bail!("{module_path} manifest.args[{index}] must be a string");
            }
        }
    }

    Ok(())
}

fn validate_http(
    manifest: &serde_json::Map<String, serde_json::Value>,
    module_path: &str,
) -> anyhow::Result<()> {
    reject_present_field(manifest, "command", "http", module_path)?;
    reject_present_field(manifest, "args", "http", module_path)?;
    let raw_url = required_non_empty_string(manifest, "url", module_path)?;
    let url = reqwest::Url::parse(raw_url)
        .with_context(|| format!("{module_path} manifest.url must be a valid HTTP URL"))?;
    if !matches!(url.scheme(), "http" | "https") || url.host().is_none() {
        anyhow::bail!("{module_path} manifest.url must use http or https and include a host");
    }

    Ok(())
}

fn required_non_empty_string<'a>(
    manifest: &'a serde_json::Map<String, serde_json::Value>,
    field: &str,
    module_path: &str,
) -> anyhow::Result<&'a str> {
    manifest
        .get(field)
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow::anyhow!("{module_path} requires non-empty manifest.{field}"))
}

fn reject_present_field(
    manifest: &serde_json::Map<String, serde_json::Value>,
    field: &str,
    transport: &str,
    module_path: &str,
) -> anyhow::Result<()> {
    if manifest.get(field).is_some_and(|value| !value.is_null()) {
        anyhow::bail!("{module_path} {transport} MCP server must not define manifest.{field}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::ResourceManifest;

    fn module(manifest: serde_json::Value) -> ResolvedModule {
        ResolvedModule {
            package_name: "connectors".to_string(),
            package_version: "1.0.0".to_string(),
            path: "agent-browser.yaml".to_string(),
            source_path: "agent-browser.yaml".to_string(),
            hash: "hash".to_string(),
            kind: PackageKind::McpServer,
            manifest: ResourceManifest {
                schema: "nenjo.mcp_server.v1".to_string(),
                slug: None,
                root_uri: None,
                selector: None,
                imports: BTreeMap::new(),
                manifest,
            },
            imports: Vec::new(),
            files: Vec::new(),
        }
    }

    fn error(manifest: serde_json::Value) -> String {
        validate_mcp_server_manifest(&module(manifest))
            .expect_err("validation should fail")
            .to_string()
    }

    #[test]
    fn accepts_stdio_command_with_string_arguments() {
        validate_mcp_server_manifest(&module(serde_json::json!({
            "transport": "stdio",
            "command": "agent-browser",
            "args": ["mcp", "--tools", "core"],
            "metadata": {"nenjo": {"managed_connector": "agent_browser"}}
        })))
        .expect("stdio MCP server should be valid");
    }

    #[test]
    fn rejects_stdio_without_command_or_with_url() {
        let missing = error(serde_json::json!({"transport": "stdio"}));
        assert!(missing.contains("requires non-empty manifest.command"));

        let ambiguous = error(serde_json::json!({
            "transport": "stdio",
            "command": "server",
            "url": "https://example.com/mcp"
        }));
        assert!(ambiguous.contains("must not define manifest.url"));
    }

    #[test]
    fn rejects_non_string_stdio_arguments() {
        let error = error(serde_json::json!({
            "transport": "stdio",
            "command": "server",
            "args": ["serve", 42]
        }));

        assert!(error.contains("manifest.args[1] must be a string"));
    }

    #[test]
    fn accepts_http_url() {
        validate_mcp_server_manifest(&module(serde_json::json!({
            "transport": "http",
            "url": "https://example.com/mcp"
        })))
        .expect("HTTP MCP server should be valid");
    }

    #[test]
    fn rejects_invalid_or_ambiguous_http_configuration() {
        let invalid_url = error(serde_json::json!({
            "transport": "http",
            "url": "file:///tmp/mcp.sock"
        }));
        assert!(invalid_url.contains("must use http or https and include a host"));

        let command = error(serde_json::json!({
            "transport": "http",
            "url": "https://example.com/mcp",
            "command": "server"
        }));
        assert!(command.contains("must not define manifest.command"));
    }

    #[test]
    fn rejects_unsupported_transport() {
        let error = error(serde_json::json!({
            "transport": "sse",
            "url": "https://example.com/mcp"
        }));

        assert!(error.contains("unsupported; expected 'stdio' or 'http'"));
    }

    #[test]
    fn rejects_invalid_or_legacy_managed_connector_metadata() {
        let empty = error(serde_json::json!({
            "transport": "stdio",
            "command": "server",
            "metadata": {"nenjo": {"managed_connector": "  "}}
        }));
        assert!(empty.contains("managed_connector must be a non-empty string"));

        let legacy = error(serde_json::json!({
            "transport": "stdio",
            "command": "server",
            "metadata": {"runtime": {"connector": "agent_browser"}}
        }));
        assert!(legacy.contains("runtime.connector is no longer supported"));
    }
}
