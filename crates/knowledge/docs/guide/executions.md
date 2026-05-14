# Executions

## Purpose

Executions are stateful runs that process project tasks, chat turns, scheduled
work, heartbeats, and domain-expanded sessions through the runtime.

An execution is the runtime container for coordinated processing, cancellation,
pausing, transcript recording, checkpoints, and trace state. It is not the task
definition itself.

## Core Model

An execution usually belongs to a project and controls:

- which tasks are linked into the run
- how many tasks can run in parallel
- the current lifecycle state of the run
- event and progress tracking for the run
- active cancellation and pause handles
- session transcript, checkpoint, and trace updates

## Lifecycle

- create in `pending`
- start as a separate command
- dispatch ready work up to `parallel_count`
- re-evaluate dependents as tasks finish
- complete when no runnable work remains

Platform harness handlers track active executions by cancellation key and
execution kind. The shared execution kinds are `chat`, `task`, `cron`, and
`heartbeat`. Domain sessions keep a domain-expanded runner active until the
domain exits or the session ends.

Session runtime state is recorded through `nenjo-sessions` contracts. Worker
storage may clean up terminal file-backed sessions after the configured
retention period.

## Statuses

- `pending`
- `running`
- `paused`
- `cancelled`
- `completed`
