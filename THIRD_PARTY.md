# Third-party components and licences

Dictate Agent is MIT-licensed (see `LICENSE`). This file lists the third-party
code and models it builds against, bundles or downloads, and their licences.
Audited 2026-10-07 from `cargo metadata --features dictated/cuda` (all
non-workspace crates) and the S32 UI's installed `node_modules`.

## Compiled in or bundled

| Component | Used by | Licence | Notes |
|---|---|---|---|
| whisper.cpp / ggml (via `whisper-rs-sys` 0.15) | `dictate-stt` | MIT | Statically linked. The `whisper-rs` bindings themselves are Unlicense. |
| Silero VAD v5 ONNX model (embedded by `voice_activity_detector` 0.2.1) | `dictate-vad` | MIT | Model file `silero_vad.onnx` is shipped inside the crate; crate MIT. |
| ONNX Runtime (via `ort` 2.0.0-rc.10) | `dictate-vad` | MIT | `ort`/`ort-sys` are MIT OR Apache-2.0. |
| SQLite (via `libsqlite3-sys`, bundled) | `dictate-history`, `dictate-dict` | Public domain | |
| `symphonia` 0.5 (core, metadata, mp3) | `dictate-audio` decode | MPL-2.0 | File-level copyleft: MPL-covered source files must stay available under MPL if modified. Compatible with distributing an MIT-licensed binary. |
| `ollama-rs` 0.3.6 | `dictate-fmt`, `dictate-core` | MIT | Licence file only (no SPDX field). |
| `cpal`, `x11rb`, `enigo`, `arboard`, `tokio`, `serde`, `axum` etc. | various | MIT / Apache-2.0 / dual | |

Rust dependency licence census (443 non-workspace crates): all are MIT, Apache-2.0,
BSD-2/3-Clause, ISC, Zlib, BSL-1.0, 0BSD, Unlicense, Unicode-3.0,
CDLA-Permissive-2.0, or dual/multi-licensed with one of those. `r-efi` offers
`MIT OR Apache-2.0 OR LGPL-2.1-or-later` and is used under MIT. MPL-2.0 is
the only weak copyleft (symphonia, above). No GPL/AGPL/SSPL dependency.

UI (`ui/`, Tauri v2 + Preact + Vite): npm packages are MIT, Apache-2.0, ISC,
BSD-3-Clause, and MPL-2.0 (3 packages). The Tauri crates are MIT OR Apache-2.0.

## Downloaded at run time, not redistributed

These are fetched by the user's machine (the STT model manager, or Ollama) and
are not part of this repository or any package built from it.

| Model | Licence | Notes |
|---|---|---|
| OpenAI Whisper weights (e.g. `large-v3-turbo`, ggml conversion) | MIT | Pulled by `dictate-stt`'s pinned catalog. |
| Gemma 4 (`gemma4:e4b`, `gemma4:12b`) via Ollama | Gemma Terms of Use | Default LLM formatter ladder. Not redistributed; the user pulls it with `ollama pull`. |
| Qwen models via Ollama (`[local]` route) | Apache-2.0 (Qwen3) | Optional. |

## Regenerating

```bash
cargo metadata --format-version 1 --features dictated/cuda | \
  python3 -c 'import json,sys,collections; d=json.load(sys.stdin); ws=set(d["workspace_members"]); print(collections.Counter(p.get("license") for p in d["packages"] if p["id"] not in ws))'
```
Re-run when adding a dependency; anything GPL/AGPL/SSPL or unlicensed needs a decision first.
