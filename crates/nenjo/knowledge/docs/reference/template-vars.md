# Template Vars

## Purpose

Template variables are the runtime context references available inside prompts and context block templates.

They are the main interface between static prompt design and live execution state.

## Main Variable Groups

### Agent

- `{{ self }}`
- `{{ agent.id }}`
- `{{ agent.role }}`
- `{{ agent.name }}`
- `{{ agent.model }}`
- `{{ agent.description }}`

### Chat

- `{{ chat.message }}`

### Task

- `{{ task }}`
- `{{ task.id }}`
- `{{ task.title }}`
- `{{ task.description }}`
- `{{ task.acceptance_criteria }}`
- `{{ task.tags }}`
- `{{ task.source }}`
- `{{ task.status }}`
- `{{ task.priority }}`
- `{{ task.type }}`
- `{{ task.slug }}`
- `{{ task.complexity }}`

### Project

- `{{ project }}`
- `{{ project.id }}`
- `{{ project.name }}`
- `{{ project.slug }}`
- `{{ project.description }}`
- `{{ project.metadata }}`
- `{{ project.working_dir }}`
- `{{ project.documents }}`

### Builtin Knowledge

- `{{ builtin.documents }}`

### Routine

- `{{ routine }}`
- `{{ routine.id }}`
- `{{ routine.name }}`
- `{{ routine.execution_id }}`
- `{{ routine.step.name }}`
- `{{ routine.step.type }}`
- `{{ routine.step.metadata }}`

### Gate

- `{{ gate.criteria }}`
- `{{ gate.previous_output }}`

### Heartbeat

- `{{ heartbeat.previous_output }}`
- `{{ heartbeat.last_run_at }}`
- `{{ heartbeat.next_run_at }}`

### Subtask

- `{{ subtask.parent_task }}`
- `{{ subtask.description }}`

### Available Resources

- `{{ available_agents }}`
- `{{ available_abilities }}`
- `{{ available_domains }}`

### Memory

- `{{ memories }}`
- `{{ memories.core }}`
- `{{ memories.project }}`
- `{{ memories.shared }}`
- `{{ memory_profile }}`
- `{{ memory_profile.core_focus }}`
- `{{ memory_profile.project_focus }}`
- `{{ memory_profile.shared_focus }}`
- `{{ resources }}`
- `{{ resources.project }}`
- `{{ resources.workspace }}`

### Git

- `{{ git }}`
- `{{ git.current_branch }}`
- `{{ git.target_branch }}`
- `{{ git.work_dir }}`
- `{{ git.repo_url }}`

### Global

- `{{ global.timestamp }}`

## Context Block References

Context blocks are referenced by their path-based name.

Examples:

- `{{ nenjo.core.methodology }}`
- `{{ nenjo.core.delegation }}`
- `{{ custom.coding.standards }}`
