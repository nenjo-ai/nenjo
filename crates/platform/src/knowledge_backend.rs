use crate::library_knowledge::LibraryKnowledgePack;
use anyhow::{Result, anyhow, bail};
use nenjo_knowledge::{
    KnowledgeDocEdgeType, KnowledgeDocFilter, KnowledgeDocManifest, KnowledgeDocNeighbor,
    KnowledgeDocSearchHit, KnowledgePack, KnowledgePackManifest,
};

#[derive(Clone)]
pub(crate) enum ResolvedKnowledgePack {
    Library(LibraryKnowledgePack),
}

impl std::fmt::Debug for ResolvedKnowledgePack {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Library(_) => f.write_str("ResolvedKnowledgePack::Library"),
        }
    }
}

impl KnowledgePack for ResolvedKnowledgePack {
    fn manifest(&self) -> &dyn KnowledgePackManifest {
        match self {
            Self::Library(pack) => pack.manifest(),
        }
    }

    fn doc_content(&self, manifest: &KnowledgeDocManifest) -> Option<std::borrow::Cow<'_, str>> {
        match self {
            Self::Library(pack) => pack.doc_content(manifest),
        }
    }

    fn read_manifest(&self, path: &str) -> Option<&KnowledgeDocManifest> {
        match self {
            Self::Library(pack) => pack.read_manifest(path),
        }
    }

    fn list_docs(&self, filter: KnowledgeDocFilter) -> Vec<&KnowledgeDocManifest> {
        match self {
            Self::Library(pack) => pack.list_docs(filter),
        }
    }

    fn search(&self, query: &str, filter: KnowledgeDocFilter) -> Vec<KnowledgeDocSearchHit> {
        match self {
            Self::Library(pack) => pack.search(query, filter),
        }
    }

    fn neighbors(
        &self,
        path: &str,
        edge_type: Option<KnowledgeDocEdgeType>,
    ) -> Option<KnowledgeDocNeighbor> {
        match self {
            Self::Library(pack) => pack.neighbors(path, edge_type),
        }
    }
}

pub(crate) fn library_pack_selector(pack_slug: &str) -> String {
    format!("lib:{pack_slug}")
}

pub(crate) fn parse_library_pack_selector(selector: &str) -> Result<&str> {
    let slug = selector
        .strip_prefix("lib:")
        .ok_or_else(|| anyhow!("library knowledge packs must use lib:<slug>"))?;
    if slug.is_empty() {
        bail!("library knowledge pack selector must include a slug")
    }
    Ok(slug)
}

pub(crate) fn unknown_pack(selector: &str) -> anyhow::Error {
    anyhow!(
        "unknown knowledge pack '{selector}'; use 'lib:<pack>', 'pkg:<package>', or 'local:<pack>'"
    )
}

pub(crate) fn ensure_known_pack_selector(selector: &str) -> Result<()> {
    if selector.starts_with("lib:")
        || selector.starts_with("pkg:")
        || selector.starts_with("local:")
    {
        Ok(())
    } else {
        bail!("unknown knowledge pack selector '{selector}'")
    }
}
