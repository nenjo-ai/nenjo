# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.28.0](https://github.com/nenjo-ai/nenjo/compare/v0.27.2...v0.28.0) - 2026-07-15

### Added

- add support for voice transcription ([#95](https://github.com/nenjo-ai/nenjo/pull/95))

## [0.27.2](https://github.com/nenjo-ai/nenjo/compare/v0.27.1...v0.27.2) - 2026-07-12

### Fixed

- use user role instead of developer for steering ([#93](https://github.com/nenjo-ai/nenjo/pull/93))

## [0.27.1](https://github.com/nenjo-ai/nenjo/compare/v0.27.0...v0.27.1) - 2026-07-12

### Fixed

- set developer role support to false ([#91](https://github.com/nenjo-ai/nenjo/pull/91))

## [0.27.0](https://github.com/nenjo-ai/nenjo/compare/v0.26.0...v0.27.0) - 2026-07-12

### Fixed

- allow coexistence of multiple versions of a package ([#87](https://github.com/nenjo-ai/nenjo/pull/87))
- remove version from non dep resources ([#89](https://github.com/nenjo-ai/nenjo/pull/89))

### Other

- Finalized core feature improvements ([#56](https://github.com/nenjo-ai/nenjo/pull/56))

## [0.26.0](https://github.com/nenjo-ai/nenjo/compare/v0.25.0...v0.26.0) - 2026-07-08

### Added

- improved knowledge search ([#85](https://github.com/nenjo-ai/nenjo/pull/85))

## [0.25.0](https://github.com/nenjo-ai/nenjo/compare/v0.24.0...v0.25.0) - 2026-07-07

### Added

- Package runtime arguments for package-authored prompts, including typed `text`, `markdown`, `xml`, and `json` values, declared `args.*` selectors, provider-level bindings, and per-run bindings ([#83](https://github.com/nenjo-ai/nenjo/pull/83)).
- Worker package graph argument sync so platform-supplied org bindings are persisted with installed platform packages and loaded into the runtime provider after `package.graph_changed` ([#83](https://github.com/nenjo-ai/nenjo/pull/83)).
- External package dependency resolution across registries, including scoped dependency names, package source overrides, and package graph validation for external dependencies ([#82](https://github.com/nenjo-ai/nenjo/pull/82)).
- Routine package validation support for slug-based agent step references with ambiguity checks ([#83](https://github.com/nenjo-ai/nenjo/pull/83)).

### Changed

- Hardened `nenpm` provider source fetching for GitHub-backed registries with archive downloads, fetch limits, stale temporary directory cleanup, branch/tag ref handling, and unauthenticated retry behavior for public repositories ([#83](https://github.com/nenjo-ai/nenjo/pull/83)).
- Locked installs can now materialize from `nenpm.lock.yml` without reloading package registries, reducing drift between platform and worker package graph installs ([#83](https://github.com/nenjo-ai/nenjo/pull/83)).
- Prompt construction now returns errors instead of panicking when runtime argument bindings are missing or invalid.

### Fixed

- Updated Rayon's transitive Crossbeam dependencies so `crossbeam-epoch` resolves to `0.9.20` instead of the vulnerable `0.9.18`.

### Other

- update Cargo.toml dependencies

## [0.24.0](https://github.com/nenjo-ai/nenjo/compare/v0.23.0...v0.24.0) - 2026-07-01

### Fixed

- manifest caching and access policy ([#80](https://github.com/nenjo-ai/nenjo/pull/80))

## [0.23.0](https://github.com/nenjo-ai/nenjo/compare/v0.22.0...v0.23.0) - 2026-06-30

### Added

- Custom handoff JSON schema validation for routine edges and route outputs ([#78](https://github.com/nenjo-ai/nenjo/pull/78))
- Payload-aware context compaction to keep provider request bodies under a configurable byte budget ([#78](https://github.com/nenjo-ai/nenjo/pull/78))

### Changed

- Reworked routine routing and execution across agent, gate, council, fan-out, fan-in, retry, and failure flows ([#78](https://github.com/nenjo-ai/nenjo/pull/78))
- Expanded routine/task runtime event coverage and worker event bridging ([#78](https://github.com/nenjo-ai/nenjo/pull/78))
- Hardened related worker tools and package routine validation behavior ([#78](https://github.com/nenjo-ai/nenjo/pull/78))

## [0.22.0](https://github.com/nenjo-ai/nenjo/compare/v0.21.0...v0.22.0) - 2026-06-25

### Fixed

- add explicit hand off instructions in mcp tool surface ([#76](https://github.com/nenjo-ai/nenjo/pull/76))

## [0.21.0](https://github.com/nenjo-ai/nenjo/compare/v0.20.0...v0.21.0) - 2026-06-24

### Other

- Fix/cancellation ([#74](https://github.com/nenjo-ai/nenjo/pull/74))
- Finalized core feature improvements ([#56](https://github.com/nenjo-ai/nenjo/pull/56))

## [0.20.0](https://github.com/nenjo-ai/nenjo/compare/v0.19.0...v0.20.0) - 2026-06-23

### Added

- support agent to agent delegation ([#72](https://github.com/nenjo-ai/nenjo/pull/72))

### Fixed

- update `quinn-proto` to 0.11.15 to address RUSTSEC-2026-0185 ([#72](https://github.com/nenjo-ai/nenjo/pull/72))

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
