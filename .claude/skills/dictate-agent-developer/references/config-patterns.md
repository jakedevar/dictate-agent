# Configuration Patterns

## Adding a New Config Section

### Step 1: Add dataclass to `config.py`

Place after existing config dataclasses (before the top-level `Config` class):

```python
@dataclass
class NewFeatureConfig:
    """Description of configuration section."""
    enabled: bool = True
    some_param: str = "default_value"
    threshold: float = 0.8
    items: list[str] = field(default_factory=lambda: ["item1", "item2"])
```

All fields must have defaults — the config system is graceful-fallback by design.

### Step 2: Add to top-level `Config` dataclass (config.py:98-107)

```python
@dataclass
class Config:
    whisper: WhisperConfig = field(default_factory=WhisperConfig)
    router: RouterConfig = field(default_factory=RouterConfig)
    editor: EditorConfig = field(default_factory=EditorConfig)
    commands: CommandConfig = field(default_factory=CommandConfig)
    output: OutputConfig = field(default_factory=OutputConfig)
    notifications: NotificationConfig = field(default_factory=NotificationConfig)
    new_feature: NewFeatureConfig = field(default_factory=NewFeatureConfig)  # ← Add
```

### Step 3: Add TOML loading block in `load_config()` (config.py:110-194)

Follow the exact pattern used by existing sections:

```python
# New feature config
if "new_feature" in data:
    nf = data["new_feature"]
    config.new_feature = NewFeatureConfig(
        enabled=nf.get("enabled", config.new_feature.enabled),
        some_param=nf.get("some_param", config.new_feature.some_param),
        threshold=nf.get("threshold", config.new_feature.threshold),
        items=nf.get("items", config.new_feature.items),
    )
```

**Critical**: Always use `.get(key, default_from_dataclass)` — never raw dict access.

### Step 4: Add section to `config/config.example.toml`

```toml
[new_feature]
# Whether to enable the new feature
enabled = true
# Description of this parameter
some_param = "default_value"
# Threshold for something (0.0-1.0)
threshold = 0.8
# List of items
items = ["item1", "item2"]
```

Include comments explaining each setting.

### Step 5: Pass to component in `main.py`

Config values are passed through constructors, **not** accessed globally:

```python
# In DictateAgent.__init__():
self.new_thing = NewThing(
    enabled=self.config.new_feature.enabled,
    param=self.config.new_feature.some_param,
)
```

## Existing Config Sections

| Section | Dataclass | Used By |
|---------|-----------|---------|
| `[whisper]` | `WhisperConfig` | `Transcriber` |
| `[router]` | `RouterConfig` | `Router`, `ClaudeExecutor`, `LocalExecutor` |
| `[editor]` | `EditorConfig` | (not yet wired) |
| `[commands]` | `CommandConfig` | (not yet wired) |
| `[output]` | `OutputConfig` | `OutputHandler` |
| `[notifications]` | `NotificationConfig` | `Notifier` |

## Config File Locations

```python
CONFIG_DIR = Path.home() / ".config" / "dictate-agent"
CONFIG_FILE = CONFIG_DIR / "config.toml"
PID_FILE = CONFIG_DIR / "dictate.pid"
MEDIA_STATE_FILE = CONFIG_DIR / "media_was_playing"
```
