"""
Local model executor using Ollama for fast, offline inference.
"""

import subprocess
import time
import urllib.request
from dataclasses import dataclass
from typing import Optional


@dataclass
class ExecutionResult:
    """Result from local model execution."""

    success: bool
    response: str
    error: Optional[str] = None


class LocalExecutor:
    """Executes requests via local Ollama models."""

    def __init__(
        self,
        host: str = "http://localhost:11434",
        model: str = "qwen3:14b",
        timeout_s: float = 120.0,
    ):
        self.host = host
        self.model = model
        self.timeout = timeout_s

    def execute(self, prompt: str, model: str | None = None) -> ExecutionResult:
        """
        Execute a prompt via Ollama.

        Args:
            prompt: The prompt to send to the local model
            model: Optional model override

        Returns:
            ExecutionResult with response or error
        """
        use_model = model or self.model
        try:
            import ollama

            client = ollama.Client(host=self.host, timeout=self.timeout)

            print(f"Executing Ollama ({use_model}): {prompt[:50]}...")

            response = client.generate(
                model=use_model,
                prompt=prompt,
                options={
                    "num_predict": 2048,
                },
            )

            text = response.get("response", "").strip()
            print(f"Ollama response ({len(text)} chars): {text[:100]}...")

            return ExecutionResult(
                success=True,
                response=text,
            )

        except ImportError:
            return ExecutionResult(
                success=False,
                response="",
                error="ollama package not installed. Install with: pip install ollama",
            )
        except Exception as e:
            error_msg = str(e)
            # Check for common Ollama errors
            if "connection" in error_msg.lower() or "refused" in error_msg.lower():
                error_msg = f"Cannot connect to Ollama at {self.host}. Is it running? (ollama serve)"
            elif "not found" in error_msg.lower():
                error_msg = f"Model '{use_model}' not found. Pull it with: ollama pull {use_model}"

            print(f"Ollama error: {error_msg}")
            return ExecutionResult(
                success=False,
                response="",
                error=error_msg,
            )

    def execute_edit(self, instruction: str, selected_text: str) -> ExecutionResult:
        """
        Execute a text editing instruction via Ollama.

        Args:
            instruction: What to do with the text
            selected_text: The text to transform

        Returns:
            ExecutionResult with transformed text
        """
        prompt = f"""Transform the following text according to the instruction.
Return ONLY the transformed text, nothing else.

Instruction: {instruction}

Text to transform:
{selected_text}

Transformed text:"""

        return self.execute(prompt)


def is_ollama_running(host: str = "http://localhost:11434", timeout: float = 1.0) -> bool:
    """Check if Ollama server is responding."""
    try:
        req = urllib.request.Request(f"{host}/api/tags", method="GET")
        with urllib.request.urlopen(req, timeout=timeout):
            return True
    except Exception:
        return False


def ensure_ollama_running(
    host: str = "http://localhost:11434",
    max_wait_s: float = 10.0,
) -> bool:
    """
    Ensure Ollama server is running, starting it if necessary.

    Args:
        host: Ollama server URL
        max_wait_s: Maximum seconds to wait for server to start

    Returns:
        True if Ollama is running, False otherwise
    """
    # Already running?
    if is_ollama_running(host):
        print("Ollama server already running")
        return True

    print("Starting Ollama server...")
    try:
        # Start ollama serve in background
        subprocess.Popen(
            ["ollama", "serve"],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            start_new_session=True,  # Detach from parent process
        )

        # Wait for it to come up
        start_time = time.time()
        while time.time() - start_time < max_wait_s:
            if is_ollama_running(host):
                print("Ollama server started successfully")
                return True
            time.sleep(0.5)

        print(f"Ollama server did not start within {max_wait_s}s")
        return False

    except FileNotFoundError:
        print("Ollama not installed. Install from: https://ollama.ai")
        return False
    except Exception as e:
        print(f"Failed to start Ollama: {e}")
        return False


def check_local_dependencies() -> list[tuple[str, str]]:
    """Check local executor dependencies. Returns list of (dep, instruction) missing."""
    missing = []

    try:
        import ollama
    except ImportError:
        missing.append(("ollama", "pip install ollama"))

    return missing
