# Dictate Agent: Local Voice Assistant & Intelligent Dictation Daemon

The **Dictate Agent** is a lightweight, local-first voice dictation and task execution system designed for Arch Linux. Leveraging the power of a dedicated Nvidia RTX 5080, it runs GPU-accelerated Whisper model inference for speech-to-text and a local Ollama server for inline grammar correction and LLM task execution.

The daemon acts as an orchestrator, receiving signals from the window manager to toggle audio recording, processing the captured audio through an AI pipeline, and outputting the result either directly as keystrokes (typing into the active window) or executing local system commands.

---

## System Architecture

The following diagram illustrates the lifecycle of a single dictation toggle. When the user hits the hotkey, audio is captured, processed, routed, and dispatched:

```mermaid
graph TD
    A[User Hotkey] -->|dictate-toggle SIGUSR1| B[DictateAgent]
    B -->|Pause Media| C[playerctl]
    B -->|Capture 16kHz Audio| D[AudioCapture]
    A -->|dictate-cancel SIGUSR2| E[Cancel & Discard]
    
    D -->|f32 samples| F[Transcriber]
    F -->|Whisper.cpp CUDA| G[Raw Text]
    G -->|Scrub, protect, corrections, rules| H[TextChain Rules Output]
    
    H -->|Prefix Matching| K[Router]
    
    K -->|Default Route| I[LlmFormatter]
    I -->|Ollama model ladder, fail-open, protected spans verified| L[Type Agent]
    K -->|'timer' prefix| M[Timer Agent]
    K -->|'easy/simple...' prefix| N[Local LLM Agent]
    K -->|'edit/fix...' prefix| O[Edit Agent - Planned]
    
    L -->|Ctrl+V Clipboard Paste| P[Active Window]
    M -->|systemd-run| Q[Dunst Notify + Sox Alarm]
    N -->|Ollama qwen3:14b| R[Type Response]
    
    B -->|Commit Telemetry| S[HistoryStore SQLite]
    B -->|Resume Media| T[playerctl play]
```

---

## Core Agents & Pipeline Components

### 1. The Orchestrator Daemon: [DictateAgent](file:///home/jakedevar/dictate_agent/crates/dictate-core/src/agent.rs#L18)
The daemon binds to POSIX signals to handle recording toggles and cancellations asynchronously:
* **`SIGUSR1` (Toggle)**: Handled by [dictate-toggle](file:///home/jakedevar/dictate_agent/scripts/dictate-toggle). Starts audio capture, pauses media if playing (via `playerctl`), and launches the processing pipeline on a subsequent toggle.
* **`SIGUSR2` (Cancel)**: Handled by [dictate-cancel](file:///home/jakedevar/dictate_agent/scripts/dictate-cancel). Immediately stops the input stream, purges buffers, discards captured audio, and resumes system media.

> [!NOTE]
> Media states are tracked via a temporary state file (`~/.config/dictate-agent/media_state`). This ensures that if the daemon is interrupted or crashes, it cleans up stale states without leaving media players paused permanently.

### 2. Audio Capture Engine: [AudioCapture](file:///home/jakedevar/dictate_agent/crates/dictate-audio/src/lib.rs#L7)
Captures raw audio streams using `cpal` from the default ALSA/PipeWire input.
* Requests **16kHz mono** audio format.
* Prefers `f32` sample buffers, falling back to `i16` with float conversion if required by hardware.
* Implements a **500ms trailing delay** upon stopping to ensure final words are not clipped during transcribing.

### 3. Speech-to-Text Pipeline: [Transcriber](file:///home/jakedevar/dictate_agent/crates/dictate-stt/src/transcribe.rs#L21)
Runs local Whisper inference using GGUF model configurations.
* **Hardware Acceleration**: Configured for CUDA execution, running Whisper parameters on the Nvidia RTX 5080.
* **Model**: Typically uses `ggml-large-v3-turbo.bin` (see [config.example.toml](file:///home/jakedevar/dictate_agent/config/config.example.toml#L8)).
* **Raw output**: Returns Whisper's text untouched; corrections and cleanup belong to the text chain below.

### 3b. Deterministic Text Chain: [TextChain](file:///home/jakedevar/dictate_agent/crates/dictate-fmt/src/text/mod.rs)
Pure-Rust rules between STT and the router, timed as `fmt_rules` and configured under `[format]` / `[format.rules]`.
* **Protected spans**: URLs, emails, paths, slash commands, and code identifiers become opaque placeholders no later stage can alter; LLM output that drops or edits one is rejected.
* **Rules**: hallucination scrub, the historical acoustic corrections ("cloud" → "Claude", "create plan" → `/create_plan`), fillers, stutters, numbers, spacing, casing, terminal punctuation; spoken punctuation and line breaks are opt-in.
* **Plug-in slots**: the dictionary (S22) and snippets (S24) run inside the chain as `TextStage`s.

### 4. LLM Formatting Pass: [LlmFormatter](file:///home/jakedevar/dictate_agent/crates/dictate-fmt/src/llm/mod.rs)
An optional pass (S21) that runs after routing, for `type` utterances only, on the text chain's output. Configured under `[format.llm]`; the deprecated `[grammar]` keys are read as an alias. The daemon wraps it as `LlmFormatterPort` (`crates/dictate-core/src/llm_formatter.rs`).
* Resolves a model ladder (default `gemma4:e4b` → `gemma4:12b`) against the installed Ollama models, and warms the model when recording starts.
* The prompt follows the destination app's category and tone: terminals and editors stay verbatim (fillers, false starts, self-corrections and punctuation only); chat, email and documents get light grammar and, where allowed, structure.
* Protected spans are masked before the model sees the text and restored afterwards; a validator rejects answers, drift, dropped content and broken spans.
* **Fail-Open Strategy**: if Ollama is down, the model is missing, the call times out, or the validator rejects the output, the rules output is typed unchanged and `status.formatter` reports the health.

### 5. Routing & Dispatch: [router](file:///home/jakedevar/dictate_agent/crates/dictate-core/src/router.rs#L2)
Inspects the transcribed and corrected text to decide how to respond. The system matches prefixes and dispatches to the corresponding execution agents:

| Trigger Prefix | Route Type | Destination Agent | Description |
|---|---|---|---|
| None (Default) | `Type` | [OutputHandler](file:///home/jakedevar/dictate_agent/crates/dictate-inject/src/output.rs#L6) | Paste text directly into the focused window. |
| `timer` | `Timer` | [TimerExecutor](file:///home/jakedevar/dictate_agent/crates/dictate-core/src/timer.rs#L289) | Schedules a systemd user timer. |
| `easy`, `simple`, `medium`, `hard` | `Local` | [LocalExecutor](file:///home/jakedevar/dictate_agent/crates/dictate-core/src/local_executor.rs#L14) | Executes prompt with local Ollama LLM and types result. |
| `edit:`, `fix:`, `change:`, `rewrite:` | `Edit` | *Planned* | Structural text transformations. |

---

## Action Execution Agents

### Output Agent: [OutputHandler](file:///home/jakedevar/dictate_agent/crates/dictate-inject/src/output.rs#L6)
Responsible for typing generated or transcribed text. Rather than typing key-by-key (which is slow and error-prone), the Output Agent uses a clipboard-paste approach:
1. Temporarily copies and backs up the user's active clipboard contents.
2. Writes the target text to the system clipboard.
3. Uses `enigo` to simulate a `Ctrl+V` key combination to paste instantly.
4. Waits `50ms` for the OS to register the paste action.
5. Restores the user's original clipboard contents.

### Local LLM Agent: [LocalExecutor](file:///home/jakedevar/dictate_agent/crates/dictate-core/src/local_executor.rs#L14)
Provides local AI generation on-demand.
* Triggered when a phrase begins with a difficulty level (e.g., "easy explain quantum computing").
* Dispatches the query directly to Ollama running a larger model (e.g., `qwen3:14b` on the RTX 5080).
* Feeds the text response back to the Output Handler to automatically paste the model's answer.
* If Ollama is not active, it will automatically attempt to spawn it (`ollama serve`) and poll for up to 10 seconds.

### Timer Agent: [TimerExecutor](file:///home/jakedevar/dictate_agent/crates/dictate-core/src/timer.rs#L289)
Manages localized natural language timers.
* Parsed durations support both digit strings and words (e.g. "two hours and a half", "15 minutes", "half an hour").
* Schedules a transient user systemd service using `systemd-run --user --on-active=<duration>`.
* Once the timer fires, it runs a shell script that plays an alarm sound (using `play` from sox) and triggers a critical desktop notification (via `dunstify`) that stays open until dismissed.

---

## History & Telemetry Store: [HistoryStore](file:///home/jakedevar/dictate_agent/crates/dictate-history/src/history.rs#L11)
Telemetry is written to a local SQLite database (`~/.local/share/dictate-agent/history.db`).
Every voice interaction creates a structured record mapping:
* **Durations**: Audio capture duration, transcription latency, grammar correction latency, and LLM processing duration.
* **Changes**: Grammar corrections applied, input/output diff, and routing classification details.
* **Status**: Execution success indicators, error summaries, and outputs typed.

> [!TIP]
> The database runs with Write-Ahead Logging (`PRAGMA journal_mode=WAL`) enabled, allowing concurrent reads/writes without locking the main thread during high-frequency speech sessions.
