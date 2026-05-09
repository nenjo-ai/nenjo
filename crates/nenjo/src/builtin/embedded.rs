use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use super::generated::BUILTIN_DOCS;
use super::types::*;

pub const BUILTIN_KNOWLEDGE_DISCOVERY: &str = "Nenjo knowledge is available as the built-in Nenjo knowledge pack. Use list_knowledge_tree, search_knowledge_paths, read_knowledge_doc_manifest, list_knowledge_neighbors, and read_knowledge_doc with pack=\"builtin:nenjo\" when platform concepts or built-in patterns are relevant.";

const MANIFEST_YAML: &str = include_str!("../../knowledge/manifest.yaml");

static PACK: OnceLock<BuiltinKnowledgePack> = OnceLock::new();

#[derive(Debug, Deserialize)]
struct BuiltinKnowledgeManifestFile {
    pack_id: String,
    schema_version: u32,
    root_uri: String,
    docs: Vec<BuiltinDocManifest>,
}

pub fn builtin_knowledge_pack() -> &'static BuiltinKnowledgePack {
    PACK.get_or_init(|| {
        let manifest_file: BuiltinKnowledgeManifestFile = serde_yaml::from_str(MANIFEST_YAML)
            .expect("embedded builtin knowledge manifest is invalid");
        BuiltinKnowledgePack {
            manifest: BuiltinKnowledgeManifest {
                pack_id: manifest_file.pack_id,
                pack_version: env!("CARGO_PKG_VERSION").to_string(),
                schema_version: manifest_file.schema_version,
                root_uri: manifest_file.root_uri,
                content_hash: content_hash(&manifest_file.docs),
                docs: manifest_file.docs,
            },
            docs: BUILTIN_DOCS,
        }
    })
}

pub fn builtin_knowledge_summary() -> String {
    let pack = builtin_knowledge_pack();
    let ctx = BuiltinKnowledgeSummaryContext {
        source: "builtin",
        name: "nenjo",
        root: "builtin://nenjo/",
        usage: BUILTIN_KNOWLEDGE_DISCOVERY,
        docs: pack
            .manifest
            .docs
            .iter()
            .map(|doc| BuiltinDocumentSummaryContext {
                path: doc.virtual_path.as_str(),
                id: doc.id.as_str(),
                kind: doc.kind.as_str(),
                title: doc.title.as_str(),
                summary: doc.summary.as_str(),
            })
            .collect(),
    };

    nenjo_xml::to_xml_pretty(&ctx, 2)
}

#[derive(Debug, Serialize)]
#[serde(rename = "knowledge_pack")]
struct BuiltinKnowledgeSummaryContext<'a> {
    #[serde(rename = "@source")]
    source: &'a str,
    #[serde(rename = "@name")]
    name: &'a str,
    #[serde(rename = "@root")]
    root: &'a str,
    usage: &'a str,
    #[serde(rename = "doc")]
    docs: Vec<BuiltinDocumentSummaryContext<'a>>,
}

#[derive(Debug, Serialize)]
#[serde(rename = "doc")]
struct BuiltinDocumentSummaryContext<'a> {
    #[serde(rename = "@path")]
    path: &'a str,
    #[serde(rename = "@id")]
    id: &'a str,
    #[serde(rename = "@kind")]
    kind: &'a str,
    title: &'a str,
    summary: &'a str,
}

fn content_hash(manifest_docs: &[BuiltinDocManifest]) -> String {
    let mut hash = 0xcbf29ce484222325_u64;
    for manifest in manifest_docs {
        fnv1a(&mut hash, manifest.id.as_bytes());
        fnv1a(&mut hash, manifest.virtual_path.as_bytes());
        if let Some(doc) = BUILTIN_DOCS.iter().find(|doc| doc.id == manifest.id) {
            fnv1a(&mut hash, doc.content.as_bytes());
        }
    }
    format!("{hash:016x}")
}

fn fnv1a(hash: &mut u64, bytes: &[u8]) {
    for byte in bytes {
        *hash ^= u64::from(*byte);
        *hash = hash.wrapping_mul(0x100000001b3);
    }
}
