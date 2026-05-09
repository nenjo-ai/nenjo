use std::borrow::Cow;

use super::types::*;
use crate::knowledge::{KnowledgeDocManifest, KnowledgePack};

impl KnowledgePack for BuiltinKnowledgePack {
    fn manifest(&self) -> &dyn crate::knowledge::KnowledgePackManifest {
        &self.manifest
    }

    fn doc_content(&self, manifest: &KnowledgeDocManifest) -> Option<Cow<'_, str>> {
        self.docs
            .iter()
            .find(|doc| doc.id == manifest.id || doc.virtual_path == manifest.virtual_path)
            .map(|doc| Cow::Borrowed(doc.content))
    }
}

impl BuiltinKnowledgePack {
    pub fn list_tree(&self, prefix: Option<&str>) -> BuiltinDocTree {
        <Self as KnowledgePack>::list_tree(self, prefix)
    }

    pub fn list_docs(&self, filter: BuiltinDocFilter) -> Vec<&BuiltinDocManifest> {
        <Self as KnowledgePack>::list_docs(self, filter)
    }

    pub fn read_doc(&self, path: &str) -> Option<BuiltinDocRead> {
        <Self as KnowledgePack>::read_doc(self, path)
    }

    pub fn read_manifest(&self, path: &str) -> Option<&BuiltinDocManifest> {
        <Self as KnowledgePack>::read_manifest(self, path)
    }

    pub fn neighbors(
        &self,
        path: &str,
        edge_type: Option<BuiltinDocEdgeType>,
    ) -> Vec<BuiltinDocNeighbor> {
        <Self as KnowledgePack>::neighbors(self, path, edge_type)
    }

    pub fn search_paths(&self, query: &str, filter: BuiltinDocFilter) -> Vec<BuiltinDocSearchHit> {
        <Self as KnowledgePack>::search_paths(self, query, filter)
    }

    pub fn search_docs(&self, query: &str, filter: BuiltinDocFilter) -> Vec<BuiltinDocSearchHit> {
        <Self as KnowledgePack>::search_docs(self, query, filter)
    }
}
