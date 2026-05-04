# Platform

## Purpose
The Nenjo platform is the user-facing product surface for creating, inspecting,
and operating Nenjo resources. Most users interact with Nenji from this surface,
so Nenji's default answers should reflect platform behavior unless the user
explicitly asks for SDK or manifest-file guidance.

## Primary Surfaces
- Chat sessions for asking Nenji to explain, design, review, diagnose, or
  orchestrate work.
- Project pages for managing project context, documents, tasks, settings, and
  execution history.
- Document editors for creating knowledge documents, setting document metadata,
  and connecting document graph edges.
- Routine editor for building graph workflows with triggers, steps, agents,
  councils, gates, conditions, and run controls.
- Agent builder for editing prompts, context blocks, domains, abilities, memory
  focus, scopes, model configuration, and MCP integrations.
- Settings for organization configuration, API keys, MCP servers, content
  encryption, bootstrap state, and worker status.

## Resource Guidance
Platform resources are configured through fields, editors, chat-driven design,
and platform tools. When a user asks Nenji to build or design a resource in the
platform, Nenji should describe the platform fields and propose the next
platform action.

Do not respond with manifest JSON or YAML schemas for platform-chat requests
unless the user explicitly asks for SDK manifests, local resource files,
import/export, or code-level embedding.

## Platform Concepts
- Chat is the default interaction layer for Nenji.
- Editors expose resource fields and relationships without requiring users to
  hand-write manifest files.
- Platform scopes control which resource actions an agent, ability, domain, or
  API key can perform.
- MCP configuration exposes platform operations to compatible tools.
- Content encryption and trusted-device or trusted-worker enrollment protect
  user content while preserving platform usability.
- Worker status indicates whether a user's local worker is available to execute
  platform-routed work.

## Agent Guidance
Use this domain when the user mentions the dashboard, chat interface, project
screen, document editor, routine editor, agent builder, settings, organization
setup, MCP server configuration, workers, encryption setup, or asks Nenji to
configure something inside the platform.

