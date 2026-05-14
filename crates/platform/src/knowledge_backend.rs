use crate::library_knowledge::LibraryKnowledgePack;
use anyhow::{Result, anyhow, bail};
use nenjo_knowledge::builtin::{BuiltinKnowledgePack, nenjo_pack};
use nenjo_knowledge::{
    KnowledgeDocEdgeType, KnowledgeDocFilter, KnowledgeDocManifest, KnowledgeDocNeighbor,
    KnowledgeDocSearchHit, KnowledgeDocTree, KnowledgePack, KnowledgePackManifest,
};

#[derive(Debug, Clone)]
pub(crate) enum ResolvedKnowledgePack {
    Builtin(BuiltinKnowledgePack),
    Library(LibraryKnowledgePack),
}

impl KnowledgePack for ResolvedKnowledgePack {
    fn manifest(&self) -> &dyn KnowledgePackManifest {
        match self {
            Self::Builtin(pack) => pack.manifest(),
            Self::Library(pack) => pack.manifest(),
        }
    }

    fn doc_content(&self, manifest: &KnowledgeDocManifest) -> Option<std::borrow::Cow<'_, str>> {
        match self {
            Self::Builtin(pack) => pack.doc_content(manifest),
            Self::Library(pack) => pack.doc_content(manifest),
        }
    }

    fn read_manifest(&self, path: &str) -> Option<&KnowledgeDocManifest> {
        match self {
            Self::Builtin(pack) => pack.read_manifest(path),
            Self::Library(pack) => pack.read_manifest(path),
        }
    }

    fn list_docs(&self, filter: KnowledgeDocFilter) -> Vec<&KnowledgeDocManifest> {
        match self {
            Self::Builtin(pack) => pack.list_docs(filter),
            Self::Library(pack) => pack.list_docs(filter),
        }
    }

    fn list_tree(&self, prefix: Option<&str>) -> KnowledgeDocTree {
        match self {
            Self::Builtin(pack) => pack.list_tree(prefix),
            Self::Library(pack) => pack.list_tree(prefix),
        }
    }

    fn search_paths(&self, query: &str, filter: KnowledgeDocFilter) -> Vec<KnowledgeDocSearchHit> {
        match self {
            Self::Builtin(pack) => pack.search_paths(query, filter),
            Self::Library(pack) => pack.search_paths(query, filter),
        }
    }

    fn search_docs(&self, query: &str, filter: KnowledgeDocFilter) -> Vec<KnowledgeDocSearchHit> {
        match self {
            Self::Builtin(pack) => pack.search_docs(query, filter),
            Self::Library(pack) => pack.search_docs(query, filter),
        }
    }

    fn neighbors(
        &self,
        path: &str,
        edge_type: Option<KnowledgeDocEdgeType>,
    ) -> Vec<KnowledgeDocNeighbor> {
        match self {
            Self::Builtin(pack) => pack.neighbors(path, edge_type),
            Self::Library(pack) => pack.neighbors(path, edge_type),
        }
    }
}

pub(crate) fn builtin_pack() -> ResolvedKnowledgePack {
    ResolvedKnowledgePack::Builtin(nenjo_pack().clone())
}

pub(crate) fn library_pack_selector(pack_slug: &str) -> String {
    format!("workspace:{pack_slug}")
}

pub(crate) fn is_default_library_pack_selector(selector: &str) -> bool {
    selector == "workspace"
}

pub(crate) fn parse_library_pack_selector(selector: &str) -> Result<&str> {
    let slug = selector
        .strip_prefix("workspace:")
        .ok_or_else(|| anyhow!("library knowledge packs must use workspace:<slug>"))?;
    if slug.is_empty() {
        bail!("library knowledge pack selector must include a slug")
    }
    Ok(slug)
}

pub(crate) fn unknown_pack(selector: &str) -> anyhow::Error {
    anyhow!(
        "unknown knowledge pack '{selector}'; use 'builtin:nenjo', 'workspace' when a default library pack is available, or 'workspace:<slug>'"
    )
}

pub(crate) fn ensure_known_pack_selector(selector: &str) -> Result<()> {
    if is_nenjo_pack_selector(selector)
        || is_default_library_pack_selector(selector)
        || selector.starts_with("workspace:")
    {
        Ok(())
    } else {
        bail!("unknown knowledge pack selector '{selector}'")
    }
}

pub(crate) fn is_nenjo_pack_selector(selector: &str) -> bool {
    selector == "builtin:nenjo"
}
