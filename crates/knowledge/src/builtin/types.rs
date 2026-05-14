use serde::Serialize;

pub type BuiltinKnowledgeManifest = crate::KnowledgePackManifestData;
pub type BuiltinDocManifest = crate::KnowledgeDocManifest;
pub type BuiltinDocEdgeType = crate::KnowledgeDocEdgeType;
pub type BuiltinDocKind = crate::KnowledgeDocKind;
pub type BuiltinDocAuthority = crate::KnowledgeDocAuthority;
pub type BuiltinDocStatus = crate::KnowledgeDocStatus;
pub type BuiltinDocEdge = crate::KnowledgeDocEdge;
pub type BuiltinDocFilter = crate::KnowledgeDocFilter;
pub type BuiltinDocRead = crate::KnowledgeDocRead;
pub type BuiltinDocNeighbor = crate::KnowledgeDocNeighbor;
pub type BuiltinDocNeighborEdge = crate::KnowledgeDocNeighborEdge;
pub type BuiltinDocSearchHit = crate::KnowledgeDocSearchHit;
pub type BuiltinDocTree = crate::KnowledgeDocTree;
pub type BuiltinDocTreeEntry = crate::KnowledgeDocTreeEntry;

#[derive(Debug, Clone)]
pub struct BuiltinKnowledgePack {
    pub manifest: BuiltinKnowledgeManifest,
    pub docs: &'static [BuiltinKnowledgeDoc],
}

#[derive(Debug, Clone, Serialize)]
pub struct BuiltinKnowledgeDoc {
    pub id: &'static str,
    pub virtual_path: &'static str,
    pub content: &'static str,
}
