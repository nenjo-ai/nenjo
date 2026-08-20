//! Agent-facing immutable artifact publication and catalog browsing tools.

use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use base64::Engine;
use cap_std::{ambient_authority, fs::Dir};
use nenjo::{Tool, ToolCategory, ToolOrigin, ToolResult};
use nenjo_content::ArtifactId;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::sync::Semaphore;
use uuid::Uuid;

use crate::{
    ContentScope, ManifestAccessPolicy, PlatformManifestClient, ScopeResource,
    SensitivePayloadEncoder,
    artifacts::{ArtifactMetadata, PlatformArtifactMaterializer},
};

const ARTIFACT_CONTENT_OBJECT_TYPE: &str = "artifact.content";
const MAX_ARTIFACT_BYTES: u64 = 16 * 1024 * 1024;
const DEFAULT_LINE_COUNT: usize = 500;
const MAX_LINE_COUNT: usize = 2_000;
const MAX_OUTPUT_BYTES: usize = 256 * 1024;
static ARTIFACT_UPLOAD_PERMIT: Semaphore = Semaphore::const_new(1);

/// Request body for creating a signed artifact upload.
#[derive(Debug, Clone, Serialize)]
pub struct CreateArtifactUploadRequest {
    /// Client-generated immutable artifact identity.
    pub artifact_id: Uuid,
    /// Current immutable artifact that this upload explicitly revises.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revision_of: Option<Uuid>,
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
    /// Exact byte length bound into the signed PUT.
    pub content_length: u64,
    /// Base64 SHA-256 checksum bound into the signed PUT.
    pub checksum_sha256: String,
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
    /// Authenticated owning organization.
    pub org_id: Uuid,
    /// Upload lifecycle state.
    pub state: String,
    /// Stable identity shared by immutable revisions of the logical artifact.
    pub lineage_id: Uuid,
    /// Monotonic revision number within the lineage.
    pub revision_number: i32,
    /// Immediately preceding immutable revision.
    pub previous_artifact_id: Option<Uuid>,
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

/// Artifact detail returned by the metadata endpoint.
#[derive(Debug, Clone, Deserialize)]
pub struct ArtifactDetailRecord {
    pub artifact: ArtifactRecord,
}

/// Downloaded immutable encrypted artifact and its authoritative metadata.
pub struct DownloadedArtifact {
    pub artifact: ArtifactRecord,
    pub ciphertext: Vec<u8>,
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
    /// Stable lineage shared by the entry's immutable revisions.
    pub lineage_id: Uuid,
    /// Current immutable revision number.
    pub revision_number: i32,
    /// Number of ready revisions retained for this entry.
    pub revision_count: i64,
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
    /// Current ready revisions without a catalog placement.
    #[serde(default)]
    pub unfiled: Vec<UnfiledArtifactRecord>,
}

/// Current immutable artifact without a catalog placement.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnfiledArtifactRecord {
    pub artifact_id: Uuid,
    pub lineage_id: Uuid,
    pub revision_number: i32,
    pub revision_count: i64,
    pub name: String,
    pub media_type: String,
    pub size_bytes: i64,
    pub digest: String,
    pub created_at: String,
}

/// Dependencies for the organization artifact tools.
pub struct PlatformArtifactToolsBackend<E> {
    /// Authenticated platform client used for metadata operations.
    pub client: Arc<PlatformManifestClient>,
    /// Organization content-envelope encoder.
    pub payload_encoder: E,
    /// Organization identity cached from worker bootstrap when available.
    pub cached_org_id: Option<Uuid>,
    /// Capability-scoped root for workspace-relative source files.
    pub workspace_root: PathBuf,
    /// Shared authenticated materializer used by tools and model-input preparation.
    pub materializer: Arc<PlatformArtifactMaterializer<E>>,
}

impl<E: Clone> Clone for PlatformArtifactToolsBackend<E> {
    fn clone(&self) -> Self {
        Self {
            client: Arc::clone(&self.client),
            payload_encoder: self.payload_encoder.clone(),
            cached_org_id: self.cached_org_id,
            workspace_root: self.workspace_root.clone(),
            materializer: Arc::clone(&self.materializer),
        }
    }
}

/// Add artifact tools allowed by the agent's platform access policy.
pub fn add_artifact_tools<E>(
    tools: &mut Vec<Arc<dyn Tool>>,
    backend: Option<PlatformArtifactToolsBackend<E>>,
    policy: &ManifestAccessPolicy,
) where
    E: SensitivePayloadEncoder + Clone + Send + Sync + 'static,
{
    let Some(backend) = backend else {
        return;
    };
    if policy.can_read_resource(ScopeResource::Artifacts) {
        if !tools.iter().any(|tool| tool.name() == "list_artifacts") {
            tools.push(Arc::new(ListArtifactsTool {
                backend: backend.clone(),
            }));
        }
        if !tools.iter().any(|tool| tool.name() == "read_artifact") {
            tools.push(Arc::new(ReadArtifactTool {
                backend: backend.clone(),
            }));
        }
    }
    if policy.can_write_resource(ScopeResource::Artifacts)
        && !tools.iter().any(|tool| tool.name() == "upload_artifact")
    {
        tools.push(Arc::new(UploadArtifactTool { backend }));
    }
}

struct ListArtifactsTool<E> {
    backend: PlatformArtifactToolsBackend<E>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ListArtifactsArgs {
    #[serde(default)]
    path: String,
}

#[derive(Debug, Serialize)]
struct ArtifactDirectoryListing {
    path: String,
    folders: Vec<ArtifactFolderRecord>,
    artifacts: Vec<ArtifactEntryRecord>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    unfiled: Vec<UnfiledArtifactRecord>,
}

#[async_trait]
impl<E> Tool for ListArtifactsTool<E>
where
    E: SensitivePayloadEncoder + Clone + Send + Sync + 'static,
{
    fn name(&self) -> &str {
        "list_artifacts"
    }

    fn description(&self) -> &str {
        "List the immediate child folders and current artifact revisions at an organization artifact-catalog path. Paths are catalog paths, not workspace paths."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "path": {"type": "string", "description": "Optional catalog folder path relative to the organization artifact root."}
            }
        })
    }

    async fn execute(&self, args: Value) -> Result<ToolResult> {
        let args: ListArtifactsArgs =
            serde_json::from_value(args).context("list_artifacts arguments are invalid")?;
        let path = ArtifactCatalogDirectoryPath::parse(&args.path)?;
        let catalog = self.backend.client.list_artifacts(path.as_str()).await?;
        let listing = direct_children(catalog);
        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&listing)?.into(),
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

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum ArtifactSelector {
    Path { path: String },
    ArtifactId { artifact_id: Uuid },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadArtifactArgs {
    selector: ArtifactSelector,
    #[serde(default)]
    view: ArtifactReadView,
    #[serde(default = "default_start_line")]
    start_line: usize,
    #[serde(default = "default_line_count")]
    line_count: usize,
}

/// Select whether an artifact should be read as bounded text or handed to the
/// model-input router for automatic inspection.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ArtifactReadView {
    #[default]
    Automatic,
    Text,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ArtifactReadPlan {
    Text,
    ModelInput,
}

impl ArtifactReadPlan {
    fn resolve(view: ArtifactReadView, media_type: &str) -> Result<Self> {
        if is_textual_media_type(media_type) {
            return Ok(Self::Text);
        }
        match view {
            ArtifactReadView::Automatic => Ok(Self::ModelInput),
            ArtifactReadView::Text => bail!(
                "artifact media type '{media_type}' cannot be represented as UTF-8 text; use view='automatic' for model inspection"
            ),
        }
    }
}

const fn default_start_line() -> usize {
    1
}

const fn default_line_count() -> usize {
    DEFAULT_LINE_COUNT
}

struct ReadArtifactTool<E> {
    backend: PlatformArtifactToolsBackend<E>,
}

#[async_trait]
impl<E> Tool for ReadArtifactTool<E>
where
    E: SensitivePayloadEncoder + Clone + Send + Sync + 'static,
{
    fn name(&self) -> &str {
        "read_artifact"
    }

    fn description(&self) -> &str {
        "Read an organization artifact. Automatic view returns bounded lines for text and a typed immutable artifact input for model inspection of images, documents, audio, or video. Text view requires UTF-8 textual media. A catalog path resolves the current revision; an artifact ID selects one immutable revision."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "selector": {
                    "oneOf": [
                        {
                            "type": "object",
                            "additionalProperties": false,
                            "properties": {
                                "kind": {"const": "path"},
                                "path": {"type": "string", "minLength": 1, "description": "Organization-relative artifact catalog path."}
                            },
                            "required": ["kind", "path"]
                        },
                        {
                            "type": "object",
                            "additionalProperties": false,
                            "properties": {
                                "kind": {"const": "artifact_id"},
                                "artifact_id": {"type": "string", "format": "uuid"}
                            },
                            "required": ["kind", "artifact_id"]
                        }
                    ]
                },
                "view": {
                    "type": "string",
                    "enum": ["automatic", "text"],
                    "default": "automatic",
                    "description": "Use automatic for media-aware model inspection, or text to require a bounded UTF-8 line read."
                },
                "start_line": {"type": "integer", "minimum": 1, "default": 1},
                "line_count": {"type": "integer", "minimum": 1, "maximum": MAX_LINE_COUNT, "default": DEFAULT_LINE_COUNT}
            },
            "required": ["selector"]
        })
    }

    async fn execute(&self, args: Value) -> Result<ToolResult> {
        let args: ReadArtifactArgs =
            serde_json::from_value(args).context("read_artifact arguments are invalid")?;
        let (artifact_id, catalog_path) = match args.selector {
            ArtifactSelector::Path { path } => {
                let artifact = resolve_artifact_path(&self.backend.client, &path).await?;
                (artifact.artifact_id, Some(artifact.path))
            }
            ArtifactSelector::ArtifactId { artifact_id } => (artifact_id, None),
        };
        let org_id = current_org_id(&self.backend).await?;
        let metadata = self
            .backend
            .materializer
            .resolve_metadata(org_id, ArtifactId::parse(artifact_id)?)
            .await?;
        let plan =
            ArtifactReadPlan::resolve(args.view, metadata.reference().media_type().essence_str())?;
        let header = render_artifact_header(&metadata, catalog_path.as_deref());
        if plan == ArtifactReadPlan::ModelInput {
            // Verify/decrypt now so the tool reports authorization or content
            // failures directly. Request preparation reuses the shared cache.
            self.backend
                .materializer
                .materialize_metadata(org_id, metadata.clone())
                .await?;
            return Ok(model_input_result(&metadata, catalog_path.as_deref()));
        }
        if args.start_line == 0 || args.line_count == 0 || args.line_count > MAX_LINE_COUNT {
            bail!(
                "read_artifact line ranges must be positive and line_count cannot exceed {MAX_LINE_COUNT}"
            );
        }
        let materialized = self
            .backend
            .materializer
            .materialize_metadata(org_id, metadata.clone())
            .await?;
        let contents = std::str::from_utf8(materialized.bytes())
            .context("artifact declares textual content but is not valid UTF-8")?;
        let rendered = render_artifact_lines(contents, args.start_line, args.line_count)?;
        Ok(ToolResult {
            success: true,
            output: format!("{header}---\n{rendered}").into(),
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

fn model_input_result(metadata: &ArtifactMetadata, catalog_path: Option<&str>) -> ToolResult {
    let header = render_artifact_header(metadata, catalog_path);
    ToolResult::success(format!(
        "{header}inspection: automatic model input\n---\nThe immutable artifact is attached as a typed input for inspection."
    ))
    .with_artifact(metadata.reference().clone())
}

fn render_artifact_header(metadata: &ArtifactMetadata, catalog_path: Option<&str>) -> String {
    let path = catalog_path
        .map(|path| format!("catalog_path: {path}\n"))
        .unwrap_or_default();
    format!(
        "artifact_id: {}\nlineage_id: {}\nrevision: {}\n{}media_type: {}\nsize_bytes: {}\ndigest: {}\n",
        metadata.reference().id(),
        metadata.lineage_id(),
        metadata.revision_number(),
        path,
        metadata.reference().media_type(),
        metadata.reference().size().bytes(),
        metadata.reference().digest(),
    )
}

fn direct_children(catalog: ArtifactCatalogRecord) -> ArtifactDirectoryListing {
    let base = catalog.path.trim_matches('/').to_string();
    let folders = catalog
        .folders
        .into_iter()
        .filter(|folder| parent_catalog_path(&folder.path) == base)
        .collect();
    let artifacts = catalog
        .artifacts
        .into_iter()
        .filter(|artifact| parent_catalog_path(&artifact.path) == base)
        .collect();
    ArtifactDirectoryListing {
        path: base,
        folders,
        artifacts,
        unfiled: catalog.unfiled,
    }
}

fn parent_catalog_path(path: &str) -> String {
    path.rsplit_once('/')
        .map(|(parent, _)| parent.to_string())
        .unwrap_or_default()
}

async fn resolve_artifact_path(
    client: &PlatformManifestClient,
    raw_path: &str,
) -> Result<ArtifactEntryRecord> {
    let path = ArtifactCatalogFilePath::parse(raw_path)?;
    let catalog = client.list_artifacts(path.parent()).await?;
    catalog
        .artifacts
        .into_iter()
        .find(|artifact| artifact.path.eq_ignore_ascii_case(path.as_str()))
        .with_context(|| format!("artifact catalog path '{}' was not found", path.as_str()))
}

struct ArtifactCatalogFilePath(String);

impl ArtifactCatalogFilePath {
    fn parse(raw: &str) -> Result<Self> {
        validate_catalog_path(raw, false, 33)?;
        Ok(Self(raw.to_string()))
    }

    fn as_str(&self) -> &str {
        &self.0
    }

    fn parent(&self) -> &str {
        self.0
            .rsplit_once('/')
            .map(|(parent, _)| parent)
            .unwrap_or("")
    }
}

struct ArtifactCatalogDirectoryPath(String);

impl ArtifactCatalogDirectoryPath {
    fn parse(raw: &str) -> Result<Self> {
        validate_catalog_path(raw, true, 32)?;
        Ok(Self(raw.to_string()))
    }

    fn as_str(&self) -> &str {
        &self.0
    }
}

fn validate_catalog_path(raw: &str, allow_empty: bool, max_segments: usize) -> Result<()> {
    if (!allow_empty && raw.is_empty())
        || raw.len() > 2_048
        || raw.starts_with('/')
        || raw.ends_with('/')
    {
        bail!("artifact path must be a non-empty relative file path");
    }
    if raw.is_empty() {
        return Ok(());
    }
    let segments = raw.split('/').collect::<Vec<_>>();
    if segments.len() > max_segments {
        bail!("artifact path exceeds the maximum folder depth");
    }
    if segments.iter().any(|segment| {
        segment.is_empty()
            || *segment == "."
            || *segment == ".."
            || segment.len() > 255
            || segment.contains('\\')
            || segment.chars().any(char::is_control)
            || segment.trim() != *segment
    }) {
        bail!("artifact path contains an invalid segment");
    }
    Ok(())
}

async fn current_org_id<E>(backend: &PlatformArtifactToolsBackend<E>) -> Result<Uuid> {
    match backend.cached_org_id {
        Some(org_id) => Ok(org_id),
        None => backend.client.current_org_id().await,
    }
}

fn is_textual_media_type(media_type: &str) -> bool {
    nenjo_content::is_utf8_text_media_type(media_type)
}

fn render_artifact_lines(contents: &str, start_line: usize, line_count: usize) -> Result<String> {
    let total_lines = contents.lines().count();
    if !contents.is_empty() && start_line > total_lines {
        bail!("start_line {start_line} is past the end of the artifact ({total_lines} lines)");
    }
    let mut output: String = contents
        .split_inclusive('\n')
        .skip(start_line - 1)
        .take(line_count)
        .collect();
    let last_selected_line = start_line
        .saturating_add(line_count)
        .saturating_sub(1)
        .min(total_lines);
    let has_more_lines = last_selected_line < total_lines;
    let mut byte_truncated = false;
    if output.len() > MAX_OUTPUT_BYTES {
        output.truncate(output.floor_char_boundary(MAX_OUTPUT_BYTES));
        byte_truncated = true;
    }
    if has_more_lines || byte_truncated {
        if !output.ends_with('\n') {
            output.push('\n');
        }
        if byte_truncated {
            output.push_str(&format!(
                "... [output bounded at {MAX_OUTPUT_BYTES} bytes; request a narrower line range]"
            ));
        } else {
            output.push_str(&format!(
                "... [artifact has {total_lines} lines; continue with start_line={}]",
                last_selected_line + 1
            ));
        }
    }
    Ok(output)
}

struct UploadArtifactTool<E> {
    backend: PlatformArtifactToolsBackend<E>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct UploadArtifactArgs {
    source_path: String,
    target: ArtifactUploadTarget,
    #[serde(default)]
    media_type: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum ArtifactUploadTarget {
    Create { publish_path: String },
    Revision { revision_of: Uuid },
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
        "Publish an immutable encrypted snapshot of a workspace file up to 16 MiB. New artifacts require a catalog path. Revisions require the current immutable artifact ID and retain the logical artifact's existing catalog path; stale revision IDs are rejected."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "source_path": {"type": "string", "minLength": 1, "description": "Workspace-relative regular file to snapshot."},
                "target": {
                    "oneOf": [
                        {
                            "type": "object",
                            "additionalProperties": false,
                            "properties": {
                                "kind": {"const": "create"},
                                "publish_path": {"type": "string", "minLength": 1, "description": "Organization-relative catalog path for the new logical artifact."}
                            },
                            "required": ["kind", "publish_path"]
                        },
                        {
                            "type": "object",
                            "additionalProperties": false,
                            "properties": {
                                "kind": {"const": "revision"},
                                "revision_of": {"type": "string", "format": "uuid", "description": "Current immutable artifact ID to revise."}
                            },
                            "required": ["kind", "revision_of"]
                        }
                    ]
                },
                "media_type": {"type": "string", "minLength": 3, "description": "Optional plaintext media type; inferred from the extension when omitted."}
            },
            "required": ["source_path", "target"]
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
        let (revision_of, publish_path, publish_name) = match args.target {
            ArtifactUploadTarget::Create { publish_path } => {
                let path = ArtifactCatalogFilePath::parse(&publish_path)?;
                let name = path
                    .as_str()
                    .rsplit_once('/')
                    .map(|(_, name)| name)
                    .unwrap_or(path.as_str())
                    .to_string();
                (None, Some(publish_path), name)
            }
            ArtifactUploadTarget::Revision { revision_of } => {
                (Some(revision_of), None, file.file_name.clone())
            }
        };
        let catalog_path = publish_path.clone();
        let grant = self
            .backend
            .client
            .create_artifact_upload(&CreateArtifactUploadRequest {
                artifact_id,
                revision_of,
                name: publish_name,
                media_type,
                plaintext_size_bytes: u64::try_from(file.bytes.len()).unwrap_or(u64::MAX),
                ciphertext_size_bytes: u64::try_from(ciphertext.len()).unwrap_or(u64::MAX),
                plaintext_digest,
                ciphertext_digest: digest(&ciphertext),
                publish_path,
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
        let metadata = ArtifactMetadata::parse(&artifact)?;
        if let Err(error) = self
            .backend
            .materializer
            .seed(org_id, &metadata, &file.bytes)
            .await
        {
            tracing::warn!(
                artifact_id = %artifact.id,
                error = %error,
                "Artifact upload completed but its local plaintext cache could not be seeded"
            );
        }
        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&json!({
                "artifact_id": artifact.id,
                "lineage_id": artifact.lineage_id,
                "revision_number": artifact.revision_number,
                "name": artifact.name,
                "media_type": artifact.media_type,
                "size_bytes": artifact.plaintext_size_bytes,
                "digest": artifact.plaintext_digest,
                "catalog_path": catalog_path
            }))?
            .into(),
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
        Some("jsonld") => "application/ld+json",
        Some("md") => "text/markdown",
        Some("txt") => "text/plain",
        Some("csv") => "text/csv",
        Some("html" | "htm") => "text/html",
        Some("xml") => "application/xml",
        Some("yaml" | "yml") => "application/yaml",
        Some("toml") => "application/toml",
        Some("js" | "mjs" | "cjs") => "application/javascript",
        Some("svg") => "image/svg+xml",
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

    fn artifact_entry(path: &str) -> ArtifactEntryRecord {
        ArtifactEntryRecord {
            entry_id: Uuid::new_v4(),
            artifact_id: Uuid::new_v4(),
            lineage_id: Uuid::new_v4(),
            revision_number: 1,
            revision_count: 1,
            name: path.rsplit('/').next().unwrap().to_string(),
            path: path.to_string(),
            media_type: "text/plain".to_string(),
            size_bytes: 4,
            digest: digest(b"test"),
            created_at: "2026-08-11T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn workspace_paths_reject_parent_traversal() {
        let root = tempfile::tempdir().unwrap();
        assert!(read_workspace_file(root.path(), "../secret.pdf").is_err());
    }

    #[test]
    fn media_type_inference_is_bounded_and_predictable() {
        assert_eq!(infer_media_type("report.PDF"), "application/pdf");
        assert_eq!(infer_media_type("payload.jsonld"), "application/ld+json");
        assert_eq!(infer_media_type("page.HTM"), "text/html");
        assert_eq!(infer_media_type("config.yaml"), "application/yaml");
        assert_eq!(infer_media_type("diagram.svg"), "image/svg+xml");
        assert_eq!(infer_media_type("archive.bin"), "application/octet-stream");
    }

    #[test]
    fn upload_target_parses_explicit_revision_identity() {
        let revision_of = Uuid::new_v4();
        let args: UploadArtifactArgs = serde_json::from_value(json!({
            "source_path": "report.pdf",
            "target": {
                "kind": "revision",
                "revision_of": revision_of
            }
        }))
        .unwrap();
        assert!(matches!(
            args.target,
            ArtifactUploadTarget::Revision {
                revision_of: parsed
            } if parsed == revision_of
        ));
    }

    #[test]
    fn upload_target_requires_a_catalog_path_for_creation() {
        let missing_path = serde_json::from_value::<UploadArtifactArgs>(json!({
            "source_path": "report.pdf",
            "target": { "kind": "create" }
        }));
        assert!(missing_path.is_err());
    }

    #[test]
    fn artifact_listing_keeps_only_immediate_children() {
        let catalog = ArtifactCatalogRecord {
            path: "Reports".to_string(),
            folders: vec![
                ArtifactFolderRecord {
                    folder_id: Uuid::new_v4(),
                    name: "2026".to_string(),
                    path: "Reports/2026".to_string(),
                },
                ArtifactFolderRecord {
                    folder_id: Uuid::new_v4(),
                    name: "August".to_string(),
                    path: "Reports/2026/August".to_string(),
                },
            ],
            artifacts: vec![
                artifact_entry("Reports/summary.md"),
                artifact_entry("Reports/2026/detail.md"),
            ],
            unfiled: Vec::new(),
        };

        let listing = direct_children(catalog);

        assert_eq!(listing.folders.len(), 1);
        assert_eq!(listing.folders[0].path, "Reports/2026");
        assert_eq!(listing.artifacts.len(), 1);
        assert_eq!(listing.artifacts[0].path, "Reports/summary.md");
    }

    #[test]
    fn artifact_path_is_parsed_once_and_exposes_its_parent() {
        let path = ArtifactCatalogFilePath::parse("Reports/2026/summary.md").unwrap();
        assert_eq!(path.parent(), "Reports/2026");
        assert!(ArtifactCatalogFilePath::parse("Reports/../secret.md").is_err());
        assert!(ArtifactCatalogFilePath::parse("/absolute.md").is_err());
    }

    #[test]
    fn artifact_directory_path_allows_only_a_relative_directory_or_root() {
        assert_eq!(
            ArtifactCatalogDirectoryPath::parse("").unwrap().as_str(),
            ""
        );
        assert_eq!(
            ArtifactCatalogDirectoryPath::parse("Reports/2026")
                .unwrap()
                .as_str(),
            "Reports/2026"
        );
        assert!(ArtifactCatalogDirectoryPath::parse("Reports/../Secret").is_err());
        assert!(ArtifactCatalogDirectoryPath::parse("/Reports").is_err());
        assert!(ArtifactCatalogDirectoryPath::parse("Reports/").is_err());
    }

    #[test]
    fn artifact_line_ranges_report_how_to_continue() {
        let rendered = render_artifact_lines("one\ntwo\nthree\n", 1, 2).unwrap();
        assert!(rendered.starts_with("one\ntwo\n"));
        assert!(rendered.contains("continue with start_line=3"));
    }

    #[test]
    fn automatic_view_keeps_text_as_a_bounded_read() {
        assert_eq!(
            ArtifactReadPlan::resolve(ArtifactReadView::Automatic, "text/markdown").unwrap(),
            ArtifactReadPlan::Text
        );
        assert_eq!(
            ArtifactReadPlan::resolve(ArtifactReadView::Automatic, "application/json").unwrap(),
            ArtifactReadPlan::Text
        );
    }

    #[test]
    fn automatic_view_routes_media_to_model_input() {
        for media_type in ["image/png", "application/pdf", "audio/mpeg", "video/mp4"] {
            assert_eq!(
                ArtifactReadPlan::resolve(ArtifactReadView::Automatic, media_type).unwrap(),
                ArtifactReadPlan::ModelInput,
                "unexpected plan for {media_type}"
            );
        }
    }

    #[test]
    fn text_view_rejects_non_textual_media() {
        let error = ArtifactReadPlan::resolve(ArtifactReadView::Text, "image/png").unwrap_err();
        assert!(error.to_string().contains("view='automatic'"));
    }

    #[test]
    fn read_view_defaults_to_automatic_and_rejects_unknown_values() {
        let args: ReadArtifactArgs = serde_json::from_value(json!({
            "selector": {"kind": "artifact_id", "artifact_id": Uuid::new_v4()}
        }))
        .unwrap();
        assert_eq!(args.view, ArtifactReadView::Automatic);

        let invalid = serde_json::from_value::<ReadArtifactArgs>(json!({
            "selector": {"kind": "artifact_id", "artifact_id": Uuid::new_v4()},
            "view": "raw"
        }));
        assert!(invalid.is_err());
    }

    #[test]
    fn model_input_output_contains_only_metadata_and_an_immutable_reference() {
        let artifact_id = Uuid::new_v4();
        let record = ArtifactRecord {
            id: artifact_id,
            org_id: Uuid::new_v4(),
            state: "ready".to_string(),
            lineage_id: Uuid::new_v4(),
            revision_number: 2,
            previous_artifact_id: Some(Uuid::new_v4()),
            name: "diagram.png".to_string(),
            media_type: "image/png".to_string(),
            plaintext_size_bytes: 4,
            ciphertext_size_bytes: 8,
            plaintext_digest: digest(b"test"),
            ciphertext_digest: digest(b"envelope"),
            created_at: "2026-08-13T00:00:00Z".to_string(),
            ready_at: Some("2026-08-13T00:00:01Z".to_string()),
        };
        let metadata = ArtifactMetadata::parse(&record).unwrap();

        let result = model_input_result(&metadata, Some("Design/diagram.png"));

        assert!(result.success);
        assert!(result.output.has_artifacts());
        assert_eq!(result.output.parts().len(), 2);
        assert!(matches!(
            &result.output.parts()[1],
            nenjo_models::ToolOutputPart::Artifact(reference) if reference == metadata.reference()
        ));
        let text = result.output.text_content();
        assert!(text.contains(&artifact_id.to_string()));
        assert!(text.contains("media_type: image/png"));
        assert!(!text.contains("base64"));
        assert!(!text.contains("artifact-cache"));
    }
}
