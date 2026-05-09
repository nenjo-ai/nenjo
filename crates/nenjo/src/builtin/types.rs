use serde::Serialize;

pub type BuiltinKnowledgeManifest = crate::knowledge::KnowledgePackManifestData;
pub type BuiltinDocManifest = crate::knowledge::KnowledgeDocManifest;
pub type BuiltinDocEdgeType = crate::knowledge::KnowledgeDocEdgeType;
pub type BuiltinDocKind = crate::knowledge::KnowledgeDocKind;
pub type BuiltinDocAuthority = crate::knowledge::KnowledgeDocAuthority;
pub type BuiltinDocStatus = crate::knowledge::KnowledgeDocStatus;
pub type BuiltinDocEdge = crate::knowledge::KnowledgeDocEdge;
pub type BuiltinDocFilter = crate::knowledge::KnowledgeDocFilter;
pub type BuiltinDocRead = crate::knowledge::KnowledgeDocRead;
pub type BuiltinDocNeighbor = crate::knowledge::KnowledgeDocNeighbor;
pub type BuiltinDocNeighborEdge = crate::knowledge::KnowledgeDocNeighborEdge;
pub type BuiltinDocSearchHit = crate::knowledge::KnowledgeDocSearchHit;
pub type BuiltinDocTree = crate::knowledge::KnowledgeDocTree;
pub type BuiltinDocTreeEntry = crate::knowledge::KnowledgeDocTreeEntry;

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
