use std::collections::HashSet;

use nenjo::builtin_knowledge::{BuiltinDocEdgeType, BuiltinDocKind, builtin_knowledge_pack};

#[test]
fn builtin_pack_has_matching_manifest_and_docs() {
    let pack = builtin_knowledge_pack();

    assert_eq!(pack.manifest.pack_id, "nenjo.builtin");
    assert_eq!(pack.manifest.root_uri, "builtin://nenjo/");
    assert_eq!(pack.manifest.schema_version, 1);
    assert!(!pack.manifest.content_hash.is_empty());
    assert_eq!(pack.manifest.docs.len(), pack.docs.len());

    let manifest_ids: HashSet<_> = pack
        .manifest
        .docs
        .iter()
        .map(|doc| doc.id.as_str())
        .collect();
    let embedded_ids: HashSet<_> = pack.docs.iter().map(|doc| doc.id).collect();
    assert_eq!(manifest_ids, embedded_ids);
}

#[test]
fn builtin_pack_has_unique_ids_paths_and_sources() {
    let pack = builtin_knowledge_pack();
    let mut ids = HashSet::new();
    let mut virtual_paths = HashSet::new();
    let mut source_paths = HashSet::new();

    for doc in &pack.manifest.docs {
        assert!(ids.insert(doc.id.as_str()), "duplicate id {}", doc.id);
        assert!(
            virtual_paths.insert(doc.virtual_path.as_str()),
            "duplicate virtual path {}",
            doc.virtual_path
        );
        assert!(
            source_paths.insert(doc.source_path.as_str()),
            "duplicate source path {}",
            doc.source_path
        );
        assert!(doc.virtual_path.starts_with("builtin://nenjo/"));
    }
}

#[test]
fn builtin_pack_edges_and_tags_are_valid() {
    let pack = builtin_knowledge_pack();
    let ids: HashSet<_> = pack
        .manifest
        .docs
        .iter()
        .map(|doc| doc.id.as_str())
        .collect();
    let allowed_tag_prefixes = [
        "domain:",
        "resource:",
        "operation:",
        "pattern:",
        "risk:",
        "audience:",
    ];

    for doc in &pack.manifest.docs {
        for edge in &doc.related {
            assert!(
                ids.contains(edge.target.as_str()),
                "{} has missing edge target {}",
                doc.id,
                edge.target
            );
            assert!(matches!(
                edge.edge_type,
                BuiltinDocEdgeType::PartOf
                    | BuiltinDocEdgeType::Defines
                    | BuiltinDocEdgeType::Governs
                    | BuiltinDocEdgeType::Classifies
                    | BuiltinDocEdgeType::References
                    | BuiltinDocEdgeType::DependsOn
                    | BuiltinDocEdgeType::Extends
                    | BuiltinDocEdgeType::RelatedTo
            ));
        }
        for tag in &doc.tags {
            assert!(
                allowed_tag_prefixes
                    .iter()
                    .any(|prefix| tag.starts_with(prefix)),
                "{} has invalid tag {}",
                doc.id,
                tag
            );
        }
    }
}

#[test]
fn builtin_pack_contains_required_guide_docs() {
    let pack = builtin_knowledge_pack();
    for id in [
        "nenjo.guide.abilities",
        "nenjo.guide.agents",
        "nenjo.guide.context_blocks",
        "nenjo.guide.councils",
        "nenjo.guide.domains",
        "nenjo.guide.executions",
        "nenjo.guide.memory",
        "nenjo.guide.projects",
        "nenjo.guide.routines",
        "nenjo.guide.scopes",
        "nenjo.guide.tasks",
        "nenjo.reference.template_vars",
        "nenjo.domain.nenjo_platform",
        "nenjo.taxonomy.workflow_patterns",
    ] {
        assert!(pack.read_manifest(id).is_some(), "missing {id}");
    }
}

#[test]
fn builtin_tree_and_read_work_by_path_and_id() {
    let pack = builtin_knowledge_pack();
    let tree = pack.list_tree(None);

    assert!(
        tree.entries
            .iter()
            .any(|entry| entry.path == "builtin://nenjo/guide/routines.md")
    );
    assert!(
        tree.entries
            .iter()
            .any(|entry| entry.path.starts_with("builtin://nenjo/domain/"))
    );
    assert!(
        tree.entries
            .iter()
            .any(|entry| entry.path.starts_with("builtin://nenjo/reference/"))
    );
    assert!(
        tree.entries
            .iter()
            .any(|entry| entry.path.starts_with("builtin://nenjo/taxonomy/"))
    );

    let by_path = pack
        .read_doc("builtin://nenjo/guide/routines.md")
        .expect("read routines by path");
    let by_id = pack
        .read_doc("nenjo.guide.routines")
        .expect("read routines by id");
    assert_eq!(by_path.manifest.id, "nenjo.guide.routines");
    assert_eq!(
        by_id.manifest.virtual_path,
        "builtin://nenjo/guide/routines.md"
    );
    assert!(by_path.content.contains("# Routines"));
}

#[test]
fn builtin_search_finds_expected_concepts() {
    let pack = builtin_knowledge_pack();

    assert_eq!(
        pack.search_paths("workflow", Default::default())[0].id,
        "nenjo.taxonomy.workflow_patterns"
    );
    assert_eq!(
        pack.search_paths("permission", Default::default())[0].id,
        "nenjo.guide.scopes"
    );
    assert_eq!(
        pack.search_paths("mode", Default::default())[0].id,
        "nenjo.guide.domains"
    );
}

#[test]
fn builtin_neighbors_expose_graph_relationships() {
    let pack = builtin_knowledge_pack();
    let routine_neighbors = pack.neighbors("nenjo.guide.routines", None);
    assert!(
        routine_neighbors
            .iter()
            .any(|doc| doc.id == "nenjo.taxonomy.workflow_patterns")
    );

    let agent_neighbors = pack.neighbors("nenjo.guide.agents", None);
    assert!(
        agent_neighbors
            .iter()
            .any(|doc| doc.id == "nenjo.guide.abilities")
    );
    assert!(
        agent_neighbors
            .iter()
            .any(|doc| doc.id == "nenjo.guide.domains")
    );
    assert!(
        agent_neighbors
            .iter()
            .any(|doc| doc.id == "nenjo.guide.scopes")
    );
    assert!(
        agent_neighbors
            .iter()
            .any(|doc| doc.id == "nenjo.guide.memory")
    );

    let scoped_neighbors = pack.neighbors(
        "nenjo.reference.resource_dependency_order",
        Some(BuiltinDocEdgeType::DependsOn),
    );
    assert_eq!(scoped_neighbors.len(), 4);
}

#[test]
fn compact_search_and_manifest_do_not_include_bodies() {
    let pack = builtin_knowledge_pack();

    let paths = pack.search_paths("workflow", Default::default());
    assert!(paths.iter().all(|hit| hit.content.is_none()));

    let docs = pack.search_docs("workflow", Default::default());
    assert!(docs.iter().any(|hit| hit.content.is_some()));

    let manifest = pack
        .read_manifest("nenjo.guide.routines")
        .expect("manifest exists");
    assert_eq!(manifest.kind, BuiltinDocKind::Guide);
}
