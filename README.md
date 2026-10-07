# dictate-agent

Local-first voice dictation for Linux: whisper.cpp speech-to-text, a local-LLM
cleanup pass, and text injected into the focused window. A daemon (`dictated`),
a control CLI (`dictate`) and an optional desktop UI (`dictate-ui`).

- Install, upgrade, roll back: [docs/INSTALL.md](docs/INSTALL.md)
- Coming from the Python or monolithic daemon: [docs/MIGRATION-FROM-PYTHON.md](docs/MIGRATION-FROM-PYTHON.md)
- Wire protocol: [docs/protocol.md](docs/protocol.md)
- Contributing / architecture: [AGENTS.md](AGENTS.md), [CLAUDE.md](CLAUDE.md)

CI (`.github/workflows/ci.yml`) runs the CPU gate only: `just check-cpu`, the UI
tests and an install smoke. No GPU, secrets or models are needed.
