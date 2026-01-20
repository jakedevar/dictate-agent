---
name: healthy-mind-plan-documenter
description: "Create implementation plan documents for the Healthy Mind project following the established structure in thoughts/shared/plans/. Use when planning new features, refactoring efforts, or multi-step implementations. Handles YAML frontmatter, phased implementation sections, success criteria, and cross-references. Triggers: create plan, implementation plan, plan for, write up the plan."
---

# Healthy Mind Plan Documenter

Create implementation plans following project structure.

## Workflow

1. Gather planning information:
   - Feature/task overview
   - Prerequisites and dependencies
   - Implementation phases
   - Success criteria
   - Risks and mitigations

2. Generate plan with:
   - Standard frontmatter
   - Phased implementation sections
   - Code change specifications
   - Success criteria per phase

3. Save to `thoughts/shared/plans/YYYY-MM-DD-feature-name.md`

## Plan Template

```markdown
---
date: {ISO 8601 timestamp}
author: Claude
topic: "{Feature/Task Name} Implementation Plan"
tags: [plan, implementation, {relevant}, {tags}]
status: draft
---

# {Feature Name} Implementation Plan

## Overview

{Brief description of what we're implementing and why}

## Current State Analysis

{What exists now, what's missing, key constraints}

### Key Discoveries:
- {Finding with file:line reference}

## Desired End State

{Specification of desired state and how to verify}

## What We're NOT Doing

- {Out-of-scope item 1}
- {Out-of-scope item 2}

## Implementation Approach

{High-level strategy and reasoning}

## Phase 1: {Descriptive Name}

### Overview
{What this phase accomplishes}

### Changes Required:

#### 1. {Component/File}
**File**: `path/to/file.ext`
**Changes**: {Summary}

```{language}
// Code changes
```

### Success Criteria:
- [ ] {Testable criterion}
- [ ] {Another criterion}

---

## Phase 2: {Descriptive Name}

{Similar structure...}

---

## Testing Strategy

### Unit Tests:
- {What to test}

### Integration Tests:
- {End-to-end scenarios}

### Manual Testing:
1. {Step to verify}

## Risks and Mitigations

| Risk | Impact | Mitigation |
|------|--------|------------|
| {Risk} | {Impact} | {Mitigation} |

## References

- Related research: `thoughts/shared/research/{file}.md`
- Similar implementation: `{file:line}`
```

## File Naming

`YYYY-MM-DD-feature-name.md`

Examples:
- `2025-11-30-appointment-scheduling.md`
- `2025-11-30-notification-system.md`
