# Projects — Top-Level Containers for Work

## Purpose
Projects are the top-level containers for work in Nenjo. They group tasks, documents, settings, repository connections, and execution runs into a single, coherent workspace. Every significant body of work in Nenjo lives inside a Project.

## Core Concepts

- A Project is the **primary unit of organization** and ownership
- Projects provide isolation for tasks, documents, memory, and execution history
- Projects can be connected to a Git repository for version-controlled, workspace-driven workflows
- Projects serve as the boundary for knowledge organization and agent memory scoping

## Key Fields

- `name` — Human-readable project name
- `slug` — URL-friendly unique identifier
- `description` — Overview of the project’s purpose and goals
- `settings` — Project-level configuration (default models, memory profiles, permissions, etc.)
- `repo_url` — Optional Git repository connection for workspace-driven execution

## Documents & Knowledge Graph

One of the most powerful features of a Project is its ability to host **structured documents** that together form a **local knowledge graph**.

By organizing documents using the six `BuiltinDocKind` types (`Guide`, `Reference`, `Taxonomy`, `Domain`, `Entity`, `Policy`) inside a Project, you can build a complete **AI Operating System** tailored to that project.

### How It Works

- **Domain** documents define the vertical or problem space (e.g., “Regulatory Compliance”)
- **Entity** documents define the data models used in the project
- **Policy** documents encode rules, compliance criteria, and decision logic
- **Taxonomy** documents provide classification systems
- **Reference** documents store facts, glossaries, and lookup tables
- **Guide** documents provide explanatory context and usage patterns

When these documents are properly linked using canonical relationships (`part_of`, `defines`, `governs`, `references`, etc.), they create a **queryable knowledge graph** that agents can navigate intelligently.

This turns a Project into a true **operating system** — not just a container for tasks, but a self-describing, self-improving environment where agents have deep contextual understanding.

## Knowledge Graph Templates & Examples

To help teams quickly build high-quality knowledge graphs inside their projects, we provide a dedicated reference document containing **ready-to-use templates and examples** for each of the six document kinds.

**Reference Material:** `nenjo.reference.knowledge`

This reference includes:

- Template structures for `Domain`, `Entity`, `Policy`, `Taxonomy`, `Guide`, and `Reference` documents
- Real-world examples from different verticals
- Best practices for linking documents using canonical relationships
- Starter kits for common project types (compliance, product development, research, etc.)

Teams are encouraged to copy and adapt these templates when initializing the knowledge layer of a new Project.

## Runtime Behavior

- All tasks created inside a Project inherit its settings and memory scope
- Agents operating on Project tasks use the Project’s `project_focus` memory
- Execution runs are scoped to the Project
- Document-based knowledge is available to any agent or routine running inside the Project

## Key Relationships (Canonical)

- `part_of` → Nenjo Platform
- `contains` → Tasks, Executions, and Documents
- `defines` → Project-level settings and knowledge graph structure
- `governs` → Memory scoping and execution boundaries
- `references` → `nenjo.reference.knowledge` (templates and examples)

## Common Patterns

- **Product Development Project** — Strong Entity + Policy documents + task-driven routines
- **Compliance / Audit Project** — Heavy use of Domain + Policy + Taxonomy documents
- **Research / Analysis Project** — Rich Reference + Guide documents with exploratory routines
- **Client Delivery Project** — Balanced mix with strong shared_focus memory

## Agent Guidance

**Reference this block when:**
- Creating or configuring a new project
- Explaining how knowledge is organized inside a workspace
- Designing project-level knowledge graphs or operating systems
- Troubleshooting scoping or memory issues across projects

## Pitfalls to Avoid

- Treating Projects as simple folders (they are knowledge + execution environments)
- Neglecting to build a proper document-based knowledge graph for complex projects
- Mixing unrelated work into a single Project (hurts memory isolation and clarity)
- Forgetting to connect a repository when workspace-driven workflows are needed

## Best Practices

- Start every significant body of work with a Project
- Invest early in building a small but high-quality set of Domain, Entity, and Policy documents
- Use the knowledge graph to make agents dramatically more effective inside the Project
- Keep project memory profiles focused and regularly reviewed
- Leverage repository connections for version-controlled knowledge and code
- Start from the templates in `nenjo.reference.knowledge` to accelerate setup