# Dictate Agent: Code Style & Conventions

## Python Style

### Naming Conventions
- **Modules**: `lowercase_with_underscores` (e.g., `audio.py`, `transcribe.py`)
- **Classes**: `PascalCase` (e.g., `DictateAgent`, `AudioCapture`, `Router`)
- **Functions/Methods**: `lowercase_with_underscores` (e.g., `check_all_dependencies()`, `route()`)
- **Constants**: `UPPERCASE_WITH_UNDERSCORES` (e.g., `DEFAULT_MODEL`, `TIMEOUT_SECONDS`)
- **Private methods**: `_lowercase_with_underscores` prefix (e.g., `_parse_config()`)

### Type Hints
- **Required**: All function signatures should include type hints for parameters and return types
- **Format**: Use Python's standard typing module (e.g., `def process(text: str) -> str:`)
- **Complex types**: Use typing.Union, Optional, List, Dict as needed

### Docstrings
- **Format**: Google-style docstrings for classes and public methods
- **Content**: Description, Args, Returns, Raises sections
- **Example**:
  ```python
  def transcribe(audio_data: bytes) -> str:
      """Transcribe audio data using Whisper.
      
      Args:
          audio_data: Raw audio bytes
          
      Returns:
          Transcribed text
          
      Raises:
          ValueError: If audio data is invalid
      """
  ```

### Code Organization
- One class per file (or related classes in same file if tightly coupled)
- Imports organized: stdlib → third-party → local
- Class methods ordered: `__init__` → public methods → private methods
- Max line length: 100 characters (practical limit)

## Project Structure

```
dictate_agent/
├── dictate/              # Main package
│   ├── __init__.py
│   ├── main.py          # Entry point, signal handling
│   ├── audio.py         # AudioCapture class
│   ├── transcribe.py    # Transcription logic
│   ├── router.py        # Router, RouteType classes
│   ├── executor.py      # ClaudeExecutor class
│   ├── local_executor.py # LocalExecutor class
│   ├── timer_executor.py # TimerExecutor class
│   ├── output.py        # Text output functions
│   ├── notify.py        # Notification functions
│   └── config.py        # Configuration loading
├── config/
│   └── config.example.toml  # Example configuration
├── scripts/
│   ├── run.sh
│   ├── dictate-toggle
│   └── dictate-cancel
├── systemd/
│   └── dictate-agent.service
├── tests/               # Test files (mirror structure)
├── pyproject.toml       # Project metadata and dependencies
└── CLAUDE.md           # Development guide
```

## Configuration

### TOML Structure
- **Sections**: `[whisper]`, `[router]`, `[output]`, `[logging]`
- **Types**: strings, booleans, integers, arrays
- **Paths**: Use absolute paths or environment variable expansion where possible

### Environment Variables
- `DICTATE_CONFIG_PATH`: Override config location (default: `~/.config/dictate-agent/config.toml`)
- Other env vars as documented in config.example.toml

## Testing

### Test File Organization
- Test files mirror source structure (e.g., `tests/test_audio.py` for `dictate/audio.py`)
- Use pytest framework
- Test naming: `test_<function>_<scenario>` (e.g., `test_transcribe_valid_audio`)

### Test Pattern
```python
def test_router_routes_simple_question():
    """Test that simple questions route to Haiku."""
    router = Router(use_ollama=False)
    result = router.route("what is Python")
    assert result.model == ModelType.HAIKU
```

## Error Handling

### Exception Types
- **System errors**: Use standard Python exceptions (ValueError, FileNotFoundError, etc.)
- **Domain-specific**: Consider custom exceptions for routing/execution errors
- **Always log**: Use logging module, not print()

### Logging
- Use Python's `logging` module
- Log levels: DEBUG (detailed), INFO (normal), WARNING (issues), ERROR (failures)
- Include context: "Starting daemon", "Transcription complete", etc.

## Signal Handling

### Signals Used
- `SIGUSR1`: Toggle recording
- `SIGUSR2`: Cancel recording
- `SIGINT`: Graceful shutdown
- `SIGTERM`: Graceful shutdown

### Implementation Pattern
```python
signal.signal(signal.SIGUSR1, self._handle_toggle)
signal.signal(signal.SIGUSR2, self._handle_cancel)
```

## Dependencies Management

### Adding Dependencies
1. Add to `pyproject.toml` under `[project] dependencies`
2. Update `pip install -e .`
3. Add system dependency checks to `check_all_dependencies()` if external tools needed

### Pinning Versions
- Pin major versions for stability
- Allow minor/patch updates for bug fixes
- Example: `torch>=2.0,<3.0`

## Documentation Standards

- Inline comments: Only for non-obvious logic
- Docstrings: Required for all public functions/classes
- Type hints: Used throughout for clarity
- README: Maintained in project CLAUDE.md
- Keybindings: Maintained in `~/PC_USE_DOCS/dictate-agent-keybindings.md`

## Git Conventions

- Commit messages: Imperative mood, descriptive (e.g., "Add audio recording timeout")
- Branch naming: `feature/xyz`, `fix/xyz`, `refactor/xyz`
- Keep history clean: squash related commits when appropriate
