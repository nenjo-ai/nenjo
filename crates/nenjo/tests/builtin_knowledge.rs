use std::collections::{HashMap, HashSet};

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
        "nenjo.guide.prompt_structuring",
        "nenjo.guide.councils",
        "nenjo.guide.domains",
        "nenjo.guide.executions",
        "nenjo.guide.memory",
        "nenjo.guide.projects",
        "nenjo.guide.routines",
        "nenjo.guide.scopes",
        "nenjo.guide.tasks",
        "nenjo.reference.template_vars",
        "nenjo.domain.nenjo",
        "nenjo.domain.platform",
        "nenjo.domain.sdk",
        "nenjo.taxonomy.resource_surfaces",
        "nenjo.taxonomy.workflow_patterns",
    ] {
        assert!(pack.read_manifest(id).is_some(), "missing {id}");
    }
}

#[test]
fn scopes_guide_lists_canonical_scope_reference() {
    let pack = builtin_knowledge_pack();
    let doc = pack
        .read_doc("nenjo.guide.scopes")
        .expect("scopes guide exists");

    for scope in [
        "agents:read",
        "agents:write",
        "abilities:read",
        "abilities:write",
        "domains:read",
        "domains:write",
        "projects:read",
        "projects:write",
        "routines:read",
        "routines:write",
        "councils:read",
        "councils:write",
        "context_blocks:read",
        "context_blocks:write",
        "mcp_servers:read",
        "mcp_servers:write",
        "chat:read",
        "chat:write",
        "models:read",
        "models:write",
        "org:read",
        "org:write",
        "org_members:read",
        "org_members:write",
        "org_invites:read",
        "org_invites:write",
        "org_billing:read",
        "org_billing:write",
        "workers:read",
        "workers:approve",
        "workers:write",
        "api_keys:read",
        "api_keys:write",
    ] {
        assert!(
            doc.content.contains(&format!("`{scope}`")),
            "scope guide missing {scope}"
        );
    }

    for invented_scope in ["routines:execute", "tasks:read", "tasks:write"] {
        assert!(
            !doc.content.contains(&format!("`{invented_scope}`")),
            "scope guide should not list non-canonical scope {invented_scope}"
        );
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
    assert_eq!(
        pack.search_paths("chat interface", Default::default())[0].id,
        "nenjo.domain.platform"
    );
    assert_eq!(
        pack.search_paths("manifest files", Default::default())[0].id,
        "nenjo.domain.sdk"
    );
    assert_eq!(
        pack.search_paths("platform vs sdk", Default::default())[0].id,
        "nenjo.taxonomy.resource_surfaces"
    );
}

#[test]
fn builtin_neighbors_expose_graph_relationships() {
    let pack = builtin_knowledge_pack();
    let routine_neighbors = pack.neighbors("nenjo.guide.routines", None);
    assert!(
        routine_neighbors
            .iter()
            .any(|neighbor| neighbor.target == "builtin://nenjo/taxonomy/workflow-patterns.md")
    );

    let agent_neighbors = pack.neighbors("nenjo.guide.agents", None);
    assert!(
        agent_neighbors
            .iter()
            .any(|neighbor| neighbor.target == "builtin://nenjo/guide/abilities.md")
    );
    assert!(
        agent_neighbors
            .iter()
            .any(|neighbor| neighbor.target == "builtin://nenjo/guide/domains.md")
    );
    assert!(
        agent_neighbors
            .iter()
            .any(|neighbor| neighbor.target == "builtin://nenjo/guide/scopes.md")
    );
    assert!(
        agent_neighbors
            .iter()
            .any(|neighbor| neighbor.target == "builtin://nenjo/guide/memory.md")
    );
    assert!(
        agent_neighbors
            .iter()
            .any(|neighbor| neighbor.edges.iter().any(|edge| {
                edge.source == "builtin://nenjo/guide/agents.md" && edge.target == neighbor.target
            }))
    );

    let scoped_neighbors = pack.neighbors(
        "nenjo.reference.resource_dependency_order",
        Some(BuiltinDocEdgeType::DependsOn),
    );
    assert_eq!(scoped_neighbors.len(), 4);
    assert!(scoped_neighbors.iter().all(|neighbor| {
        neighbor
            .edges
            .iter()
            .all(|edge| edge.edge_type == BuiltinDocEdgeType::DependsOn)
    }));
}

#[test]
fn builtin_neighbors_expose_every_manifest_edge_by_id_and_path() {
    let pack = builtin_knowledge_pack();
    let paths_by_id: HashMap<_, _> = pack
        .manifest
        .docs
        .iter()
        .map(|doc| (doc.id.as_str(), doc.virtual_path.as_str()))
        .collect();

    for doc in &pack.manifest.docs {
        let neighbors_by_id = pack.neighbors(&doc.id, None);
        let neighbors_by_path = pack.neighbors(&doc.virtual_path, None);
        assert_eq!(neighbors_by_id, neighbors_by_path);

        for edge in &doc.related {
            let target_path = paths_by_id
                .get(edge.target.as_str())
                .expect("edge target path exists");
            assert!(
                neighbors_by_id.iter().any(|neighbor| {
                    neighbor.target == *target_path
                        && neighbor.edges.iter().any(|neighbor_edge| {
                            neighbor_edge.source == doc.virtual_path
                                && neighbor_edge.target == *target_path
                                && neighbor_edge.edge_type == edge.edge_type
                                && neighbor_edge.note == edge.description
                        })
                }),
                "{} missing exposed outgoing edge {:?} to {}",
                doc.id,
                edge.edge_type,
                edge.target
            );
        }
    }
}

#[test]
fn builtin_neighbors_expose_incoming_edges() {
    let pack = builtin_knowledge_pack();
    let paths_by_id: HashMap<_, _> = pack
        .manifest
        .docs
        .iter()
        .map(|doc| (doc.id.as_str(), doc.virtual_path.as_str()))
        .collect();

    for source in &pack.manifest.docs {
        for edge in &source.related {
            let target = pack
                .read_manifest(&edge.target)
                .expect("edge target manifest exists");
            let target_neighbors = pack.neighbors(&target.id, Some(edge.edge_type));
            assert!(
                target_neighbors.iter().any(|neighbor| {
                    neighbor.target == paths_by_id[source.id.as_str()]
                        && neighbor.edges.iter().any(|neighbor_edge| {
                            neighbor_edge.source == paths_by_id[source.id.as_str()]
                                && neighbor_edge.target == target.virtual_path
                                && neighbor_edge.edge_type == edge.edge_type
                                && neighbor_edge.note == edge.description
                        })
                }),
                "{} missing exposed incoming edge {:?} from {}",
                target.id,
                edge.edge_type,
                source.id
            );
        }
    }
}

#[test]
fn builtin_neighbors_return_empty_for_unknown_docs() {
    let pack = builtin_knowledge_pack();
    assert!(pack.neighbors("nenjo.missing.doc", None).is_empty());
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
