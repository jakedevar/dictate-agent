"""
Claude Code executor - runs Claude Code CLI and parses responses.
"""

import subprocess
from dataclasses import dataclass
from typing import Callable, Optional


@dataclass
class ExecutionResult:
    """Result from Claude Code execution."""

    success: bool
    response: str
    error: Optional[str] = None


class ClaudeExecutor:
    """Executes requests via Claude Code CLI."""

    def __init__(self, model: str = "sonnet"):
        self.model = model

    def execute(
        self,
        prompt: str,
        model: Optional[str] = None,
        on_delta: Optional[Callable[[str], None]] = None,
    ) -> ExecutionResult:
        """
        Execute a prompt via Claude Code CLI.

        Args:
            prompt: The prompt to send to Claude
            model: Model to use (haiku, sonnet, opus)
            on_delta: Optional callback for streaming text deltas (not used in simple mode)

        Returns:
            ExecutionResult with response or error
        """
        use_model = model or self.model

        # Build command - use claude code CLI with simple text output
        cmd = [
            "claude",
            "--print",  # Print response to stdout
            "--dangerously-skip-permissions",  # Skip permission prompts for headless operation
            "--model", use_model,
            prompt,
        ]

        try:
            print(f"Executing Claude ({use_model}): {prompt[:50]}...")

            result = subprocess.run(
                cmd,
                capture_output=True,
                text=True,
                timeout=120,  # 2 minute timeout
            )

            if result.returncode != 0:
                error_msg = result.stderr.strip() or f"Claude exited with code {result.returncode}"
                print(f"Claude error: {error_msg}")
                return ExecutionResult(
                    success=False,
                    response="",
                    error=error_msg,
                )

            response = result.stdout.strip()
            print(f"Claude response ({len(response)} chars): {response[:100]}...")

            return ExecutionResult(
                success=True,
                response=response,
            )

        except subprocess.TimeoutExpired:
            return ExecutionResult(
                success=False,
                response="",
                error="Claude timed out after 120 seconds",
            )
        except FileNotFoundError:
            return ExecutionResult(
                success=False,
                response="",
                error="Claude Code CLI not found. Install with: npm install -g @anthropic-ai/claude-code",
            )
        except Exception as e:
            return ExecutionResult(
                success=False,
                response="",
                error=str(e),
            )

    def execute_edit(
        self,
        instruction: str,
        selected_text: str,
        model: str = "haiku",
    ) -> ExecutionResult:
        """
        Execute a text editing instruction.

        Args:
            instruction: What to do with the text
            selected_text: The text to transform
            model: Model to use

        Returns:
            ExecutionResult with transformed text
        """
        prompt = f"""Transform the following text according to the instruction.
Return ONLY the transformed text, nothing else.

Instruction: {instruction}

Text to transform:
{selected_text}

Transformed text:"""

        return self.execute(prompt, model=model)


def check_executor_dependencies() -> list[tuple[str, str]]:
    """Check executor dependencies. Returns list of (dep, instruction) missing."""
    missing = []

    # Check for claude CLI
    result = subprocess.run(["which", "claude"], capture_output=True)
    if result.returncode != 0:
        missing.append(
            ("claude", "npm install -g @anthropic-ai/claude-code")
        )

    return missing
