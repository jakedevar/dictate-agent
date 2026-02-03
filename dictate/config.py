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
    """Claude model routing configuration."""

    ollama_host: str = "http://localhost:11434"
    ollama_model: str = "qwen3:0.6b"
    ollama_timeout_s: float = 30.0
    default_model: str = "sonnet"
    short_threshold: int = 20
    long_threshold: int = 100
    haiku_keywords: list[str] = field(
        default_factory=lambda: ["easy", "quick", "haiku"]
    )
    sonnet_keywords: list[str] = field(
        default_factory=lambda: ["medium", "normal", "sonnet"]
    )
    opus_keywords: list[str] = field(
        default_factory=lambda: ["hard", "complex", "difficult", "opus", "analyze"]
    )
    code_terms: list[str] = field(
        default_factory=lambda: [
            "code", "function", "bug", "error", "refactor", "implement", "debug"
        ]
    )


@dataclass
class EditorConfig:
    """Text editing mode configuration."""

    enabled: bool = True
    triggers: list[str] = field(
        default_factory=lambda: ["edit:", "fix:", "change:", "rewrite:", "transform:"]
    )
    model: str = "haiku"


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
class GrammarConfig:
    """Grammar correction configuration."""

    enabled: bool = True
    model: str = "qwen3:0.6b"
    timeout_s: float = 10.0
    min_words: int = 3


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
class Config:
    """Main configuration container."""

    whisper: WhisperConfig = field(default_factory=WhisperConfig)
    router: RouterConfig = field(default_factory=RouterConfig)
    editor: EditorConfig = field(default_factory=EditorConfig)
    commands: CommandConfig = field(default_factory=CommandConfig)
    grammar: GrammarConfig = field(default_factory=GrammarConfig)
    output: OutputConfig = field(default_factory=OutputConfig)
    notifications: NotificationConfig = field(default_factory=NotificationConfig)


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
            default_model=r.get("default_model", config.router.default_model),
            short_threshold=r.get("short_threshold", config.router.short_threshold),
            long_threshold=r.get("long_threshold", config.router.long_threshold),
            haiku_keywords=r.get("haiku_keywords", config.router.haiku_keywords),
            sonnet_keywords=r.get("sonnet_keywords", config.router.sonnet_keywords),
            opus_keywords=r.get("opus_keywords", config.router.opus_keywords),
            code_terms=r.get("code_terms", config.router.code_terms),
        )

    # Editor config
    if "editor" in data:
        e = data["editor"]
        config.editor = EditorConfig(
            enabled=e.get("enabled", config.editor.enabled),
            triggers=e.get("triggers", config.editor.triggers),
            model=e.get("model", config.editor.model),
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

    # Grammar config
    if "grammar" in data:
        g = data["grammar"]
        config.grammar = GrammarConfig(
            enabled=g.get("enabled", config.grammar.enabled),
            model=g.get("model", config.grammar.model),
            timeout_s=g.get("timeout_s", config.grammar.timeout_s),
            min_words=g.get("min_words", config.grammar.min_words),
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

    return config
