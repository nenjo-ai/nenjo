# Taxonomy — Classification & Ontology Knowledge

## Purpose
A Taxonomy defines classification systems, ontologies, categories, risk levels, frameworks, or tagging structures. It enables agents to organize and reason about knowledge hierarchically.

## When to Use
- Creating classification systems (e.g., compliance risk levels, task types, priority scales)
- Building ontologies or tag taxonomies
- Defining categories that other documents will reference

## When to Avoid
- Storing actual data (use Entity)
- Explaining concepts (use Guide)
- Enforcing rules (use Policy)

## Core Characteristics
- Hierarchical or faceted structure
- Clear parent-child or many-to-many relationships
- Used as a reference by other kinds

## Key Relationships
- `classifies` Entities, Policies, and Domains
- `part_of` larger Domain taxonomies

## Agent Guidance
**Reference this block when:**
- You need to categorize or filter knowledge
- Building smart search or recommendation logic
- A user asks about classification or “types of X”

## Adaptation Notes
Design Taxonomies to be stable but extensible. Use clear, unambiguous category names.