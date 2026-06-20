# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.19.0](https://github.com/nenjo-ai/nenjo/compare/v0.18.0...v0.19.0) - 2026-06-20

### Added

- Explicit chat response handling and turn input plumbing for runtime and worker chat flows ([#70](https://github.com/nenjo-ai/nenjo/pull/70))

### Fixed

- Preserve command content sidecars through package resolution, lockfiles, installs, and worker assembly ([#70](https://github.com/nenjo-ai/nenjo/pull/70))
- Reject zero interval routine schedules instead of accepting invalid configs ([#70](https://github.com/nenjo-ai/nenjo/pull/70))
- Include loader type context in provider manifest load errors ([#70](https://github.com/nenjo-ai/nenjo/pull/70))

## [0.18.0](https://github.com/nenjo-ai/nenjo/compare/v0.17.0...v0.18.0) - 2026-06-19

### Added

- Native Nenjo slash commands, including command manifests, platform MCP tooling, secure command sync, and worker-side command execution ([#67](https://github.com/nenjo-ai/nenjo/pull/67))
- Platform-backed handling for sensitive manifest instructions, including agent heartbeat instructions, cron routine task content, and routine step instructions ([#68](https://github.com/nenjo-ai/nenjo/pull/68))
- Stable routine step session ids so repeated routine loops preserve each step's transcript and tool context ([#68](https://github.com/nenjo-ai/nenjo/pull/68))
- Push notification tooling that supports recipient lookup, notification listing, user-scoped sends, and source session ids for follow-up chat context ([#68](https://github.com/nenjo-ai/nenjo/pull/68))

### Changed

- Routine manifests now retain cron task metadata and step instructions in the local manifest while platform sync keeps the persisted payload shape aligned ([#68](https://github.com/nenjo-ai/nenjo/pull/68))
- Notification tool descriptions now hide platform implementation details and expose only the agent-facing list, recipient lookup, and send workflows ([#68](https://github.com/nenjo-ai/nenjo/pull/68))

## [0.17.0](https://github.com/nenjo-ai/nenjo/compare/v0.16.0...v0.17.0) - 2026-06-18

### Added

- support model native tools for xai ([#65](https://github.com/nenjo-ai/nenjo/pull/65))

## [0.16.0](https://github.com/nenjo-ai/nenjo/compare/v0.15.0...v0.16.0) - 2026-06-16

### Added

- *(secure-bus)* enforce require-secured-commands config flag ([#63](https://github.com/nenjo-ai/nenjo/pull/63))


## [0.15.0](https://github.com/nenjo-ai/nenjo/compare/v0.14.0...v0.15.0) - 2026-06-14

### Added

- KnowledgePackLoader for dynamic pack discovery and always-on `list_knowledge_packs` tool
- Manifest resource contract for knowledge documents with inline edges in events and worker
- ProjectDetailRecord for REST settings surface and project settings loading in worker
- Async operations support for abilities; improved MCP tooling for abilities, domains, and context blocks

### Changed

- Manifest contract wire types: introduced wire module foundation, record types for resources (including KnowledgePackRecord, ContextBlockRecord, ProjectDetailRecord); completed worker/platform sync and API client consumption of records; removed knowledge_contract compatibility shim and legacy knowledge read backend/helpers
- Library/knowledge alignment: renamed pack_id to pack_slug, reconciled doc UUIDs, centralized discovery and paths, normalized write refs and exposed edge slugs in search, removed pack status from wire types; removed library manifest read tools and `library:read` scope; consolidated wire types in knowledge_contract
- Platform client: added retry policy; refactored subscribe and respond to use org id
- Removed uuids completely and tightened backend to worker contracts
- Moved abilities to async operations; various mcp and manifest tooling improvements

### Fixed

- Worker knowledge sync to only uploaded library knowledge from platform; sync between platform and local cache
- Project settings load from ProjectDetailRecord; resource id sidecar on slug rename/delete
- Domain prompt updates; provider knowledge prompt vars refresh from pack loader
- Agent manifest tooling; manifest tooling fixes

## [0.14.0](https://github.com/nenjo-ai/nenjo/compare/v0.13.0...v0.14.0) - 2026-06-07

### Added

- enable raw web fetch for packages & plugins ([#59](https://github.com/nenjo-ai/nenjo/pull/59))

## [0.13.0](https://github.com/nenjo-ai/nenjo/compare/v0.12.0...v0.13.0) - 2026-06-06

### Added

- bundle `nenjo`, `nenpm`, and `nenjoup` in binary release artifacts
- add binary bundle update commands and update-available notices
- add cron task and heartbeat instructions
- add create knowledge pack MCP tooling
- add encrypted push notifications
- add Claude plugin support
- add chat with councils
- support context block imports of other context blocks

### Changed

- `nenpm update` now updates the installed binary bundle; use `nenpm upgrade`
  to re-resolve package dependencies and rewrite `nenpm.lock.yml`
- `nenpm upgrade` now keeps locked packages within their current major version
  by default; use `nenpm upgrade --major` for explicit major upgrades

### Fixed

- fix routine task execution with git repo
- fix cron routine execution

## [0.12.0](https://github.com/nenjo-ai/nenjo/compare/v0.11.0...v0.12.0) - 2026-05-17

### Added

- add workspace library knowledge packs ([#48](https://github.com/nenjo-ai/nenjo/pull/48))
- improved abilites, domains, and turn loop events
- added configurable nenjo dir
- git worktree isolation, agent identity tracking, and config-driven git identity
- improve memory and resource system
- per user nat account isolation
- inject chat message into agent prompt, harden config defaults, add worker ping
- add nenjo

### Fixed

- release worker to crates io ([#31](https://github.com/nenjo-ai/nenjo/pull/31))
- concurrency nonce for manifest sync
- propagate custom workspace dir
- address dependabot and codeql findings
- nenjo git config stays local
- clean up log output
- ability and domain scoping
- pass manifest into use_ability

### Other

- Feat/packages and plugins ([#51](https://github.com/nenjo-ai/nenjo/pull/51))
- refactor folder structure for mems and res
- add release workflows

## [0.11.0](https://github.com/nenjo-ai/nenjo/compare/v0.10.0...v0.11.0) - 2026-05-04

### Added

- token usage metrics, delegate to enhancment, nats connection info in bootstrap

### Other

- Big changes ([#44](https://github.com/nenjo-ai/nenjo/pull/44))

## [0.10.0](https://github.com/nenjo-ai/nenjo/compare/v0.9.0...v0.10.0) - 2026-04-12

### Added

- send stream events to steam subject ([#39](https://github.com/nenjo-ai/nenjo/pull/39))

### Other

- Session store api ([#38](https://github.com/nenjo-ai/nenjo/pull/38))

## [0.9.0](https://github.com/nenjo-ai/nenjo/compare/v0.8.0...v0.9.0) - 2026-04-11

### Added

- add agent heartbeats ([#35](https://github.com/nenjo-ai/nenjo/pull/35))

### Other

- reduce log contents ([#36](https://github.com/nenjo-ai/nenjo/pull/36))

## [0.8.0](https://github.com/nenjo-ai/nenjo/compare/v0.7.1...v0.8.0) - 2026-04-11

### Fixed

- e2e test all model providers ([#33](https://github.com/nenjo-ai/nenjo/pull/33))

## [0.7.1](https://github.com/nenjo-ai/nenjo/compare/v0.7.0...v0.7.1) - 2026-04-10

### Added

- added configurable nenjo dir

### Fixed

- release worker to crates io ([#31](https://github.com/nenjo-ai/nenjo/pull/31))
- address dependabot and codeql findings
- clean up log output

## [0.7.0](https://github.com/nenjo-ai/nenjo/compare/v0.6.0...v0.7.0) - 2026-04-09

### Added

- improved abilites, domains, and turn loop events

## [0.6.0](https://github.com/nenjo-ai/nenjo/compare/v0.5.0...v0.6.0) - 2026-04-05

### Fixed

- propagate custom workspace dir

## [0.5.0](https://github.com/nenjo-ai/nenjo/compare/v0.4.0...v0.5.0) - 2026-04-05

### Added

- git worktree isolation, agent identity tracking, and config-driven git identity

### Fixed

- address dependabot and codeql findings
- clean up log output

### Other

- refactor folder structure for mems and res

## [0.4.0](https://github.com/nenjo-ai/nenjo/compare/v0.3.2...v0.4.0) - 2026-04-04

### Added

- improve memory and resource system

## [0.3.2](https://github.com/nenjo-ai/nenjo/compare/v0.3.0...v0.3.2) - 2026-04-03

### Fixed

- try force 0.3.2

### Other

- consolidate releases, tags, and changelogs
- release v0.3.1

## [0.3.1](https://github.com/nenjo-ai/nenjo/compare/v0.3.0...v0.3.1) - 2026-04-03

### Fixed

- try force 0.3.2
