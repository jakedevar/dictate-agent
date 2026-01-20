---
name: healthy-mind-research-documenter
description: "Create research documents for the Healthy Mind project following the established YAML frontmatter and section structure used in thoughts/shared/research/. Use when documenting research findings, investigating issues, or analyzing codebase patterns. Handles frontmatter generation, standard sections, git metadata, and cross-references. Triggers: document research, create research document, write up findings, document investigation."
---

# Healthy Mind Research Documenter

Create research documents following the project's established structure.

## Workflow

1. Gather information:
   - Research topic/question
   - Key findings to document
   - Code references discovered
   - Related research documents

2. Generate document with:
   - YAML frontmatter with git metadata
   - Standard section structure
   - File:line references for code
   - Links to related research

3. Save to `thoughts/shared/research/YYYY-MM-DD-topic-slug.md`

## Document Template

```markdown
---
date: {ISO 8601 timestamp}
researcher: Claude
git_commit: {current commit hash}
branch: {current branch}
repository: healthy_mind
topic: "{Research Topic}"
tags: [research, {relevant}, {tags}]
status: complete
last_updated: {YYYY-MM-DD}
last_updated_by: Claude
---

# Research: {Topic Title}

**Date**: {ISO 8601 timestamp}
**Researcher**: Claude
**Git Commit**: {commit hash}
**Branch**: {branch name}

## Research Question

{The specific question or problem being investigated}

## Summary

{1-2 paragraph overview of key findings}

## Detailed Findings

### {Finding Category 1}

{Detailed analysis with code references}

**Found in**: `file/path.rs:line-range`
```rust
// Relevant code snippet
```

### {Finding Category 2}

{Continue pattern...}

## Code References

- `file1.rs:100-150` - {What this code does}
- `file2.rs:50-75` - {What this code does}

## Architecture Documentation

{If applicable, document architectural patterns discovered}

## Historical Context

{Reference to related thoughts/ documents or git history}

## Related Research

- `thoughts/shared/research/related-doc.md` - {Relevance}

## Open Questions

1. {Unanswered question for future investigation}
```

## Getting Git Metadata

Use these bash commands:
- Commit hash: `git rev-parse HEAD`
- Branch: `git branch --show-current`
- Date: `date -Iseconds`

## File Naming Convention

`YYYY-MM-DD-descriptive-slug.md`

Examples:
- `2025-11-30-session-persistence-analysis.md`
- `2025-11-30-form-validation-patterns.md`
