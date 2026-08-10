//! Agent-facing immutable artifact publication and catalog browsing tools.

use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use base64::Engine;
use cap_std::{ambient_authority, fs::Dir};
use nenjo::{Tool, ToolCategory, ToolOrigin, ToolResult};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::sync::Semaphore;
use uuid::Uuid;

use crate::{ContentScope, PlatformManifestClient, SensitivePayloadEncoder};

const ARTIFACT_CONTENT_OBJECT_TYPE: &str = "artifact.content";
const MAX_ARTIFACT_BYTES: u64 = 16 * 1024 * 1024;
static ARTIFACT_UPLOAD_PERMIT: Semaphore = Semaphore::const_new(1);

/// Request body for creating a signed artifact upload.
#[derive(Debug, Clone, Serialize)]
pub struct CreateArtifactUploadRequest {
    /// Client-generated immutable artifact identity.
    pub artifact_id: Uuid,
    /// Display name without folder components.
    pub name: String,
    /// Media type of the plaintext file.
    pub media_type: String,
    /// Exact plaintext length before envelope encryption.
    pub plaintext_size_bytes: u64,
    /// Exact serialized encrypted-envelope length.
    pub ciphertext_size_bytes: u64,
    /// SHA-256 digest of plaintext bytes.
    pub plaintext_digest: String,
    /// SHA-256 digest of serialized encrypted-envelope bytes.
    pub ciphertext_digest: String,
    /// Optional organization-relative catalog destination.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub publish_path: Option<String>,
}

/// Signed upload grant returned by the platform.
#[derive(Debug, Clone, Deserialize)]
pub struct ArtifactUploadGrant {
    /// Stable signed-upload session identity.
    pub upload_id: Uuid,
    /// Artifact identity being uploaded.
    pub artifact_id: Uuid,
    /// Short-lived object-store PUT URL.
    pub upload_url: Option<String>,
    /// Required encrypted envelope media type.
    pub content_type: String,
    /// Digest metadata that must accompany the signed PUT.
    pub ciphertext_digest: String,
    /// RFC 3339 upload expiration timestamp.
    pub expires_at: String,
    /// True when this replay already refers to a finalized artifact.
    #[serde(default)]
    pub already_ready: bool,
}

/// Immutable artifact metadata returned after finalization.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactRecord {
    /// Immutable artifact identity.
    pub id: Uuid,
    /// Upload lifecycle state.
    pub state: String,
    /// Artifact display name.
    pub name: String,
    /// Plaintext media type.
    pub media_type: String,
    /// Plaintext byte length.
    pub plaintext_size_bytes: i64,
    /// Encrypted envelope byte length.
    pub ciphertext_size_bytes: i64,
    /// SHA-256 plaintext digest.
    pub plaintext_digest: String,
    /// SHA-256 encrypted envelope digest.
    pub ciphertext_digest: String,
    /// RFC 3339 creation timestamp.
    pub created_at: String,
    /// RFC 3339 finalization timestamp when ready.
    pub ready_at: Option<String>,
}

/// Folder row returned by the artifact catalog view.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactFolderRecord {
    /// Stable catalog-folder identity.
    pub folder_id: Uuid,
    /// Folder display name.
    pub name: String,
    /// Organization-relative folder path.
    pub path: String,
}

/// Placed artifact row returned by the artifact catalog view.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactEntryRecord {
    /// Stable catalog-entry identity.
    pub entry_id: Uuid,
    /// Immutable artifact referenced by this entry.
    pub artifact_id: Uuid,
    /// Entry display name.
    pub name: String,
    /// Organization-relative entry path.
    pub path: String,
    /// Plaintext media type.
    pub media_type: String,
    /// Plaintext byte length.
    pub size_bytes: i64,
    /// SHA-256 plaintext digest.
    pub digest: String,
    /// RFC 3339 entry creation timestamp.
    pub created_at: String,
}

/// Bounded artifact folder tree returned to agents.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactCatalogRecord {
    /// Requested catalog root path.
    pub path: String,
    /// Descendant folders within the requested depth.
    pub folders: Vec<ArtifactFolderRecord>,
    /// Ready artifacts within the requested depth.
    pub artifacts: Vec<ArtifactEntryRecord>,
}

/// Dependencies for the two artifact tools.
pub struct PlatformArtifactToolsBackend<E> {
    /// Authenticated platform client used for metadata operations.
    pub client: Arc<PlatformManifestClient>,
    /// Organization content-envelope encoder.
    pub payload_encoder: E,
    /// Organization identity cached from worker bootstrap when available.
    pub cached_org_id: Option<Uuid>,
    /// Capability-scoped root for workspace-relative source files.
    pub workspace_root: PathBuf,
}

impl<E: Clone> Clone for PlatformArtifactToolsBackend<E> {
    fn clone(&self) -> Self {
        Self {
            client: Arc::clone(&self.client),
            payload_encoder: self.payload_encoder.clone(),
            cached_org_id: self.cached_org_id,
            workspace_root: self.workspace_root.clone(),
        }
    }
}

/// Add `view_artifacts` and `upload_artifact` when their platform backend is available.
pub fn add_artifact_tools<E>(
    tools: &mut Vec<Arc<dyn Tool>>,
    backend: Option<PlatformArtifactToolsBackend<E>>,
) where
    E: SensitivePayloadEncoder + Clone + Send + Sync + 'static,
{
    let Some(backend) = backend else {
        return;
    };
    if !tools.iter().any(|tool| tool.name() == "view_artifacts") {
        tools.push(Arc::new(ViewArtifactsTool {
            backend: backend.clone(),
        }));
    }
    if !tools.iter().any(|tool| tool.name() == "upload_artifact") {
        tools.push(Arc::new(UploadArtifactTool { backend }));
    }
}

struct ViewArtifactsTool<E> {
    backend: PlatformArtifactToolsBackend<E>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ViewArtifactsArgs {
    #[serde(default)]
    path: String,
    #[serde(default = "default_depth")]
    depth: u32,
}

fn default_depth() -> u32 {
    2
}

#[async_trait]
impl<E> Tool for ViewArtifactsTool<E>
where
    E: SensitivePayloadEncoder + Clone + Send + Sync + 'static,
{
    fn name(&self) -> &str {
        "view_artifacts"
    }

    fn description(&self) -> &str {
        "View uploaded artifacts and their user-facing folder organization. Paths are catalog paths, not workspace paths."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "path": {"type": "string", "description": "Optional catalog folder path relative to the organization artifact root."},
                "depth": {"type": "integer", "minimum": 1, "maximum": 5, "description": "Folder depth to return; defaults to 2."}
            }
        })
    }

    async fn execute(&self, args: Value) -> Result<ToolResult> {
        let args: ViewArtifactsArgs =
            serde_json::from_value(args).context("view_artifacts arguments are invalid")?;
        let catalog = self
            .backend
            .client
            .view_artifacts(&args.path, args.depth.clamp(1, 5))
            .await?;
        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&catalog)?,
            error: None,
        })
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Read
    }

    fn origin(&self) -> ToolOrigin {
        ToolOrigin::Platform
    }
}

struct UploadArtifactTool<E> {
    backend: PlatformArtifactToolsBackend<E>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct UploadArtifactArgs {
    source_path: String,
    #[serde(default)]
    publish_path: Option<String>,
    #[serde(default)]
    media_type: Option<String>,
}

#[async_trait]
impl<E> Tool for UploadArtifactTool<E>
where
    E: SensitivePayloadEncoder + Clone + Send + Sync + 'static,
{
    fn name(&self) -> &str {
        "upload_artifact"
    }

    fn description(&self) -> &str {
        "Publish an immutable encrypted snapshot of a workspace file up to 16 MiB. source_path is workspace-relative; publish_path is an optional, independent artifact-catalog path whose missing parent folders are created automatically. Returns an artifact_id suitable for JSON Schema format nenjo-artifact-id."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "source_path": {"type": "string", "minLength": 1, "description": "Workspace-relative regular file to snapshot."},
                "publish_path": {"type": "string", "minLength": 1, "description": "Optional user-facing catalog path, which may differ from source_path."},
                "media_type": {"type": "string", "minLength": 3, "description": "Optional plaintext media type; inferred from the extension when omitted."}
            },
            "required": ["source_path"]
        })
    }

    async fn execute(&self, args: Value) -> Result<ToolResult> {
        let args: UploadArtifactArgs =
            serde_json::from_value(args).context("upload_artifact arguments are invalid")?;
        let _upload_permit = ARTIFACT_UPLOAD_PERMIT
            .acquire()
            .await
            .context("artifact upload coordinator is closed")?;
        let file = read_workspace_file(&self.backend.workspace_root, &args.source_path)?;
        let artifact_id = Uuid::new_v4();
        let org_id = match self.backend.cached_org_id {
            Some(org_id) => org_id,
            None => self.backend.client.current_org_id().await?,
        };
        let plaintext_digest = digest(&file.bytes);
        let encoded = self
            .backend
            .payload_encoder
            .encode_payload_with_scope(
                ContentScope::Org,
                org_id,
                artifact_id,
                ARTIFACT_CONTENT_OBJECT_TYPE,
                &json!({
                    "content_base64": base64::engine::general_purpose::STANDARD.encode(&file.bytes)
                }),
            )
            .await?
            .context("artifact payload encoder did not return encrypted content")?;
        let ciphertext = serde_json::to_vec(&encoded)?;
        let media_type = args
            .media_type
            .unwrap_or_else(|| infer_media_type(&file.file_name).to_string());
        let publish_name = args
            .publish_path
            .as_deref()
            .and_then(|path| path.rsplit('/').next())
            .filter(|name| !name.is_empty())
            .unwrap_or(&file.file_name)
            .to_string();
        let catalog_path = args.publish_path.clone();
        let grant = self
            .backend
            .client
            .create_artifact_upload(&CreateArtifactUploadRequest {
                artifact_id,
                name: publish_name,
                media_type,
                plaintext_size_bytes: u64::try_from(file.bytes.len()).unwrap_or(u64::MAX),
                ciphertext_size_bytes: u64::try_from(ciphertext.len()).unwrap_or(u64::MAX),
                plaintext_digest,
                ciphertext_digest: digest(&ciphertext),
                publish_path: args.publish_path,
            })
            .await?;
        self.backend
            .client
            .put_signed_artifact(&grant, ciphertext)
            .await?;
        let artifact = self
            .backend
            .client
            .complete_artifact_upload(grant.upload_id)
            .await?;
        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&json!({
                "artifact_id": artifact.id,
                "name": artifact.name,
                "media_type": artifact.media_type,
                "size_bytes": artifact.plaintext_size_bytes,
                "digest": artifact.plaintext_digest,
                "catalog_path": catalog_path
            }))?,
            error: None,
        })
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Write
    }

    fn origin(&self) -> ToolOrigin {
        ToolOrigin::Platform
    }
}

struct WorkspaceFile {
    file_name: String,
    bytes: Vec<u8>,
}

fn read_workspace_file(root: &Path, raw_path: &str) -> Result<WorkspaceFile> {
    let path = Path::new(raw_path);
    if raw_path.trim().is_empty()
        || path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        bail!("source_path must be a safe workspace-relative path");
    }
    let workspace = Dir::open_ambient_dir(root, ambient_authority())
        .context("artifact workspace root is unavailable")?;
    let source = workspace
        .open(path)
        .with_context(|| format!("artifact source '{}' does not exist", raw_path))?;
    let metadata = source.metadata()?;
    if !metadata.is_file() || metadata.len() > MAX_ARTIFACT_BYTES {
        bail!("artifact source must be a regular file no larger than {MAX_ARTIFACT_BYTES} bytes");
    }
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .context("artifact source has no valid file name")?
        .to_string();
    let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or_default());
    source
        .take(MAX_ARTIFACT_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > usize::try_from(MAX_ARTIFACT_BYTES).unwrap_or(usize::MAX) {
        bail!("artifact source changed while reading and exceeds {MAX_ARTIFACT_BYTES} bytes");
    }
    Ok(WorkspaceFile { file_name, bytes })
}

fn infer_media_type(file_name: &str) -> &'static str {
    match Path::new(file_name)
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("pdf") => "application/pdf",
        Some("json") => "application/json",
        Some("md") => "text/markdown",
        Some("txt") => "text/plain",
        Some("csv") => "text/csv",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("webp") => "image/webp",
        _ => "application/octet-stream",
    }
}

fn digest(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_paths_reject_parent_traversal() {
        let root = tempfile::tempdir().unwrap();
        assert!(read_workspace_file(root.path(), "../secret.pdf").is_err());
    }

    #[test]
    fn media_type_inference_is_bounded_and_predictable() {
        assert_eq!(infer_media_type("report.PDF"), "application/pdf");
        assert_eq!(infer_media_type("archive.bin"), "application/octet-stream");
    }
}
