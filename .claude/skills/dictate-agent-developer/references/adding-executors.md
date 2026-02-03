# Adding a New Executor

Executors handle the actual work for a route type. Follow this pattern.

## Result Dataclass

Every executor defines its own result type:

```python
from dataclasses import dataclass
from typing import Optional

@dataclass
class NewResult:
    """Result from new execution."""
    success: bool
    response: str
    error: Optional[str] = None
```

## Executor Class

```python
@dataclass  # or regular class
class NewExecutor:
    """Description of what this executor does."""

    param: str = "default_value"

    def execute(self, prompt: str) -> NewResult:
        """Execute the task. Never raises — returns result object."""
        try:
            # Do work here (subprocess, API call, etc.)
            ...
            return NewResult(success=True, response=output)

        except subprocess.TimeoutExpired:
            return NewResult(success=False, response="", error="Timed out")
        except FileNotFoundError:
            return NewResult(
                success=False,
                response="",
                error="tool_name not found. Install with: <instructions>",
            )
        except Exception as e:
            return NewResult(success=False, response="", error=str(e))
```

### Key conventions:
- **Never raise exceptions** — catch everything, return result objects
- **Include helpful error messages** — installation instructions, "is it running?" hints
- **Print status** — `print(f"Executing: {prompt[:50]}...")` for daemon log
- **Print response preview** — `print(f"Response ({len(response)} chars): {response[:100]}...")`
- **Use timeouts** on all subprocess calls

## Subprocess Pattern (if calling external tool)

```python
result = subprocess.run(
    ["tool", "--flag", "value", prompt],
    capture_output=True,
    text=True,
    timeout=30,
)

if result.returncode != 0:
    error_msg = result.stderr.strip() or f"tool exited with code {result.returncode}"
    return NewResult(success=False, response="", error=error_msg)

return NewResult(success=True, response=result.stdout.strip())
```

## Dependency Check Function

Export at module level:

```python
def check_new_dependencies() -> list[tuple[str, str]]:
    """Check that dependencies are available. Returns list of (cmd, package) missing."""
    missing = []
    for cmd, pkg in [("tool_name", "package-name")]:
        result = subprocess.run(["which", cmd], capture_output=True)
        if result.returncode != 0:
            missing.append((cmd, pkg))
    return missing
```

## Integration in main.py

1. Import at top of `main.py`
2. Initialize in `DictateAgent.__init__()` with config values
3. Add handler in `_handle_route()`
4. Add dependency check to `check_all_dependencies()`

## Existing Executors as Reference

| Executor | External Tool | Timeout | Result Type |
|----------|--------------|---------|-------------|
| `ClaudeExecutor` | `claude` CLI | 120s | `ExecutionResult` |
| `LocalExecutor` | `ollama` Python client | 30s | `ExecutionResult` |
| `TimerExecutor` | `systemd-run` | 5s | `TimerResult` |
