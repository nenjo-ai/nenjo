//! Storage-neutral content contracts for Nenjo.
//!
//! Artifact references identify immutable plaintext content without exposing
//! platform authorization context, object-store locations, worker cache paths,
//! or decrypted bytes. Raw wire values are parsed into these types once at a
//! boundary so downstream code can rely on their invariants.

use std::fmt;
use std::str::FromStr;

use mime::Mime;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use uuid::Uuid;

const SHA256_HEX_LENGTH: usize = 64;
const MAX_ARTIFACT_INSTRUCTION_CHARS: usize = 16_384;

/// An invalid storage-neutral content value.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ContentValueError {
    /// Artifact identities must refer to a real immutable object.
    #[error("artifact id cannot be nil")]
    NilArtifactId,
    /// Artifact digests use one canonical algorithm-qualified representation.
    #[error("artifact digest must use sha256:<64 lowercase or uppercase hexadecimal characters>")]
    InvalidSha256Digest,
    /// Artifact media types must be concrete, syntactically valid MIME values.
    #[error("invalid artifact media type '{value}'")]
    InvalidMediaType { value: String },
    /// Optional artifact-specific instructions cannot be empty when present.
    #[error("artifact instruction cannot be empty")]
    EmptyArtifactInstruction,
    /// Artifact instructions are bounded before entering a model context.
    #[error("artifact instruction exceeds {max_chars} characters")]
    ArtifactInstructionTooLong { max_chars: usize },
}

/// Immutable artifact identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ArtifactId(Uuid);

impl ArtifactId {
    /// Parse an artifact identity, rejecting the nil UUID sentinel.
    pub fn parse(value: Uuid) -> Result<Self, ContentValueError> {
        if value.is_nil() {
            return Err(ContentValueError::NilArtifactId);
        }
        Ok(Self(value))
    }

    /// Return the UUID representation used by existing platform APIs.
    pub const fn as_uuid(self) -> Uuid {
        self.0
    }
}

impl fmt::Display for ArtifactId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl TryFrom<Uuid> for ArtifactId {
    type Error = ContentValueError;

    fn try_from(value: Uuid) -> Result<Self, Self::Error> {
        Self::parse(value)
    }
}

impl From<ArtifactId> for Uuid {
    fn from(value: ArtifactId) -> Self {
        value.as_uuid()
    }
}

impl Serialize for ArtifactId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.0.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ArtifactId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = Uuid::deserialize(deserializer)?;
        Self::parse(value).map_err(serde::de::Error::custom)
    }
}

/// Canonical SHA-256 digest in `sha256:<hex>` form.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Sha256Digest(String);

impl Sha256Digest {
    /// Parse and normalize an algorithm-qualified SHA-256 digest.
    pub fn parse(value: &str) -> Result<Self, ContentValueError> {
        let Some(hex) = value.strip_prefix("sha256:") else {
            return Err(ContentValueError::InvalidSha256Digest);
        };
        if hex.len() != SHA256_HEX_LENGTH || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(ContentValueError::InvalidSha256Digest);
        }
        Ok(Self(format!("sha256:{}", hex.to_ascii_lowercase())))
    }

    /// Return the canonical algorithm-qualified value.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Return the lowercase digest component for safe cache-key construction.
    pub fn hex(&self) -> &str {
        self.0
            .strip_prefix("sha256:")
            .expect("validated digest always has the sha256 prefix")
    }
}

impl fmt::Display for Sha256Digest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for Sha256Digest {
    type Err = ContentValueError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

impl Serialize for Sha256Digest {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for Sha256Digest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).map_err(serde::de::Error::custom)
    }
}

/// Parsed, concrete MIME type for immutable artifact plaintext.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MediaType(Mime);

impl MediaType {
    /// Parse a MIME value and reject wildcard media ranges.
    pub fn parse(value: &str) -> Result<Self, ContentValueError> {
        let trimmed = value.trim();
        let parsed = trimmed
            .parse::<Mime>()
            .map_err(|_| ContentValueError::InvalidMediaType {
                value: value.to_owned(),
            })?;
        if parsed.type_() == mime::STAR || parsed.subtype() == mime::STAR {
            return Err(ContentValueError::InvalidMediaType {
                value: value.to_owned(),
            });
        }
        Ok(Self(parsed))
    }

    /// Return the parsed MIME value.
    pub fn as_mime(&self) -> &Mime {
        &self.0
    }

    /// Return the MIME type and subtype without parameters.
    pub fn essence_str(&self) -> &str {
        self.0.essence_str()
    }

    /// Return whether this media type conventionally carries UTF-8 model-facing text.
    pub fn is_utf8_text(&self) -> bool {
        is_utf8_text_media_type(self.essence_str())
    }

    /// Return whether this media type identifies a PDF document.
    pub fn is_pdf(&self) -> bool {
        self.essence_str() == "application/pdf"
    }
}

/// Classify storage-neutral media types whose plaintext bytes may be decoded as UTF-8.
///
/// Actual bytes are still validated before use. This function classifies the declared
/// representation; it does not trust the declaration as proof of valid text.
pub fn is_utf8_text_media_type(media_type: &str) -> bool {
    let essence = media_type
        .split(';')
        .next()
        .map(str::trim)
        .unwrap_or_default();
    essence.starts_with("text/")
        || matches!(
            essence,
            "application/json"
                | "application/ld+json"
                | "application/csv"
                | "application/x-csv"
                | "application/xml"
                | "application/javascript"
                | "application/ecmascript"
                | "application/yaml"
                | "application/x-yaml"
                | "application/toml"
                | "image/svg+xml"
        )
        || (essence.starts_with("application/")
            && (essence.ends_with("+json") || essence.ends_with("+xml")))
}

impl fmt::Display for MediaType {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl FromStr for MediaType {
    type Err = ContentValueError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

impl Serialize for MediaType {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for MediaType {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).map_err(serde::de::Error::custom)
    }
}

/// Authoritative plaintext byte length of an immutable artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ArtifactSize(u64);

impl ArtifactSize {
    /// Construct an artifact plaintext size. Empty artifacts are valid.
    pub const fn new(bytes: u64) -> Self {
        Self(bytes)
    }

    /// Return the plaintext byte count.
    pub const fn bytes(self) -> u64 {
        self.0
    }
}

impl From<u64> for ArtifactSize {
    fn from(value: u64) -> Self {
        Self::new(value)
    }
}

/// Storage-neutral reference to one immutable artifact revision.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ArtifactRef {
    id: ArtifactId,
    digest: Sha256Digest,
    media_type: MediaType,
    size: ArtifactSize,
}

impl ArtifactRef {
    /// Construct a reference from already parsed values.
    pub const fn new(
        id: ArtifactId,
        digest: Sha256Digest,
        media_type: MediaType,
        size: ArtifactSize,
    ) -> Self {
        Self {
            id,
            digest,
            media_type,
            size,
        }
    }

    /// Return the immutable artifact identity.
    pub const fn id(&self) -> ArtifactId {
        self.id
    }

    /// Return the authoritative plaintext digest.
    pub fn digest(&self) -> &Sha256Digest {
        &self.digest
    }

    /// Return the authoritative plaintext media type.
    pub fn media_type(&self) -> &MediaType {
        &self.media_type
    }

    /// Return the authoritative plaintext length.
    pub const fn size(&self) -> ArtifactSize {
        self.size
    }
}

/// Origin of an artifact in an agent input.
///
/// This provenance is durable and storage neutral. It does not grant access;
/// authorization remains bound to the authenticated runtime context.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactInputSource {
    UserAttachment,
    TaskInput,
    ToolResult,
    SessionContext,
}

/// Optional, bounded instruction associated with an artifact input.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ArtifactInstruction(String);

impl ArtifactInstruction {
    /// Parse a non-empty instruction suitable for model context.
    pub fn parse(value: &str) -> Result<Self, ContentValueError> {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return Err(ContentValueError::EmptyArtifactInstruction);
        }
        if trimmed.chars().count() > MAX_ARTIFACT_INSTRUCTION_CHARS {
            return Err(ContentValueError::ArtifactInstructionTooLong {
                max_chars: MAX_ARTIFACT_INSTRUCTION_CHARS,
            });
        }
        Ok(Self(trimmed.to_owned()))
    }

    /// Return the normalized instruction.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ArtifactInstruction {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for ArtifactInstruction {
    type Err = ContentValueError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

impl Serialize for ArtifactInstruction {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ArtifactInstruction {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).map_err(serde::de::Error::custom)
    }
}

/// Durable, unresolved artifact input carried through tools and conversations.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ArtifactInput {
    artifact: ArtifactRef,
    source: ArtifactInputSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    instruction: Option<ArtifactInstruction>,
}

impl ArtifactInput {
    /// Construct an artifact input from validated values.
    pub const fn new(artifact: ArtifactRef, source: ArtifactInputSource) -> Self {
        Self {
            artifact,
            source,
            instruction: None,
        }
    }

    /// Attach a validated model-facing instruction.
    pub fn with_instruction(mut self, instruction: ArtifactInstruction) -> Self {
        self.instruction = Some(instruction);
        self
    }

    pub fn artifact(&self) -> &ArtifactRef {
        &self.artifact
    }

    pub const fn source(&self) -> ArtifactInputSource {
        self.source
    }

    pub fn instruction(&self) -> Option<&ArtifactInstruction> {
        self.instruction.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_id() -> Uuid {
        Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").expect("valid fixture")
    }

    #[test]
    fn artifact_id_rejects_nil_at_construction_and_deserialization() {
        assert_eq!(
            ArtifactId::parse(Uuid::nil()),
            Err(ContentValueError::NilArtifactId)
        );
        assert!(serde_json::from_str::<ArtifactId>(&format!("\"{}\"", Uuid::nil())).is_err());
    }

    #[test]
    fn digest_normalizes_hex_and_rejects_invalid_values() {
        let digest =
            Sha256Digest::parse(&format!("sha256:{}", "A".repeat(64))).expect("valid digest");
        assert_eq!(digest.hex(), "a".repeat(64));
        assert_eq!(digest.as_str(), format!("sha256:{}", "a".repeat(64)));
        assert!(Sha256Digest::parse(&"a".repeat(64)).is_err());
        assert!(Sha256Digest::parse(&format!("sha256:{}", "z".repeat(64))).is_err());
    }

    #[test]
    fn media_type_is_concrete_and_normalized() {
        let media_type = MediaType::parse(" image/png ").expect("valid MIME type");
        assert_eq!(media_type.essence_str(), "image/png");
        assert!(MediaType::parse("image/*").is_err());
        assert!(MediaType::parse("not a mime type").is_err());
    }

    #[test]
    fn utf8_text_media_types_cover_structured_documents() {
        for media_type in [
            "text/plain",
            "text/markdown; charset=utf-8",
            "application/json",
            "application/problem+json",
            "application/csv",
            "application/x-csv",
            "application/xml",
            "application/atom+xml",
            "application/yaml",
            "application/toml",
            "application/javascript",
            "image/svg+xml",
        ] {
            assert!(is_utf8_text_media_type(media_type), "{media_type}");
        }
        assert!(!is_utf8_text_media_type("application/pdf"));
        assert!(!is_utf8_text_media_type("application/vnd.ms-excel"));
        assert!(!is_utf8_text_media_type("application/octet-stream"));
    }

    #[test]
    fn artifact_reference_round_trip_preserves_validated_values() {
        let reference = ArtifactRef::new(
            ArtifactId::parse(valid_id()).expect("valid artifact id"),
            Sha256Digest::parse(&format!("sha256:{}", "b".repeat(64))).expect("valid digest"),
            MediaType::parse("image/png").expect("valid MIME type"),
            ArtifactSize::new(42),
        );

        let encoded = serde_json::to_value(&reference).expect("serialize reference");
        let decoded: ArtifactRef =
            serde_json::from_value(encoded.clone()).expect("deserialize reference");

        assert_eq!(decoded, reference);
        assert_eq!(encoded["id"], valid_id().to_string());
        assert_eq!(encoded["size"], 42);
    }

    #[test]
    fn artifact_input_preserves_provenance_without_authority_fields() {
        let reference = ArtifactRef::new(
            ArtifactId::parse(valid_id()).expect("valid artifact id"),
            Sha256Digest::parse(&format!("sha256:{}", "c".repeat(64))).expect("valid digest"),
            MediaType::parse("image/png").expect("valid MIME type"),
            ArtifactSize::new(42),
        );
        let input = ArtifactInput::new(reference, ArtifactInputSource::ToolResult)
            .with_instruction(ArtifactInstruction::parse(" inspect the error ").unwrap());

        let encoded = serde_json::to_value(&input).expect("serialize artifact input");
        let decoded: ArtifactInput = serde_json::from_value(encoded.clone()).unwrap();

        assert_eq!(decoded, input);
        assert_eq!(encoded["source"], "tool_result");
        assert_eq!(encoded["instruction"], "inspect the error");
        assert!(encoded.get("org_id").is_none());
        assert!(encoded.get("path").is_none());
        assert!(encoded.get("bytes").is_none());
    }

    #[test]
    fn artifact_instruction_rejects_empty_and_unbounded_values() {
        assert_eq!(
            ArtifactInstruction::parse("  "),
            Err(ContentValueError::EmptyArtifactInstruction)
        );
        assert!(
            ArtifactInstruction::parse(&"x".repeat(MAX_ARTIFACT_INSTRUCTION_CHARS + 1)).is_err()
        );
    }
}
