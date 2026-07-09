//! Parsing for base64-encoded audio data URIs.
//!
//! Providers own their supported MIME types and upload filenames. This module
//! only parses the common data-URI envelope and decodes its payload.

use anyhow::Context;
use base64::{Engine as _, engine::general_purpose};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DecodedDataUri {
    pub(crate) mime_type: String,
    pub(crate) bytes: Vec<u8>,
}

pub(crate) fn decode_base64_data_uri(data_uri: &str) -> anyhow::Result<DecodedDataUri> {
    let (metadata, encoded) = data_uri
        .split_once(',')
        .ok_or_else(|| anyhow::anyhow!("audio data URI must contain metadata and base64 data"))?;
    let metadata = metadata
        .strip_prefix("data:")
        .ok_or_else(|| anyhow::anyhow!("audio input must be a data URI"))?;
    let mut metadata_parts = metadata.split(';');
    let mime_type = metadata_parts
        .next()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("application/octet-stream")
        .to_ascii_lowercase();
    if !metadata_parts.any(|part| part.eq_ignore_ascii_case("base64")) {
        anyhow::bail!("audio data URI must be base64 encoded");
    }

    let bytes = general_purpose::STANDARD
        .decode(encoded.trim())
        .or_else(|_| general_purpose::STANDARD_NO_PAD.decode(encoded.trim()))
        .or_else(|_| general_purpose::URL_SAFE.decode(encoded.trim()))
        .or_else(|_| general_purpose::URL_SAFE_NO_PAD.decode(encoded.trim()))
        .context("invalid base64 audio data URI")?;
    if bytes.is_empty() {
        anyhow::bail!("audio data URI cannot be empty");
    }

    Ok(DecodedDataUri { mime_type, bytes })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_base64_data_uri_and_normalizes_mime_type() {
        let decoded = decode_base64_data_uri("data:Audio/OGG;BASE64,YXVkaW8=").unwrap();

        assert_eq!(decoded.mime_type, "audio/ogg");
        assert_eq!(decoded.bytes, b"audio");
    }

    #[test]
    fn rejects_non_base64_data_uri() {
        let error = decode_base64_data_uri("data:audio/ogg,audio").unwrap_err();

        assert!(error.to_string().contains("must be base64 encoded"));
    }
}
