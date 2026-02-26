"""Configuration management for Dictate Agent."""

from dataclasses import dataclass, field
from pathlib import Path
from typing import Optional

try:
    import tomllib
except ImportError:
    import tomli as tomllib


CONFIG_DIR = Path.home() / ".config" / "dictate-agent"
CONFIG_FILE = CONFIG_DIR / "config.toml"
PID_FILE = CONFIG_DIR / "dictate.pid"
MEDIA_STATE_FILE = CONFIG_DIR / "media_was_playing"


@dataclass
class WhisperConfig:
    """Whisper transcription configuration."""

    model: str = "openai/whisper-large-v3-turbo"
    assistant_model: str = "distil-whisper/distil-large-v3"
    device: str = "cuda"
    compute_type: str = "float16"
    use_speculative_decoding: bool = True
    chunk_length_s: int = 30
    batch_size: int = 1  # batch_size=1 is best for speculative decoding
    no_speech_threshold: float = 0.6


@dataclass
class RouterConfig:
    """Routing configuration for local Ollama inference."""

    ollama_host: str = "http://localhost:11434"
    ollama_model: str = "qwen3:14b"
    ollama_timeout_s: float = 120.0


@dataclass
class EditorConfig:
    """Text editing mode configuration."""

    enabled: bool = True
    triggers: list[str] = field(
        default_factory=lambda: ["edit:", "fix:", "change:", "rewrite:", "transform:"]
    )


@dataclass
class CommandConfig:
    """Keyboard command configuration."""

    enabled: bool = True
    fuzzy_threshold: float = 0.8
    confirm_destructive: bool = True
    destructive_patterns: list[str] = field(
        default_factory=lambda: ["kill", "close", "exit", "shutdown", "restart"]
    )


@dataclass
class OutputConfig:
    """Output configuration."""

    typing_delay_ms: int = 10
    auto_type: bool = True


@dataclass
class NotificationConfig:
    """Notification configuration."""

    enabled: bool = True
    timeout_ms: int = 3000


@dataclass
class HistoryConfig:
    """Interaction history configuration."""

    enabled: bool = True
    db_path: str = ""  # Empty string means use default XDG path
    max_response_length: int = 10000  # Truncate stored responses beyond this


@dataclass
class StatusWindowConfig:
    """Persistent floating status window configuration."""

    enabled: bool = True
    position: str = "center"  # top-right, top-left, bottom-right, bottom-left, center
    margin: int = 50
    center_offset_y: int = 0


@dataclass
class Config:
    """Main configuration container."""

    whisper: WhisperConfig = field(default_factory=WhisperConfig)
    router: RouterConfig = field(default_factory=RouterConfig)
    editor: EditorConfig = field(default_factory=EditorConfig)
    commands: CommandConfig = field(default_factory=CommandConfig)
    output: OutputConfig = field(default_factory=OutputConfig)
    notifications: NotificationConfig = field(default_factory=NotificationConfig)
    history: HistoryConfig = field(default_factory=HistoryConfig)
    status_window: StatusWindowConfig = field(default_factory=StatusWindowConfig)


def load_config(config_path: Optional[Path] = None) -> Config:
    """Load configuration from TOML file."""
    path = config_path or CONFIG_FILE
    config = Config()

    if not path.exists():
        return config

    with open(path, "rb") as f:
        data = tomllib.load(f)

    # Whisper config
    if "whisper" in data:
        w = data["whisper"]
        config.whisper = WhisperConfig(
            model=w.get("model", config.whisper.model),
            assistant_model=w.get("assistant_model", config.whisper.assistant_model),
            device=w.get("device", config.whisper.device),
            compute_type=w.get("compute_type", config.whisper.compute_type),
            use_speculative_decoding=w.get(
                "use_speculative_decoding", config.whisper.use_speculative_decoding
            ),
            chunk_length_s=w.get("chunk_length_s", config.whisper.chunk_length_s),
            batch_size=w.get("batch_size", config.whisper.batch_size),
            no_speech_threshold=w.get(
                "no_speech_threshold", config.whisper.no_speech_threshold
            ),
        )

    # Router config
    if "router" in data:
        r = data["router"]
        config.router = RouterConfig(
            ollama_host=r.get("ollama_host", config.router.ollama_host),
            ollama_model=r.get("ollama_model", config.router.ollama_model),
            ollama_timeout_s=r.get("ollama_timeout_s", config.router.ollama_timeout_s),
        )

    # Editor config
    if "editor" in data:
        e = data["editor"]
        config.editor = EditorConfig(
            enabled=e.get("enabled", config.editor.enabled),
            triggers=e.get("triggers", config.editor.triggers),
        )

    # Command config
    if "commands" in data:
        c = data["commands"]
        config.commands = CommandConfig(
            enabled=c.get("enabled", config.commands.enabled),
            fuzzy_threshold=c.get("fuzzy_threshold", config.commands.fuzzy_threshold),
            confirm_destructive=c.get(
                "confirm_destructive", config.commands.confirm_destructive
            ),
            destructive_patterns=c.get(
                "destructive_patterns", config.commands.destructive_patterns
            ),
        )

    # Output config
    if "output" in data:
        o = data["output"]
        config.output = OutputConfig(
            typing_delay_ms=o.get("typing_delay_ms", config.output.typing_delay_ms),
            auto_type=o.get("auto_type", config.output.auto_type),
        )

    # Notification config
    if "notifications" in data:
        n = data["notifications"]
        config.notifications = NotificationConfig(
            enabled=n.get("enabled", config.notifications.enabled),
            timeout_ms=n.get("timeout_ms", config.notifications.timeout_ms),
        )

    # History config
    if "history" in data:
        h = data["history"]
        config.history = HistoryConfig(
            enabled=h.get("enabled", config.history.enabled),
            db_path=h.get("db_path", config.history.db_path),
            max_response_length=h.get(
                "max_response_length", config.history.max_response_length
            ),
        )

    # Status window config
    if "status_window" in data:
        sw = data["status_window"]
        config.status_window = StatusWindowConfig(
            enabled=sw.get("enabled", config.status_window.enabled),
            position=sw.get("position", config.status_window.position),
            margin=sw.get("margin", config.status_window.margin),
            center_offset_y=sw.get("center_offset_y", config.status_window.center_offset_y),
        )

    return config
