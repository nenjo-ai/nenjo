# Policy — Rules & Governance Knowledge

## Purpose
A Policy defines rules, regulations, compliance criteria, business logic, evaluation standards, or governance constraints. It tells agents *what must be true* or *how decisions should be made*.

## When to Use
- Encoding regulatory requirements (GDPR, SOC 2, etc.)
- Defining business rules and decision criteria
- Creating evaluation or scoring logic
- Establishing guardrails and constraints

## When to Avoid
- Explaining concepts (use Guide)
- Defining data shapes (use Entity)
- General classification (use Taxonomy)

## Core Characteristics
- Clear, enforceable statements (“must”, “shall”, “should”)
- Evaluation criteria and severity levels
- Often machine-readable or checklist-style

## Key Relationships
- `governs` Entities and Domains
- `classifies` via Taxonomies
- `part_of` larger compliance or governance frameworks

## Agent Guidance
**Reference this block when:**
- Evaluating compliance, risk, or quality
- Enforcing rules in a workflow
- A user asks “Is this compliant?” or “What are the requirements?”

## Adaptation Notes
Policies should be precise and auditable. Version them carefully as they often have legal implications.