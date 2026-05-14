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
- Built-in and project knowledge pack APIs for reading, searching, and
  traversing document graphs through generic knowledge tools.
- Shared tool API contracts for tool specs, categories, security policy inputs,
  calls, and results.
- Model provider traits for integrating external model backends.
- Runtime and worker crates for connecting SDK execution to platform transport,
  harness handlers, session systems, and concrete native tools.

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
implementations, tool traits, knowledge packs, worker harness integration,
session runtime behavior, or project-local resource files.

## Refactored Runtime Boundaries

After the harness refactor, avoid treating the worker as a single crate that
owns all platform execution logic.

- `nenjo` is the core SDK. It owns manifests, provider construction, agent
  runners, prompt rendering, memory, abilities, domains, councils, and
  routines.
- `nenjo-tool-api` is the shared tool contract crate. It defines tool specs,
  categories, calls, results, autonomy, security inputs, and tool-name
  sanitization.
- `nenjo-knowledge` owns knowledge pack contracts, built-in Nenjo docs, and
  generic knowledge tools for metadata-first document retrieval.
- `nenjo-sessions` owns storage-neutral session, transcript, checkpoint, trace,
  and lease contracts.
- `nenjo-harness` owns platform command handling around an assembled provider:
  chat, task, cron, heartbeat, manifest, repo, domain, session, trace, and
  active execution registries.
- `nenjo-worker` owns the concrete worker process: config, bootstrap, event
  loop, provider/tool assembly, native tools, local project document sync,
  session storage, cleanup, and platform connectivity.
- `nenjo-platform`, `nenjo-events`, `nenjo-eventbus`, `nenjo-secure-envelope`,
  and `nenjo-crypto-auth` own platform REST/MCP contracts, wire events,
  transport, encrypted envelopes, enrollment, and key-provider behavior.

Repository: `https://github.com/nenjo-ai/nenjo`.

For agents that need to fetch source files directly, prefer raw GitHub URLs,
for example `https://raw.githubusercontent.com/nenjo-ai/nenjo/main/README.md`
or the same raw URL pattern for a specific crate file. For knowledge pack
behavior, read `nenjo.guide.knowledge_packs`.
