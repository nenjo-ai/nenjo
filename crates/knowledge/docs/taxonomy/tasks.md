# Priority & Urgency Framework

## Purpose
A unified classification system for evaluating work based on **importance** and **time sensitivity**. This taxonomy helps agents, routines, and humans make consistent prioritization decisions across projects.

## Core Dimensions

### 1. Priority (Importance)

| Level     | Definition                                      | Typical Characteristics                     |
|-----------|--------------------------------------------------|---------------------------------------------|
| **Critical** | Must be addressed immediately; business impact is severe | Revenue loss, security breach, legal risk, major outage |
| **High**     | Significant impact if delayed                    | Key milestone, customer-facing, blocks other work |
| **Medium**   | Important but not urgent                         | Standard feature work, improvements         |
| **Low**      | Nice to have, minimal immediate impact           | Polish, experiments, low-risk changes       |

### 2. Urgency (Time Sensitivity)

| Level     | Definition                                      | Time Horizon          |
|-----------|--------------------------------------------------|-----------------------|
| **Immediate** | Must be done within hours                        | < 24 hours            |
| **Soon**      | Must be done within days                         | 1–7 days              |
| **Planned**   | Should be done within weeks                      | 1–4 weeks             |
| **Backlog**   | Can be done whenever capacity exists             | 1+ months             |

## Combined Classification Matrix

| Priority   | Immediate     | Soon          | Planned       | Backlog      |
|------------|---------------|---------------|---------------|--------------|
| **Critical** | P0 – Emergency | P1 – Urgent   | P2 – Important| P3 – Monitor |
| **High**     | P1 – Urgent   | P2 – Important| P3 – Normal   | P4 – Low     |
| **Medium**   | P2 – Important| P3 – Normal   | P4 – Low      | P5 – Future  |
| **Low**      | P3 – Normal   | P4 – Low      | P5 – Future   | P6 – Icebox  |

## Usage in Nenjo

- Used by agents and routines for intelligent task ordering
- Influences `order_index` and execution scheduling
- Can be combined with `complexity` for effort-based planning
- Supports risk-based prioritization in compliance or production projects

## Key Relationships

- `classifies` → Task Entity (`priority` field)
- `governs` → Execution dispatch order and parallelization decisions
- `references` → Guides and Policies that reference prioritization logic

## Notes
This taxonomy can be extended with project-specific dimensions (e.g., Customer Impact, Technical Debt, Regulatory Deadline).