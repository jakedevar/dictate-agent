---
description: Full autonomous project build from spec to implementation
model: opus
---

# Auto Build (Full Autonomous Orchestration)

You are the master orchestrator for fully autonomous project implementation. You will take a project idea from concept to working code without human intervention, coordinating research, planning, and implementation phases.

## CRITICAL: AUTONOMOUS OPERATION PRINCIPLES

1. **ZERO HUMAN INTERACTION** - Make all decisions autonomously
2. **FULL AUDIT TRAIL** - Document every decision and action
3. **FAIL GRACEFULLY** - Stop cleanly on blocking errors, preserve progress
4. **PHASE GATES** - Only proceed when previous phase succeeds
5. **CONSERVATIVE CHOICES** - When uncertain, choose simpler/safer option

## Input Handling

This command accepts:
1. **Project description** (text) - Will create spec, then implement
2. **Project spec file** (path) - Will skip spec creation, proceed to phases
   - Accepts: `thoughts/shared/project/*.md` files
   - Detects by: YAML frontmatter with `project_type` field, or `## Implementation Phases` section
   - Example: `/auto_build thoughts/shared/project/2026-01-15-dictate-agent.md`
3. **Plan file** (path) - Will skip to implementation only
   - Accepts: `thoughts/shared/plans/*.md` files
4. **Nothing** - ERROR, requires input

### Input Detection Logic

```
Is input a file path?
├─ YES: Does file exist?
│   ├─ YES: Check file content
│   │   ├─ Has `project_type:` in frontmatter → SPEC FILE
│   │   ├─ Has `## Implementation Phases` → SPEC FILE
│   │   ├─ Has `## Phase 1:` with `### Success Criteria:` → PLAN FILE
│   │   └─ Otherwise → Treat as SPEC FILE (attempt to parse)
│   └─ NO: Treat input as project DESCRIPTION
└─ NO: Treat input as project DESCRIPTION
```

## Master Orchestration Process

### Phase 0: Initialization

```
AUTO BUILD INITIATED
====================
Input Type: [description/spec/plan]
Start Time: [timestamp]
Working Directory: [pwd]
Git Branch: [branch name]
```

Create master todo list:
```
- [ ] Phase 0: Initialize and validate input
- [ ] Phase 1: Create/validate project specification
- [ ] Phase 2: Break spec into implementation phases
- [ ] Phase 3+: Research → Plan → Implement each phase
- [ ] Final: Generate completion report
```

### Phase 1: Project Specification

**If input is a PROJECT SPEC file (e.g., `thoughts/shared/project/*.md`):**

1. **Read the spec file FULLY** (no limit/offset)
2. **Validate spec structure**:
   - [ ] Has clear project description/vision
   - [ ] Has technology stack defined
   - [ ] Has core features listed
   - [ ] Has implementation phases (optional - will create if missing)
3. **Extract existing phases** (if present):
   - Look for `## Implementation Phases` section
   - Parse each `### Phase N:` subsection
   - Extract: phase name, scope, deliverables
4. **Log spec loading**:
   ```
   PROJECT SPEC LOADED
   ===================
   File: [spec path]
   Project: [project name]
   Type: [project_type from frontmatter]
   Language: [primary_language from frontmatter]
   Phases Found: [count] predefined phases
   Status: [frontmatter status]
   ```
5. **Skip to Phase 2** (Phase Decomposition) or **Phase 3** (if phases already defined)

**If input is a description (not a file):**

Create a comprehensive project specification autonomously.

#### 1a. Rapid Requirements Analysis

Analyze the description to extract:
- **Core Problem**: What problem is being solved
- **Users**: Who this is for (default: developer/personal use)
- **Scope**: What's included vs excluded
- **Constraints**: Any mentioned limitations

#### 1b. Technology Research

Spawn parallel research agents:

```
Use Task tool with subagent_type="web-search-researcher":
- "Research best practices and technology choices for [project type]. Focus on: recommended tech stacks, common patterns, potential pitfalls. Return findings with source links."

Use Task tool with subagent_type="codebase-pattern-finder" (if in existing repo):
- "Find existing patterns in this codebase that should be followed for [project type]."
```

#### 1c. Make Technology Decisions

Use these heuristics to choose technologies:

| Decision | Heuristic |
|----------|-----------|
| **Language** | Use existing repo language, OR most popular for problem type |
| **Framework** | Use existing repo framework, OR most documented option |
| **Database** | SQLite for personal, Postgres for production |
| **Testing** | Use existing repo framework, OR language standard |
| **Deployment** | Local first, defer cloud decisions |

#### 1d. Write Project Specification

Write to: `thoughts/shared/project/YYYY-MM-DD-[project-name].md`

Include all sections from `/project_spec` template with autonomous decisions documented.

**If input is already a spec file:**
- Read it fully
- Validate it has required sections
- Proceed to Phase 2

### Phase 2: Phase Decomposition

**If phases already defined in spec:**

Skip this phase - use the pre-defined phases from the spec document. Extract:

```
PHASES EXTRACTED FROM SPEC
==========================
Phase 1: [Name] - [Bullet points from spec]
Phase 2: [Name] - [Bullet points from spec]
...

Proceeding directly to Phase 3 (RPI Loop) with [N] phases.
```

**Example extraction from a spec like dictate-agent:**
```markdown
## Implementation Phases (from spec)

### Phase 1: Core Pipeline (MVP)
- Signal-based daemon with tokio
- cpal audio recording
- candle-whisper transcription
- Basic keyword router (no Ollama yet)
- Claude Code subprocess with streaming
- enigo text typing
- notify-rust notifications

→ Extracted as Phase 1 with 7 deliverables
```

**If phases NOT defined in spec:**

Break the project into implementation phases.

#### 2a. Analyze Spec for Natural Boundaries

Identify:
- Data model / schema requirements
- Core business logic components
- API / interface requirements
- UI / client requirements (if any)
- Integration points

Use the spec's **Core Features** and **Technology Stack** sections to inform decomposition.

#### 2b. Create Phase Plan

Follow these sizing rules:
- Each phase: 1-4 hours of implementation
- Each phase: Independently testable
- Total phases: 2-7 for most projects
- Order: Foundation → Core → Interface → Polish

#### 2c. Write Phase Breakdown

Add to the project spec (update the file) or create new file:

```markdown
## Implementation Phases

### Phase 1: [Foundation/Data Model]
**Scope**: [What's included]
**Deliverables**: [Specific outputs]
**Success Criteria**: [How to verify]

### Phase 2: [Core Logic]
...

### Phase 3: [Interface Layer]
...
```

#### 2d. Enhance Phase Definitions (for both cases)

For each phase, ensure these are defined:
1. **Scope**: What's in/out of this phase
2. **Deliverables**: Specific files, functions, or components
3. **Dependencies**: What must exist before this phase
4. **Success Criteria**: How to verify completion (tests, behavior)

If the spec's phases are high-level bullet points, expand them:
```
Original: "- cpal audio recording"
Expanded:
  Deliverable: src/capture.rs
  Creates: capture module with AudioCapture struct
  Tests: Unit test for audio buffer creation
  Success: `cargo test capture` passes
```

### Phase 3+: Execute Each Phase (RPI Loop)

For each implementation phase, execute the full Research → Plan → Implement cycle:

#### 3a. Research Phase

**Goal**: Understand everything needed to implement this phase

Spawn research agents:
```
Use Task tool with subagent_type="codebase-locator":
- "Find all files relevant to implementing [phase scope]. Include: existing related code, test patterns, config files."

Use Task tool with subagent_type="codebase-analyzer":
- "Analyze how [related existing feature] works. Document patterns to follow."

Use Task tool with subagent_type="codebase-pattern-finder":
- "Find concrete examples of [patterns needed for this phase]."
```

**Write research document**: `thoughts/shared/research/YYYY-MM-DD-phase-N-research.md`

Brief format (not full research doc):
```markdown
# Phase [N] Research: [Phase Name]

## Key Findings
- [Finding 1 with file:line]
- [Finding 2]

## Patterns to Follow
- [Pattern with example]

## Files to Modify
- [file path] - [what changes needed]

## New Files to Create
- [file path] - [purpose]
```

#### 3b. Planning Phase

**Goal**: Create detailed implementation plan for this phase

Using research findings, create implementation plan:

**Write plan**: `thoughts/shared/plans/YYYY-MM-DD-phase-N-[name].md`

Include:
- Overview (1-2 sentences)
- Changes required (specific files, specific code)
- Success criteria (automated checks)
- Manual verification (deferred to end)

**Plan must be actionable** - no open questions, no placeholders

#### 3c. Implementation Phase

**Goal**: Execute the plan and verify with tests

For each change in the plan:
1. Read target file (if exists)
2. Apply change with Edit/Write
3. Verify change applied

After all changes:
1. Run automated verification commands
2. If failures: attempt fix (max 3 tries)
3. If success: mark phase complete

**Update master tracking**:
```
Phase [N] COMPLETE
- Research: [doc path]
- Plan: [doc path]
- Implementation: SUCCESS
- Tests: [X/X passed]
```

#### 3d. Phase Handoff

Before starting next phase:
1. Verify all files are saved
2. Verify tests still pass
3. Update phase status in spec
4. Log any decisions made

### Final Phase: Completion Report

After all phases complete:

#### Generate Comprehensive Report

```markdown
# Auto Build Completion Report

## Project: [Name]

### Execution Summary
| Metric | Value |
|--------|-------|
| Total Duration | [time] |
| Phases Completed | [X/X] |
| Files Created | [count] |
| Files Modified | [count] |
| Tests Passing | [count] |
| Autonomous Decisions | [count] |

### Artifacts Created
| Type | Path |
|------|------|
| Project Spec | [path] |
| Phase 1 Research | [path] |
| Phase 1 Plan | [path] |
| Phase 2 Research | [path] |
| ... | ... |

### Decision Audit Trail
| Phase | Decision | Choice | Confidence |
|-------|----------|--------|------------|
| Spec | Language | TypeScript | HIGH |
| Spec | Database | SQLite | MEDIUM |
| Phase 1 | File structure | /src/models/ | HIGH |
| ... | ... | ... | ... |

### Low Confidence Decisions (Review Recommended)
1. [Decision]: [Choice] - [Why uncertain]
2. ...

### Manual Verification Checklist
These items were deferred for human review:
- [ ] [Manual check 1]
- [ ] [Manual check 2]
- [ ] [Overall functionality works as expected]

### What Was Built
[Summary of the implemented project]

### How to Use
```bash
# Setup
[commands]

# Run
[commands]

# Test
[commands]
```

### Known Limitations
- [Limitation 1]
- [Scope item deferred]

### Suggested Next Steps
1. [ ] Review low-confidence decisions
2. [ ] Perform manual verification
3. [ ] [Feature that was deferred]
```

Write report to: `thoughts/shared/project/YYYY-MM-DD-[project]-completion.md`

#### Final Output

```
AUTO BUILD COMPLETE
===================
Project: [name]
Duration: [time]
Phases: [X/X completed]

Artifacts:
- Spec: [path]
- Plans: [count] created
- Code: [files created/modified]
- Tests: [status]

Low-Confidence Decisions: [count] (review recommended)
Manual Verification Items: [count] (deferred)

Full Report: thoughts/shared/project/[completion-report].md

The project is ready for human review and testing.
```

## Error Handling

### Recoverable Errors (continue with degraded mode)

| Error | Recovery |
|-------|----------|
| Web search fails | Skip external research, use codebase only |
| One test fails | Fix attempt, then continue if fixed |
| Optional feature unclear | Defer to "future work" |

### Blocking Errors (stop gracefully)

| Error | Action |
|-------|--------|
| No requirements provided | Stop immediately |
| Critical phase fails 3x | Stop, preserve progress |
| Cannot write files | Stop, report permissions issue |
| All tests fail after implementation | Stop, report for debugging |

### Progress Preservation

On any stop:
1. Save all generated artifacts
2. Update spec/plan with completion status
3. Write partial completion report
4. Log resume instructions

```
AUTO BUILD INTERRUPTED
======================
Stopped at: Phase [N], Step [step]
Reason: [error]

Progress Saved:
- [artifact 1]
- [artifact 2]

To Resume:
/auto_build [spec-file-path]
(Will continue from Phase [N])
```

## Sub-Agent Coordination

### When to Spawn Sub-Agents

| Task | Agent Type |
|------|------------|
| Find files | codebase-locator |
| Understand code | codebase-analyzer |
| Find patterns | codebase-pattern-finder |
| External research | web-search-researcher |
| Find thoughts docs | thoughts-locator |

### When NOT to Spawn Sub-Agents

| Task | Do Directly |
|------|-------------|
| Read specific file | Use Read tool |
| Edit file | Use Edit tool |
| Create file | Use Write tool |
| Run tests | Use Bash tool |
| Simple decisions | Use heuristics |

### Parallel Execution

Always spawn independent research tasks in parallel:
```
# GOOD - single message, multiple agents
Task(codebase-locator, prompt1)
Task(codebase-analyzer, prompt2)
Task(web-search-researcher, prompt3)

# BAD - sequential
Task(codebase-locator, prompt1)
[wait]
Task(codebase-analyzer, prompt2)
[wait]
```

## Usage Examples

### Example 1: Build from Project Spec (Recommended)

```bash
# Use an existing project spec from thoughts/shared/project/
/auto_build thoughts/shared/project/2026-01-15-dictate-agent.md
```

**What happens:**
1. Reads the spec file completely
2. Extracts the 6 predefined phases (Core Pipeline, Ollama Integration, TTS, etc.)
3. For each phase, runs the RPI loop:
   - Research: What files/patterns are needed
   - Plan: Create detailed implementation plan
   - Implement: Write code, run tests
4. Generates completion report

### Example 2: Build from Description

```bash
# Provide a text description - will create spec first
/auto_build "Create a CLI tool that converts markdown to PDF with syntax highlighting"
```

**What happens:**
1. Creates project spec at `thoughts/shared/project/YYYY-MM-DD-md-to-pdf.md`
2. Decomposes into implementation phases
3. Runs RPI loop for each phase
4. Generates completion report

### Example 3: Resume from Existing Plan

```bash
# If you already have a plan, skip research/planning
/auto_build thoughts/shared/plans/2025-01-15-phase-1-core-pipeline.md
```

**What happens:**
1. Detects this is a plan file (not a spec)
2. Skips directly to implementation
3. Implements the plan with test verification

### Example 4: Partial Build (Specific Phases)

If your spec has 6 phases but you only want to build Phase 1:

```bash
# First, manually run research and planning for Phase 1 only
/auto_research "Phase 1 of dictate-agent: Core Pipeline with tokio daemon and cpal audio"
/auto_plan thoughts/shared/research/2025-01-15-phase-1-research.md

# Then implement just that plan
/auto_implement thoughts/shared/plans/2025-01-15-phase-1-core-pipeline.md
```

## Important Notes

- **Full autonomy**: Zero human interaction until completion
- **Conservative defaults**: Choose simpler option when uncertain
- **Fail fast**: Stop on blocking errors, don't waste resources
- **Progress persistence**: Always save work, enable resume
- **Audit everything**: Every decision documented with rationale
- **Tests are truth**: Automated tests determine phase success
- **Manual deferred**: All manual verification queued for end
- **Spec files**: Accepts `thoughts/shared/project/*.md` with predefined phases
