# Entity — Data Model & Schema Knowledge

## Purpose
An Entity defines the structured data model, schema, fields, and relationships for a core concept in the system (e.g., BusinessPlan, Task, ComplianceReport, Violation, Agent). It is the canonical definition of “what this thing looks like.”

## When to Use
- Defining data structures used by agents, routines, or applications
- Creating input/output contracts
- Standardizing how information is represented across the platform

## When to Avoid
- Explaining behavior (use Guide)
- Stating rules (use Policy)
- Classifying (use Taxonomy)

## Core Characteristics
- Clear field definitions with types and constraints
- Relationship declarations
- Often includes example YAML/JSON structures

## Key Relationships
- `part_of` a Domain
- `governs` or `defines` relationships with Policies
- `classifies` via Taxonomies

## Agent Guidance
**Reference this block when:**
- Designing or validating data structures
- Generating or parsing structured input/output
- A user asks “What does a Business Plan contain?”

## Adaptation Notes
Keep Entities focused and version-aware. Changes to core Entities should be treated as breaking changes.