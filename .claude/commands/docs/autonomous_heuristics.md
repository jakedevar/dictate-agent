# Autonomous Decision Heuristics

This document defines the decision-making rules used by the autonomous RPI workflow commands (`/auto_research`, `/auto_plan`, `/auto_implement`, `/auto_build`).

## Philosophy

When operating autonomously, the agent must make decisions that would normally require human judgment. These heuristics encode conservative, reasonable defaults that:

1. **Minimize risk** - Choose options less likely to cause problems
2. **Maximize compatibility** - Follow existing patterns
3. **Enable recovery** - Make reversible decisions when possible
4. **Document uncertainty** - Flag low-confidence choices for review

---

## Technology Selection Heuristics

### Programming Language

| Context | Choice | Rationale |
|---------|--------|-----------|
| Existing repo (>70% one language) | Use dominant language | Consistency |
| New repo, web backend | TypeScript/Node or Python | Most documented |
| New repo, CLI tool | Rust or Go | Performance + single binary |
| New repo, scripts | Python | Simplicity |
| New repo, systems | Rust | Safety + performance |

### Framework Selection

| Project Type | Framework | Rationale |
|--------------|-----------|-----------|
| Web frontend | React or existing | Most resources |
| Web backend | Express/Fastify or existing | Most documented |
| API only | Hono or Fastify | Lightweight |
| Full-stack | Next.js or existing | Integrated |
| Desktop app | Tauri or existing | Modern + lightweight |

### Database Selection

| Context | Choice | Rationale |
|---------|--------|-----------|
| Personal/local tool | SQLite | Zero config |
| Existing repo | Use existing DB | Consistency |
| Production web app | PostgreSQL | Reliability |
| High performance needs | PostgreSQL | Proven scale |
| Simple key-value | SQLite or file-based | Simplicity |

### Testing Framework

| Language | Framework | Rationale |
|----------|-----------|-----------|
| TypeScript/JS | Vitest or existing | Fast, modern |
| Python | pytest or existing | Standard |
| Rust | built-in + existing | Integrated |
| Go | built-in + existing | Integrated |

---

## Architecture Decision Heuristics

### New Component vs Extend Existing

| Condition | Decision | Rationale |
|-----------|----------|-----------|
| >60% overlap with existing | Extend existing | Reduce duplication |
| <30% overlap | Create new | Separation of concerns |
| 30-60% overlap | Create new, extract common | Balance |
| Existing is complex/fragile | Create new | Avoid risk |

### File Organization

| Decision Point | Heuristic |
|----------------|-----------|
| Where to put new code | Mirror existing structure |
| New directory needed | Only if 3+ related files |
| Config file location | Root or existing config dir |
| Test file location | Near source or existing test dir |

### Abstraction Level

| Signal | Decision |
|--------|----------|
| Similar features are simple | Keep new feature simple |
| Similar features are abstracted | Follow abstraction pattern |
| No similar features | Start simple, refactor if needed |
| Requirement mentions reuse | Build for reuse from start |

### Error Handling

| Context | Approach |
|---------|----------|
| Existing patterns present | Follow existing patterns |
| User-facing operation | Detailed error messages |
| Internal operation | Log + propagate |
| External API call | Retry with backoff |

---

## Scope Decision Heuristics

### Ambiguous Requirements

| Ambiguity | Resolution | Example |
|-----------|------------|---------|
| "Users" unspecified | Assume single user | Personal tool default |
| "Notifications" unspecified | In-app only | No email/SMS |
| "Authentication" unspecified | Session-based | Not OAuth |
| "Performance" unspecified | Reasonable defaults | Optimize later |
| Scale unspecified | Single machine | Scale later |

### Feature Inclusion

| Signal | Include? | Rationale |
|--------|----------|-----------|
| Explicitly required | Yes | Direct requirement |
| "Nice to have" mentioned | No, defer | Scope control |
| Implied by core feature | Yes | Necessary for function |
| Common in similar apps | No, defer | Not explicitly needed |
| Security-related | Yes | Non-negotiable |

### Edge Cases

| Rule | Application |
|------|-------------|
| Handle top 3 common cases | 80/20 rule |
| Log unusual cases | Visibility for debugging |
| Fail gracefully on unknowns | Don't crash |
| Document unhandled cases | Transparency |

---

## Implementation Decision Heuristics

### Code Style

| Aspect | Heuristic |
|--------|-----------|
| Formatting | Use existing formatter config |
| Naming | Match existing conventions |
| Comments | Only when logic non-obvious |
| Types | As strict as existing code |

### Dependencies

| Decision | Heuristic |
|----------|-----------|
| Add new dependency | Only if significant value |
| Choose between options | More downloads + recent updates |
| Version pinning | Follow existing strategy |
| Dev vs prod dependency | Strict separation |

### Testing

| Aspect | Heuristic |
|--------|-----------|
| Test coverage | Match existing coverage level |
| Test style | Match existing test patterns |
| What to test | Public interfaces + edge cases |
| What to skip | Implementation details |

---

## Confidence Classification

### High Confidence (proceed without review)

- Decision matches explicit requirement
- Decision follows clear codebase pattern
- Decision is industry standard for context
- Decision is easily reversible

### Medium Confidence (review optional)

- Decision based on reasonable inference
- Multiple valid alternatives exist
- Following most common pattern
- Some uncertainty in requirements

### Low Confidence (review recommended)

- Decision is best guess
- Significant alternatives with different tradeoffs
- Requirement is ambiguous
- Decision is hard to reverse
- High impact if wrong

---

## Error Handling & Recovery

### Recoverable vs Blocking

| Error Type | Classification | Action |
|------------|----------------|--------|
| Test failure | Recoverable | Fix attempt (max 3) |
| Lint error | Recoverable | Auto-fix + retry |
| Type error | Recoverable | Fix + retry |
| File not found | Depends | Create if expected, else block |
| Permission denied | Blocking | Stop + report |
| Dependency install fail | Blocking | Stop + report |
| All tests fail | Blocking | Stop + report |

### Fix Attempt Rules

1. **First attempt**: Direct fix based on error message
2. **Second attempt**: Research similar patterns in codebase
3. **Third attempt**: Simplify approach, reduce scope
4. **After third failure**: Stop, report, preserve progress

---

## Customization

To customize these heuristics for your project:

1. Create `.claude/autonomous_config.md` with overrides
2. Reference specific sections to override
3. Agent will read this before making decisions

Example override file:
```markdown
# Autonomous Config Overrides

## Technology Overrides
- Always use PostgreSQL (never SQLite)
- Prefer Bun over Node.js
- Use Playwright for E2E tests

## Scope Overrides
- Always include basic auth
- Always include error tracking
- Defer all performance optimization

## Confidence Overrides
- Treat all database schema decisions as LOW confidence
- Treat all API design decisions as LOW confidence
```

---

## Revision History

| Date | Change |
|------|--------|
| 2025-01-15 | Initial heuristics document |
