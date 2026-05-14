# Nenjo

## Purpose
Nenjo is an agentic work system for designing, operating, and improving AI
resources such as agents, abilities, domains, routines, councils, projects,
tasks, memory, and knowledge documents.

Nenjo has two main surfaces:

- The platform surface, where users work through the dashboard, chat, editors,
  settings, workers, and platform-backed tools.
- The SDK surface, where developers embed the runtime, define portable
  manifests, configure providers, and compose workers or applications directly.

Nenji is the default platform guide and orchestrator. Nenji should identify
which surface the user is working in before recommending concrete actions.

## Core Concepts
- Agents define behavior, prompt structure, model usage, memory focus, scopes,
  abilities, domains, and integrations.
- Abilities are reusable specialist execution modes that an agent can invoke.
- Domains are user-activated modes that expand guidance, scopes, abilities, or
  integrations for a session.
- Routines are workflow graphs composed of steps, gates, agents, councils, and
  triggers.
- Projects organize work, documents, tasks, settings, repository context, and
  executions.
- Project documents provide structured knowledge reference material and graph
  relationships for retrieval.

## Surface Selection
When a user asks about Nenjo at a high level, explain the concept without
assuming either platform UI work or SDK manifest authoring.

When the user is in platform chat or asks Nenji to design, configure, review, or
build a resource, prefer platform resource fields, editor flows, and available
platform tools. Do not tell the user to manually create manifest files unless
the user explicitly asks for SDK, local files, manifests, export/import, or
embedding.

When the user asks about using Nenjo from code, local resource files, manifests,
or embeddable runtime behavior, route to the SDK domain.

## Key Relationships
- `references` the platform domain for dashboard and hosted-product behavior
- `references` the SDK domain for manifest and runtime behavior
- `references` resource guides for shared concepts and fields
- `references` resource surface taxonomy for routing platform versus SDK advice

