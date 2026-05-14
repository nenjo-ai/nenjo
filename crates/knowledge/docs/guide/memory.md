# Memory — Scoped Agent Memory System

## Purpose
Every agent in Nenjo maintains its own memory system divided into three distinct scopes: **core**, **project**, and **shared**. A `memory_profile` tells the agent exactly **what** to remember and **where** to store or recall that knowledge. This design prevents context pollution, enables long-term learning, and supports both project-specific and cross-project intelligence.

Memory is different from project documents and artifacts. Memory stores learned
facts and preferences. Project documents are explicit knowledge sources.
Artifacts are saved files or generated outputs that can be indexed in prompts
through `{{ artifacts }}`, `{{ artifacts.project }}`, and
`{{ artifacts.workspace }}`.

## The Three Memory Scopes

| Scope            | What It Stores                                      | Scope of Visibility          | Typical Content |
|------------------|-----------------------------------------------------|------------------------------|-----------------|
| **core_focus**   | Long-term, cross-project knowledge and principles   | Global to the agent          | Methodology, personality, core principles, long-term goals |
| **project_focus**| Knowledge specific to one project                   | Isolated per project         | Task history, decisions, documents, acceptance criteria, project context |
| **shared_focus** | Knowledge shared across projects or with other agents | Visible across projects      | Reusable patterns, team conventions, common solutions |

**Design Goal**: Prevent important project-specific details from leaking into global memory, while still allowing valuable cross-project learning.

## Memory Profiles

A `memory_profile` is attached to every agent and defines the focus areas for each scope. Memory profiles are stored in **CSV format**.

### Example Memory Profile (CSV)

```yaml
memory_profile:
  core_focus:
    - user corrections
    - methodology
    - long-term principles
    - personality traits
  project_focus:
    - task decisions
    - acceptance criteria
    - project documents
    - stakeholder preferences
  shared_focus:
    - reusable patterns
    - team conventions
    - common solutions
    - cross-project learnings
```

## Runtime Behavior

- Memory is automatically scoped at runtime based on the active memory_profile
- The agent can only read/write within the scopes defined in its profile
- project_focus memory is isolated per project
- core_focus and shared_focus are visible across projects (when explicitly enabled)
- Memory is injected into prompts via template variables: {{ memories.core }}, {{ memories.project }}, {{ memories.shared }}
- Artifact indexes are injected separately via {{ artifacts }}, {{ artifacts.project }}, and {{ artifacts.workspace }}

## Key Relationships

- part_of → Agent (via memory_profile)
- defines → structure and content of the three memory scopes
- references → Template Variables (nenjo.guide.template_vars)
- references → Knowledge Packs (nenjo.guide.knowledge_packs)

## Common Patterns

- Specialist Agent: Heavy project_focus, light core_focus, minimal shared_focus
- Bootstrap / Nenji Agent: Strong core_focus + broad shared_focus
- Long-running Council Member: Balanced core_focus + strong shared_focus
- Project-Specific Analyst: Very strong project_focus, minimal other scopes

## Agent Guidance
**Reference this block when:**

- Designing or configuring an agent’s memory profile
- Troubleshooting why an agent “forgot” something or is using wrong context
- Explaining memory behavior to users
- Building agents that need to operate across multiple projects

## Pitfalls to Avoid

- Putting project-specific data into core_focus (causes leakage between projects)
- Treating artifacts or project documents as memory
- Leaving memory_profile undefined or empty on long-running agents
- Forgetting to update project_focus after major requirement or scope changes
- Overloading shared_focus with low-value information

Best Practices

- Start simple: Begin with only project_focus and expand later
- Keep core_focus focused on timeless principles and methodology
- Use shared_focus for genuinely reusable patterns across projects
- Regularly review and prune memory profiles as projects evolve
- Make memory profiles visible and editable in the UI
