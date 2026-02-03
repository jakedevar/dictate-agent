# Adding a New Route Type

Adding a route type requires changes in exactly **3 files**. Follow this checklist.

## File 1: `dictate/router.py`

### Add enum value (router.py:19-29)

```python
class RouteType(Enum):
    LOCAL = "local"
    HAIKU = "haiku"
    SONNET = "sonnet"
    OPUS = "opus"
    EDIT = "edit"
    COMMAND = "command"
    TIMER = "timer"
    TYPE = "type"
    NEW_TYPE = "new_type"  # ← Add here
```

### Add detection logic (router.py:48-90)

The `route()` method checks triggers in this order:
1. Edit triggers (prefix-based: "edit:", "fix:", etc.)
2. First-word triggers ("timer", "simple", "easy", "medium", "hard")
3. Default (TYPE — just type the text)

Add detection at the appropriate priority level:

```python
# For prefix-based triggers (like EDIT):
if text_lower.startswith("your_prefix:"):
    return RouteResult(
        route=RouteType.NEW_TYPE,
        model="",
        text=text[len("your_prefix:"):].strip(),
        confidence=1.0,
    )

# For first-word triggers (like TIMER, LOCAL):
if first_word == "your_trigger":
    return RouteResult(
        route=RouteType.NEW_TYPE,
        model="",
        text=rest,
        confidence=1.0,
    )
```

## File 2: `dictate/main.py`

### Add handler in `_handle_route()` (main.py:173-233)

Add a new case block following the existing pattern:

```python
elif route_result.route == RouteType.NEW_TYPE:
    # Show processing notification
    self.notifier.processing("new_type")

    # Execute (use existing executor or create new one)
    result = self.new_executor.execute(text)

    if result.success:
        self.output.type_text(result.response)
        self.notifier.done(result.response)
    else:
        self.notifier.error(result.error or "Unknown error")
```

### If new executor needed, add to `__init__()` (main.py:41-74)

```python
self.new_executor = NewExecutor(
    param=self.config.new_section.param,
)
```

## File 3: `dictate/config.py` (if configurable)

Only needed if the new route type has user-configurable settings. See [config-patterns.md](config-patterns.md).

## Verification Checklist

- [ ] Enum value added to `RouteType`
- [ ] Detection logic added to `Router.route()` at correct priority
- [ ] Handler added to `_handle_route()` with notification flow
- [ ] Executor initialized in `DictateAgent.__init__()` (if new)
- [ ] Config dataclass added (if configurable)
- [ ] `check_*_dependencies()` exported (if new external tools)
- [ ] Dependency check added to `main.py:check_all_dependencies()`
