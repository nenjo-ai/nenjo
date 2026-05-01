# Domain — Vertical & Industry Knowledge

## Purpose
A Domain document defines an entire vertical, industry, or functional area (e.g., Regulatory Compliance, Legal Contracts, Financial Risk, Healthcare Operations). It provides the high-level context and boundaries for all other knowledge in that space.

## When to Use
- Introducing a new vertical or industry focus
- Setting the scope for a suite of related Entities, Policies, and Taxonomies
- Creating specialized knowledge packs for clients

## When to Avoid
- Detailed data modeling (use Entity)
- Specific rules (use Policy)
- General platform explanations (use Guide)

## Core Characteristics
- Broad scope covering an entire problem space
- Defines key concepts, stakeholders, and success criteria for the domain
- Acts as the “home” for related lower-level documents

## Key Relationships
- `part_of` larger enterprise domains
- `governs` Entities and Policies within the domain
- `references` Guides and Taxonomies

## Agent Guidance
**Reference this block when:**
- Starting work in a new vertical or client industry
- A user says “Build me a compliance engine” or “Create a legal AI system”
- You need to understand the overall context before diving deeper

## Adaptation Notes
Every major vertical Nenjo supports should have exactly one primary Domain document.