# SDK

## Purpose
The Nenjo SDK is the embeddable runtime and manifest authoring surface for
developers building Nenjo-powered systems in code. It covers local manifests,
runtime APIs, model providers, tools, prompt context, memory, worker
composition, and transport contracts.

## Primary Surfaces
- Declarative resource manifests for agents, abilities, domains, routines,
  councils, projects, and supporting resources.
- Provider builder APIs for composing loaders, model factories, tool factories,
  memory, and agent runners.
- Prompt context and template variables for rendering runtime state into agent
  prompts.
- Builtin and project knowledge APIs for reading, searching, and traversing
  document graphs.
- Tool and model provider traits for integrating external execution and model
  backends.
- Worker/runtime crates for connecting SDK execution to platform transport and
  session systems.

## Resource Guidance
SDK resources are represented as manifest files and runtime data structures.
When a user asks for SDK guidance, local authoring, embeddable use, or manifest
schemas, explain the manifest shape and code-level APIs.

Do not assume SDK manifest authoring when the user is asking from platform chat
for help designing or configuring a resource. In that case, route through the
platform domain and resource field guidance first.

## Agent Guidance
Use this domain when the user mentions the Rust SDK, provider builder, local
manifests, manifest files, runtime APIs, code embedding, crates, model provider
implementations, tool traits, worker harness integration, or project-local
resource files.

