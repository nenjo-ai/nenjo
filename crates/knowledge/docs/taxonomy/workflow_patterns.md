# Workflow Pattern Taxonomy

**Path:** `nenjo.taxonomy.workflow_patterns`  
**Kind:** Taxonomy  
**Status:** stable

## Purpose
A classification system for the different types of workflow patterns used in Nenjo routines. This taxonomy helps agents and humans choose the right structure for a given problem.

## Core Workflow Patterns

| Pattern            | Description                                                                 | Best Used When                                      | Key Characteristics |
|--------------------|-----------------------------------------------------------------------------|-----------------------------------------------------|---------------------|
| **Linear Pipeline** | Simple sequential execution of steps                                        | Work naturally flows in a fixed order               | No branching, predictable path |
| **Gated Pipeline**  | Sequential steps with validation gates at key points                        | Quality, safety, or compliance must be checked      | Explicit pass/fail decisions |
| **Fan Out**         | Parallel independent work followed by aggregation or review                 | Multiple independent analyses or generations needed | Parallel execution + join step |
| **Review Pipeline** | Generation → Review → Approval/Rejection loop                               | Output quality or compliance requires human/agent review | Iterative refinement |
| **Decomposition**   | Leader breaks work into subtasks assigned to specialists                    | Complex work that benefits from division of labor   | Clear subtask boundaries |
| **Council**         | Structured multi-agent collaboration with defined delegation strategy       | Multiple perspectives or synthesis are required     | Leader + members with roles |
| **Adversarial**     | One agent proposes, another critiques, leader decides                       | High-stakes decisions requiring critical thinking   | Built-in challenge mechanism |
| **Iterative**       | Repeated cycles of generation + evaluation until criteria met               | Creative or optimization work                       | Loop with exit condition |

## Decision Framework

Use this taxonomy when designing routines:

1. Start with **Linear Pipeline** if the work is simple and sequential.
2. Add **Gates** if validation is required at key stages.
3. Use **Fan Out** when parallel independent work improves speed or quality.
4. Choose **Review Pipeline** when output quality matters.
5. Use **Council** when multiple perspectives or synthesis are genuinely needed.
6. Consider **Decomposition** or **Adversarial** for complex or high-stakes work.

## Key Relationships

- `classifies` → Routine steps and patterns
- `references` → Guides for each specific pattern
- `governs` → How routines are designed and composed

## Notes
This taxonomy is intentionally extensible. New patterns can be added as the platform evolves.