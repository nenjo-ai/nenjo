# Executions

## Purpose

Executions are stateful runs that process project tasks through the platform's dispatch system.

An execution is the runtime container for coordinated task processing, not the task definition itself.

## Core Model

An execution belongs to a project and controls:

- which tasks are linked into the run
- how many tasks can run in parallel
- the current lifecycle state of the run
- event and progress tracking for the run

## Lifecycle

- create in `pending`
- start as a separate command
- dispatch ready work up to `parallel_count`
- re-evaluate dependents as tasks finish
- complete when no runnable work remains

## Statuses

- `pending`
- `running`
- `paused`
- `cancelled`
- `completed`
