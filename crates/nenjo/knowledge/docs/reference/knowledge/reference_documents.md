# Reference — Factual & Lookup Knowledge

## Purpose
A Reference contains factual information, constants, lookup tables, dependency orders, glossaries, or quick-reference material. It is the “source of truth” for facts that agents need to recall accurately.

## When to Use
- Dependency ordering (e.g., resource-dependency-order)
- Glossaries and terminology
- Constants, enums, or configuration values
- Quick facts or decision tables

## When to Avoid
- Explaining concepts (use Guide)
- Defining data models (use Entity)
- Stating rules (use Policy)

## Core Characteristics
- Structured, tabular, or list-based format
- Minimal narrative
- High precision and stability

## Key Relationships
- `defines` Entities
- `references` Guides and Domains
- `classifies` items via Taxonomies

## Agent Guidance
**Reference this block when:**
- You need an authoritative fact, order, or list
- Validating configuration or build order
- Answering “What are the possible values of X?”

## Adaptation Notes
Keep References extremely stable. Changes should be rare and well-communicated.