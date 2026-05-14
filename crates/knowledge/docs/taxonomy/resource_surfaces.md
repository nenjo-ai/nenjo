# Resource Surface Taxonomy

## Purpose
Resource surfaces classify how the same Nenjo resource concept appears across
the platform and SDK.

## Surface Classes

| Surface | Meaning | Indicators |
|---|---|---|
| `platform_ui` | Dashboard and editor fields shown to users | The user mentions chat, dashboard, project pages, document editor, routine editor, agent builder, settings, or worker status |
| `platform_mcp` | Platform-backed tools and API operations | The user asks Nenji to inspect, create, update, or verify platform resources |
| `sdk_manifest` | Local declarative manifest files | The user explicitly asks for manifests, local files, import/export, SDK config, or portable resource definitions |
| `sdk_runtime` | Runtime structs, builders, crates, traits, and execution APIs | The user asks about embedding Nenjo, writing code, crate behavior, tools, providers, memory, or worker composition |
| `sdk_runtime_boundary` | Crate ownership and worker/harness separation | The user asks where code lives, how the harness refactor is organized, or what owns sessions/tools/events |
| `knowledge_pack` | Built-in, project, filesystem, or remote document packs exposed through knowledge tools | The user asks about `{{ builtin.nenjo }}`, project documents, document graph retrieval, or registering docs in a provider |
| `shared_fields` | Conceptual fields and semantics common to both surfaces | The user asks what a field means or how a resource should be designed independent of a surface |

## Examples

| User Request | Correct Surface | Guidance Style |
|---|---|---|
| "I need an agent that reviews incidents." | `platform_ui`, `platform_mcp`, `shared_fields` | Describe agent fields, abilities, scopes, and prompt structure; recommend creating/updating the agent through platform tools |
| "Give me the manifest for an incident-review agent." | `sdk_manifest` | Provide the local manifest shape and field-level explanation |
| "How should I structure a routine for code review?" | `platform_ui`, `shared_fields` | Describe routine steps, gates, agent assignments, and editor graph structure |
| "How do I embed a Nenjo agent runner in Rust?" | `sdk_runtime` | Explain provider builder, model factory, tool factory, memory, and runner APIs |
| "Which crate owns worker session handling now?" | `sdk_runtime_boundary` | Explain `nenjo-sessions`, `nenjo-harness`, and `nenjo-worker` responsibilities |
| "How should my agent read project docs?" | `knowledge_pack` | Explain project document indexes, knowledge tools, graph neighbors, and full document reads |
| "What does platform_scopes mean?" | `shared_fields` | Explain the field once, then branch into platform or SDK only if needed |
