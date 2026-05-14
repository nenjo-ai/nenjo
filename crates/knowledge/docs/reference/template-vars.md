# Template Vars

## Purpose

Template variables are the runtime context references available inside prompts and context block templates.

They are the main interface between static prompt design and live execution state. Use them to keep prompts reusable while still grounding each turn in the active agent, task, project, routine, memory, available collaborators, abilities, and knowledge sources.

This reference is for lookup. For prompt composition patterns, read `nenjo.guide.prompt_structuring`.

## How Values Render

Most singular variables render as concise XML-like blocks or scalar text. Aggregate variables such as `{{ available_agents }}`, `{{ available_abilities }}`, `{{ project.documents }}`, `{{ memories }}`, and `{{ builtin.nenjo }}` render as structured XML-like context.

Prefer aggregate variables when the model needs to scan a collection. Prefer scalar fields when the prompt needs one stable value.

## Main Variable Groups

### Agent

- `{{ self }}`
- `{{ agent.id }}`
- `{{ agent.role }}`
- `{{ agent.name }}`
- `{{ agent.model }}`
- `{{ agent.description }}`

Use agent variables in system prompts and reusable context blocks when role, identity, or operating style matters.

Example:

```jinja
You are {{ agent.name }}.

Role:
{{ agent.description }}
```

Rendered shape:

```xml
<agent>
  <id>...</id>
  <name>coder</name>
  <model>claude-haiku</model>
  <description>Implements and reviews software changes.</description>
</agent>
```

Avoid using agent identity to repeat static policy. Put reusable policy in context blocks.

### Chat

- `{{ chat.message }}`

Use this in chat templates when the user message should be passed through directly.

Example:

```jinja
User request:
{{ chat.message }}
```

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

Use task variables in task execution templates, routine step prompts, and gate evaluation prompts.

Example:

```jinja
Current task:
{{ task }}

Acceptance criteria:
{{ task.acceptance_criteria }}
```

Rendered shape:

```xml
<task>
  <id>...</id>
  <title>Implement release-note routine</title>
  <description>Draft release notes, review them, and publish after approval.</description>
  <priority>high</priority>
  <type>feature</type>
  <complexity>medium</complexity>
</task>
```

Avoid copying task details into the system prompt. Task details are runtime data.

### Project

- `{{ project }}`
- `{{ project.id }}`
- `{{ project.name }}`
- `{{ project.slug }}`
- `{{ project.description }}`
- `{{ project.metadata }}`
- `{{ project.working_dir }}`
- `{{ project.documents }}`

Use project variables when the answer or action depends on project identity, workspace path, local conventions, or project documents.

Example:

```jinja
Project:
{{ project }}

Project documents:
{{ project.documents }}
```

Rendered shape:

```xml
<project>
  <id>...</id>
  <name>Billing Platform</name>
  <slug>billing-platform</slug>
  <description>Services for invoices, payments, and entitlements.</description>
  <working_dir>/workspace/billing-platform</working_dir>
  <project_documents>
    <document>
      <id>...</id>
      <title>Architecture Overview</title>
      <path>docs/architecture.md</path>
      <summary>System boundaries and service ownership.</summary>
      <tags>
        <tag>domain:architecture</tag>
      </tags>
    </document>
  </project_documents>
</project>
```

`{{ project.documents }}` is for project knowledge and document metadata. Use it when project docs should influence the response. Do not treat it as long-term memory.

Example rendered shape:

```xml
<project_documents>
  <document>
    <id>...</id>
    <title>Architecture Overview</title>
    <path>docs/architecture.md</path>
    <summary>System boundaries and service ownership.</summary>
    <tags>
      <tag>domain:architecture</tag>
    </tags>
  </document>
</project_documents>
```

Avoid injecting `{{ project.documents }}` into every prompt by default. Prefer it for knowledge-heavy project work, planning, and documentation-aware tasks.

### Built-In Knowledge

- `{{ builtin.nenjo }}`

Use built-in Nenjo knowledge as a discovery hint for Nenjo platform concepts, resource patterns, and built-in documentation. It lists the built-in Nenjo document namespace and tells agents which generic knowledge tools are available.

Example:

```jinja
Nenjo platform knowledge:
{{ builtin.nenjo }}
```

Rendered shape:

```xml
<knowledge_pack source="builtin" name="nenjo" root="builtin://nenjo/">
  <usage>Use list_knowledge_tree, search_knowledge_paths, read_knowledge_doc_manifest, list_knowledge_neighbors, and read_knowledge_doc with pack="builtin:nenjo"...</usage>
  <doc id="nenjo.guide.agents" path="builtin://nenjo/guide/agents.md" kind="guide" title="Agents">
    <summary>Primary behavioral units...</summary>
  </doc>
</knowledge_pack>
```

Use this as an index, not as the final source of truth. Agents should search, inspect neighbors, and read selected docs when the user asks about Nenjo concepts.

### Routine

- `{{ routine }}`
- `{{ routine.id }}`
- `{{ routine.name }}`
- `{{ routine.execution_id }}`
- `{{ routine.step.name }}`
- `{{ routine.step.type }}`
- `{{ routine.step.metadata }}`

Use routine variables in routine step prompts, gate prompts, and cron-driven workflow prompts.

Example:

```jinja
Routine context:
{{ routine }}

Current step:
{{ routine.step.name }} ({{ routine.step.type }})
```

Rendered shape:

```xml
<routine>
  <id>...</id>
  <name>Release Review</name>
  <execution_id>...</execution_id>
  <step>
    <name>review_notes</name>
    <type>gate</type>
  </step>
</routine>
```

### Gate

- `{{ gate.criteria }}`
- `{{ gate.previous_output }}`

Use gate variables only in gate evaluation templates. They should focus the model on evidence and pass/fail criteria.

Example:

```jinja
Evaluate the previous output against the criteria.

Criteria:
{{ gate.criteria }}

Previous output:
{{ gate.previous_output }}
```

### Heartbeat

- `{{ heartbeat.previous_output }}`
- `{{ heartbeat.last_run_at }}`
- `{{ heartbeat.next_run_at }}`

Use heartbeat variables in periodic autonomous checks, maintenance loops, and recurring status tasks.

### Subtask

- `{{ subtask.parent_task }}`
- `{{ subtask.description }}`

Use subtask variables when a council or delegation flow asks an agent to handle a focused portion of larger work.

### Available Resources

- `{{ available_agents }}`
- `{{ available_abilities }}`
- `{{ available_domains }}`

Use these when the agent needs to select collaborators, invoke specialist abilities, or explain available execution modes.

`{{ available_agents }}` example:

```xml
<available_agents>
  <agent>
    <id>...</id>
    <name>reviewer</name>
    <model>claude-haiku</model>
    <description>Reviews code, architecture, and acceptance criteria.</description>
  </agent>
  <agent>
    <id>...</id>
    <name>implementer</name>
    <model>claude-haiku</model>
    <description>Implements scoped software changes.</description>
  </agent>
</available_agents>
```

Use `{{ available_agents }}` for delegation, council leadership, routing, and handoff decisions. Avoid it in single-agent prompts where no delegation is expected.

`{{ available_abilities }}` example:

```xml
<available_abilities>
  <ability>
    <name>code_review</name>
    <tool_name>code_review</tool_name>
    <description>Reviews code for correctness, maintainability, and risk.</description>
    <activation_condition>Use when the user asks for review or when a change needs validation.</activation_condition>
  </ability>
</available_abilities>
```

Use `{{ available_abilities }}` when the agent needs to decide whether to call a specialist behavior. Avoid listing abilities in prompts that should not invoke them.

`{{ available_domains }}` example:

```xml
<available_domains>
  <domain>
    <id>...</id>
    <name>debug</name>
    <description>Expanded diagnostic mode for debugging runtime issues.</description>
  </domain>
</available_domains>
```

Use domains for explicit mode switching, elevated behavior, or session-specific expansion.

### Memory

- `{{ memories }}`
- `{{ memories.core }}`
- `{{ memories.project }}`
- `{{ memories.shared }}`
- `{{ memory_profile }}`
- `{{ memory_profile.core_focus }}`
- `{{ memory_profile.project_focus }}`
- `{{ memory_profile.shared_focus }}`
- `{{ artifacts }}`
- `{{ artifacts.project }}`
- `{{ artifacts.workspace }}`

Use memory variables when prior learned facts, preferences, and reusable knowledge should influence behavior.

Example:

```jinja
Memory profile:
{{ memory_profile }}

Relevant memories:
{{ memories }}
```

Rendered shape:

```xml
<memories>
  <core>
    <memory category="style">Prefer concise implementation notes.</memory>
  </core>
  <project>
    <memory category="architecture">Use Axum for HTTP services.</memory>
  </project>
  <shared>
    <memory category="review">Check migrations for rollback safety.</memory>
  </shared>
</memories>
```

Use `{{ artifacts }}` for saved artifact files and workspace/project outputs.

Avoid confusing memory with project documents. Memory is learned, evolving context. Project documents are explicit knowledge artifacts.

### Git

- `{{ git }}`
- `{{ git.current_branch }}`
- `{{ git.target_branch }}`
- `{{ git.work_dir }}`
- `{{ git.repo_url }}`

Use git variables when the task depends on repository state, branch targeting, or local workspace paths.

### Global

- `{{ global.timestamp }}`

Use global timestamp for recurring tasks, audit notes, and time-aware output.

## Context Block References

Context blocks are referenced by their path-based name.

Examples:

- `{{ nenjo.core.methodology }}`
- `{{ nenjo.core.delegation }}`
- `{{ custom.coding.standards }}`

Use context block variables for durable, reusable operating knowledge. Keep volatile runtime data in task, chat, project, memory, or routine variables.

## Placement Guide

| Variable group | Best placement | Avoid |
| --- | --- | --- |
| `agent`, `self` | System prompt, context blocks | Repeating static policy |
| `chat` | Chat template | System prompt |
| `task` | Task template, routine steps, gates | Generic chat prompts |
| `project` | Project-aware task prompts | Global prompts with no project |
| `project.documents` | Knowledge-heavy project work | Every prompt by default |
| `builtin.nenjo` | Nenjo concept discovery | Treating summaries as full docs |
| `available_agents` | Delegation and councils | Single-agent workflows |
| `available_abilities` | Capability selection | Prompts where abilities are unavailable |
| `available_domains` | Explicit mode switching | Hidden privilege expansion |
| `memories` | Personalized or learned behavior | Source-of-truth project docs |
| `artifacts` | Saved artifact files and workspace outputs | Long-term preferences |
| `routine`, `gate` | Routine and gate templates | Plain chat prompts |

## Common Patterns

### Project Task Agent

```jinja
You are {{ agent.name }}.

Role:
{{ agent.description }}

Project:
{{ project }}

Task:
{{ task }}

Relevant project documents:
{{ project.documents }}

Relevant memories:
{{ memories.project }}
```

### Ability-Aware Agent

```jinja
You are {{ agent.name }}.

Role:
{{ agent.description }}

Available specialist abilities:
{{ available_abilities }}

User request:
{{ chat.message }}
```

### Gate Evaluator

```jinja
You are evaluating whether work satisfies explicit criteria.

Criteria:
{{ gate.criteria }}

Evidence:
{{ gate.previous_output }}

Return a clear pass or fail decision and explain only the deciding evidence.
```

### Built-In Knowledge Discovery

```jinja
Use builtin Nenjo knowledge when the user asks about platform concepts, resource design, or prompt structuring.

Builtin knowledge index:
{{ builtin.nenjo }}

User request:
{{ chat.message }}
```

## Anti-Patterns

- Universal prompts that include every variable group.
- Putting volatile task details in system prompts.
- Dumping full project documents when summaries or targeted reads are enough.
- Duplicating context block text into multiple agent prompts.
- Treating memory as project documentation.
- Treating project documents as memory.
- Exposing available agents, abilities, or domains when the agent should not use them.
