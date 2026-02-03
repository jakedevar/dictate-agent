#!/usr/bin/env python3
"""
Dictate Agent - Ultimate voice dictation with Claude integration.

A signal-based daemon that:
1. Records audio via parecord
2. Transcribes with optimized Whisper (large-v3-turbo + speculative decoding)
3. Routes to appropriate Claude model
4. Types response or executes commands
"""

import argparse
import os
import signal
import subprocess
import sys
import time
from pathlib import Path

from . import __version__
from .audio import AudioCapture, check_audio_dependencies
from .config import (
    CONFIG_DIR,
    CONFIG_FILE,
    MEDIA_STATE_FILE,
    PID_FILE,
    load_config,
)
from .executor import ClaudeExecutor, check_executor_dependencies
from .grammar import GrammarCorrector
from .local_executor import LocalExecutor, check_local_dependencies, ensure_ollama_running
from .notify import Notifier, check_notify_dependencies
from .output import OutputHandler, check_output_dependencies
from .router import RouteType, Router
from .timer_executor import TimerExecutor
from .transcribe import Transcriber, check_transcription_dependencies


class DictateAgent:
    """Main dictation agent daemon."""

    def __init__(self):
        self.config = load_config()
        self.running = True
        self.recording = False

        # Initialize components
        self.notifier = Notifier(
            enabled=self.config.notifications.enabled,
            default_timeout_ms=self.config.notifications.timeout_ms,
        )
        self.audio = AudioCapture()
        self.output = OutputHandler(
            typing_delay_ms=self.config.output.typing_delay_ms,
            auto_type=self.config.output.auto_type,
        )
        self.router = Router(self.config.router)
        self.executor = ClaudeExecutor(model=self.config.router.default_model)

        # Ensure Ollama is running for local model inference
        ensure_ollama_running(host=self.config.router.ollama_host)

        self.grammar = GrammarCorrector(
            host=self.config.router.ollama_host,
            model=self.config.grammar.model,
            timeout_s=self.config.grammar.timeout_s,
            enabled=self.config.grammar.enabled,
            min_words=self.config.grammar.min_words,
        )

        self.local_executor = LocalExecutor(
            host=self.config.router.ollama_host,
            model=self.config.router.ollama_model,
            timeout_s=self.config.router.ollama_timeout_s,
        )
        self.timer_executor = TimerExecutor()

        # Transcriber loads models in background
        self.transcriber = Transcriber(
            config=self.config.whisper,
            on_ready=self._on_transcriber_ready,
            on_error=self._on_transcriber_error,
        )

    def _on_transcriber_ready(self) -> None:
        """Called when transcriber models are loaded."""
        self.notifier.notify(
            "Dictate Agent Ready",
            f"Model: {self.config.whisper.model.split('/')[-1]}",
            "audio-input-microphone",
            3000,
        )

    def _on_transcriber_error(self, error: str) -> None:
        """Called when transcriber fails to load."""
        self.notifier.error(f"Model load failed: {error[:50]}")

    def toggle(self) -> None:
        """Toggle recording on/off."""
        if self.recording:
            self._stop_recording()
        else:
            self._start_recording()

    def cancel(self) -> None:
        """Cancel recording without transcription."""
        if not self.recording:
            return

        print("Cancelling recording...")
        self.recording = False
        self.audio.stop()
        self.audio.cleanup()

        # Resume media if it was playing
        self._resume_media_if_needed()

        self.notifier.notify(
            "Recording Cancelled",
            "Discarded without transcription",
            "dialog-cancel",
            2000,
        )

    def _start_recording(self) -> None:
        """Start recording audio."""
        if self.recording:
            return

        # Pause media if playing
        if self._is_media_playing():
            MEDIA_STATE_FILE.write_text("playing")
            self._pause_media()
        elif MEDIA_STATE_FILE.exists():
            MEDIA_STATE_FILE.unlink()

        # Start recording
        self.audio.start()
        self.recording = True

        print("Recording... (toggle again to stop)")
        self.notifier.recording()

    def _stop_recording(self) -> None:
        """Stop recording and process audio."""
        if not self.recording:
            return

        self.recording = False
        audio_path = self.audio.stop()

        if not audio_path:
            print("No audio recorded")
            return

        print("Transcribing...")
        self.notifier.transcribing()

        # Transcribe
        result = self.transcriber.transcribe(audio_path)
        self.audio.cleanup()

        if not result or not result.text:
            print("No speech detected")
            self.notifier.no_speech()
            self._resume_media_if_needed()
            return

        text = result.text
        print(f"Transcribed: {text}")

        # Grammar correction (fail-open: original text on failure)
        if self.grammar.enabled:
            grammar_result = self.grammar.correct(text)
            if grammar_result.success and grammar_result.corrected != grammar_result.original:
                print(f"Grammar corrected: {grammar_result.corrected}")
            elif grammar_result.error:
                print(f"Grammar skipped: {grammar_result.error}")
            text = grammar_result.corrected

        # Route the text
        route_result = self.router.route(text)
        print(f"Route: {route_result.route.value} (confidence: {route_result.confidence:.2f})")

        # Handle based on route
        self._handle_route(route_result)

        # Resume media if it was playing
        self._resume_media_if_needed()

    def _handle_route(self, route_result) -> None:
        """Handle routed text based on type."""
        if route_result.route == RouteType.TYPE:
            # Just type the text
            self.output.type_text(route_result.text)
            self.notifier.done(route_result.text)

        elif route_result.route == RouteType.COMMAND:
            # TODO: Implement command execution
            print(f"Command mode not yet implemented: {route_result.text}")
            self.output.type_text(route_result.text)
            self.notifier.done(route_result.text)

        elif route_result.route == RouteType.TIMER:
            # Set a timer via systemd-run
            result = self.timer_executor.execute(route_result.text)
            if result.success:
                self.notifier.done(result.response)
            else:
                print(f"Timer error: {result.error}")
                self.notifier.error(result.error or "Failed to set timer")

        elif route_result.route == RouteType.EDIT:
            # TODO: Implement edit mode (get selected text, transform, replace)
            print(f"Edit mode not yet implemented: {route_result.text}")
            self.notifier.error("Edit mode not implemented")

        elif route_result.route == RouteType.LOCAL:
            # Send to local Ollama model
            self.notifier.processing("local")
            result = self.local_executor.execute(route_result.text)
            if result.success:
                self.output.type_text(result.response)
                self.notifier.done(result.response)
            else:
                print(f"Local model error: {result.error}")
                self.notifier.error(result.error or "Local model error")

        elif route_result.route in (RouteType.HAIKU, RouteType.SONNET, RouteType.OPUS):
            # Send to Claude
            model = route_result.model
            self.notifier.processing(model)

            response = []

            def on_delta(text: str) -> None:
                response.append(text)

            result = self.executor.execute(
                route_result.text,
                model=model,
                on_delta=on_delta,
            )

            if result.success:
                full_response = result.response or "".join(response)
                self.output.type_text(full_response)
                self.notifier.done(full_response)
            else:
                print(f"Claude error: {result.error}")
                self.notifier.error(result.error or "Unknown error")

    def _is_media_playing(self) -> bool:
        """Check if media is currently playing."""
        try:
            result = subprocess.run(
                ["playerctl", "status"],
                capture_output=True,
                text=True,
                timeout=1,
            )
            return result.stdout.strip() == "Playing"
        except Exception:
            return False

    def _pause_media(self) -> None:
        """Pause any playing media."""
        try:
            subprocess.run(["playerctl", "pause"], capture_output=True, timeout=1)
        except Exception:
            pass

    def _resume_media(self) -> None:
        """Resume media playback."""
        try:
            subprocess.run(["playerctl", "play"], capture_output=True, timeout=1)
        except Exception:
            pass

    def _resume_media_if_needed(self) -> None:
        """Resume media if it was playing before dictation."""
        if MEDIA_STATE_FILE.exists():
            try:
                MEDIA_STATE_FILE.unlink()
                self._resume_media()
            except Exception:
                pass

    def stop(self) -> None:
        """Stop the daemon."""
        print("\nStopping...")
        self.running = False

        # Cleanup
        if PID_FILE.exists():
            PID_FILE.unlink()

        os._exit(0)

    def run(self) -> None:
        """Run the daemon main loop."""
        print(f"Dictate Agent v{__version__}")
        print(f"Config: {CONFIG_FILE}")
        print(f"PID: {os.getpid()}")
        print()

        # Load transcription models in background
        self.transcriber.load_models_async()

        print("Waiting for SIGUSR1 to toggle recording...")
        print("Use: dictate-toggle or kill -USR1 $(cat ~/.config/dictate-agent/dictate.pid)")
        print("Cancel: dictate-cancel or kill -USR2 $(cat ~/.config/dictate-agent/dictate.pid)")
        print("Press Ctrl+C to quit.")
        print()

        # Wait for signals
        while self.running:
            signal.pause()


def check_all_dependencies() -> list[tuple[str, str]]:
    """Check all dependencies. Returns list of (dep, instruction) missing."""
    missing = []
    missing.extend(check_audio_dependencies())
    missing.extend(check_output_dependencies())
    missing.extend(check_notify_dependencies())
    missing.extend(check_transcription_dependencies())
    # Don't require claude CLI or ollama for basic operation
    # missing.extend(check_executor_dependencies())
    # missing.extend(check_local_dependencies())
    return missing


def main() -> None:
    """Main entry point."""
    parser = argparse.ArgumentParser(
        description="Dictate Agent - Voice dictation with Claude integration"
    )
    parser.add_argument(
        "-v", "--version",
        action="version",
        version=f"Dictate Agent {__version__}",
    )
    parser.add_argument(
        "--check",
        action="store_true",
        help="Check dependencies and exit",
    )
    args = parser.parse_args()

    # Check dependencies
    missing = check_all_dependencies()
    if missing:
        print("Missing dependencies:")
        for dep, instruction in missing:
            print(f"  {dep}: {instruction}")
        if args.check:
            sys.exit(1)
        print()
        print("Some features may not work. Continuing anyway...")
        print()

    if args.check:
        print("All dependencies OK!")
        sys.exit(0)

    # Create config directory
    CONFIG_DIR.mkdir(parents=True, exist_ok=True)

    # Write PID file
    PID_FILE.write_text(str(os.getpid()))

    # Create agent
    agent = DictateAgent()

    # Setup signal handlers
    def handle_sigint(sig, frame):
        agent.stop()

    def handle_sigusr1(sig, frame):
        agent.toggle()

    def handle_sigusr2(sig, frame):
        agent.cancel()

    signal.signal(signal.SIGINT, handle_sigint)
    signal.signal(signal.SIGTERM, handle_sigint)
    signal.signal(signal.SIGUSR1, handle_sigusr1)
    signal.signal(signal.SIGUSR2, handle_sigusr2)

    # Run
    agent.run()


if __name__ == "__main__":
    main()
