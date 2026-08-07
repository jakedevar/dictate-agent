---
date: 2026-08-07
researcher: claude-team
git_commit: 4f339376632d920c4f25f31bb4b2f79d089f13ca
branch: rsi/76789b78
repository: dictate_agent
topic: "R1 — Cannibalization audit: Handy (and Whispering/VoiceInk) for Wispr Flow parity rebuild"
tags: [research, cannibalize, comparative-analysis, handy, hotkey, overlay, model-manager, wayland, packaging, S12, S13, S31, S32, S40, S42]
status: complete
type: research
last_updated: 2026-08-07
last_updated_by: claude-team
---

# R1 — Cannibalization audit: Handy for dictate-agent Wispr Flow parity

## Question

For the Rust-first dictate-agent rebuild (locked stack: Cargo workspace,
whisper-rs/whisper.cpp + CUDA, Tauri v2 UI, X11 first-class, evdev hotkeys,
SQLite history — see `thoughts/shared/plans/2026-08-07-wisprflow-parity-master-slice-map.md`
§Target architecture / §Locked decisions), what is worth adopting, adapting,
or avoiding from **Handy** (github.com/cjpais/Handy, MIT, Rust + Tauri v2 +
whisper-rs, cross-platform) — the closest existing project to our target
architecture — across five axes feeding slices S12, S13, S31, S32, S40, S42?

Handy was cloned to `/tmp/r1-handy` (scratch, not vendored into this repo) at
commit `b428ae4c9c8aecba00d0e44e14d6466d9687795f` (2026-08-07). Whispering and
VoiceInk secondary passes were not performed in this session (time-boxed to
the primary Handy audit); flagged as an open follow-up below.

## Axis 1 — Hotkey handling (feeds S31)

| Verdict | Rationale | Source | License note |
|---|---|---|---|
| ADOPT | Single-owner-thread pattern: `HotkeyManager` owned exclusively by one thread, driven by an mpsc command channel for register/unregister, polled for events — avoids cross-thread evdev/libinput state races. | `handy:src-tauri/src/shortcut/handy_keys.rs:109` | n/a |
| ADOPT | `handy-keys` crate resolves to `evdev` on Linux per Cargo.lock, confirming an evdev-grab backend is viable underneath a Tauri v2 app — validates our S31 evdev-service approach (architecture pattern only; crate is external, not vendored). | `handy:src-tauri/Cargo.lock:2517` | n/a |
| ADOPT | Press/release modeled as first-class binary state (`is_pressed: bool`) with a dedicated single-threaded coordinator classifying each event (Passthrough/DeferRelease/CancelRelease) before start/stop — clean layering, decouples hold-to-talk semantics from the raw hotkey backend. | `handy:src-tauri/src/transcription_coordinator.rs:47` | n/a |
| ADOPT | Key-up events deferred for a short grace window (`RELEASE_GRACE`), cancelled if a matching key-down arrives first — explicitly absorbs X11 key auto-repeat bursts that would otherwise flap hold-to-talk recording. Directly solves our "reliable release detection on X11" requirement. | `handy:src-tauri/src/transcription_coordinator.rs:147` | MIT — near-verbatim reuse of the debounce logic requires Handy attribution |
| AVOID | Dynamic runtime registration/unregistration of the "cancel" shortcut is explicitly disabled on Linux ("unstable with dynamic shortcut registration") — known footgun for both Tauri global-shortcut and handy-keys paths. S31's cancel-chord should be a static/always-grabbed key, not dynamically added/removed. | `handy:src-tauri/src/shortcut/tauri_impl.rs:166` | n/a |

**Verdict counts:** 4 ADOPT, 0 ADAPT, 1 AVOID

## Axis 2 — Overlay/HUD approach (feeds S32)

| Verdict | Rationale | Source | License note |
|---|---|---|---|
| ADOPT | Overlay is a separate always-on-top, transparent, undecorated, non-focusable `WebviewWindow` (label `recording_overlay`) reused across states, decoupled from the main app window — matches our locked Tauri v2 architecture. | `handy:src-tauri/src/overlay.rs:377` | n/a |
| ADAPT | Linux click-through/layering uses `gtk-layer-shell` (`Layer::Overlay`, `keyboard_mode: None`, `exclusive_zone: 0`) with top/bottom anchors, falling back to a plain borderless window when layer-shell isn't supported. For our X11-first target, prefer X11-native override-redirect/always-on-top as primary, keep layer-shell code path as the Wayland best-effort fallback (S32 already scopes it this way). | `handy:src-tauri/src/overlay.rs:114` | MIT — verbatim code/comment reuse requires attribution |
| ADOPT | Position/size math (monitor-under-cursor detection, DPI-scaled logical coordinates, top/bottom offset constants) cleanly separates "where" from "which layer manager" — portable core reusable regardless of X11 vs layer-shell specifics. | `handy:src-tauri/src/overlay.rs:238` | MIT — verbatim formulas/constants require attribution |
| AVOID | No `set_ignore_cursor_events` call anywhere — click-through is NOT implemented via true cursor-passthrough; relies on `focusable(false)`/`skip_taskbar`/`always_on_top` plus (Linux) layer-shell `keyboard_mode: None`. Weaker guarantee than true click-through; don't assume it as a solved pattern for S32. | `handy:src-tauri/src/overlay.rs:383` | n/a |
| ADOPT | Audio-level HUD driven by a throttled (~30 FPS) `emit_to("recording_overlay", "mic-level", levels)` carrying 16 FFT-bucket levels, consumed by a React waveform (9 bars) — simple event-bridge pattern directly reusable for our whisper-rs/CUDA pipeline's live level meter. | `handy:src-tauri/src/overlay.rs:672` | MIT — verbatim code reuse requires attribution |

**Verdict counts:** 3 ADOPT, 1 ADAPT, 1 AVOID

## Axis 3 — Model manager UX (feeds S12)

| Verdict | Rationale | Source | License note |
|---|---|---|---|
| ADOPT | sha256 verify-then-delete-on-mismatch (64KB chunked digest, remove corrupt/partial file, force clean retry) is exactly the checksum discipline our `dictate model pull` needs. | `handy:src-tauri/src/managers/model/download.rs:56` | MIT — verbatim reuse requires attribution |
| ADOPT | `ModelSource::HuggingFace` resolves via hf-hub's shared cache (`HF_HOME`/`~/.cache/huggingface/hub`) with revision-pinned commit SHAs for reproducible, CDN-immutable downloads — matches our hf-hub + `models/` design directly. | `handy:src-tauri/src/managers/model.rs:44` | n/a |
| ADAPT | Build-time-generated `catalog.json` (compiled in via `include_str!`) bundling repo id, pinned revision, per-quant filenames/sizes/sha256 is a good static-manifest pattern for `dictate model list`, but Handy's multi-engine/quant richness can be stripped down since we're single-family (whisper large-v3-turbo/small/tiny). | `handy:src-tauri/src/catalog/mod.rs:117` | n/a |
| AVOID | First run does NOT force a download — `auto_select_model_if_needed` only auto-picks an already-downloaded model, deferring choice to a UI onboarding wizard. Not applicable to our CLI-first daemon; S12 should instead default-pull `large-v3-turbo` (or CPU fallback) on first `dictate` invocation. | `handy:src-tauri/src/managers/model.rs:1500` | n/a |
| ADAPT | Resumable HTTP download (Range-header resume, `.partial` file tracking, byte-cap enforcement, stall-timeout cancellation) is solid transport hardening worth reusing for our CPU/small/tiny fallback path when not going through hf-hub directly. | `handy:src-tauri/src/managers/model/download.rs:184` | MIT — verbatim reuse requires attribution |

**Verdict counts:** 2 ADOPT, 2 ADAPT, 1 AVOID

## Axis 4 — Wayland/macOS workarounds (feeds S13/S40)

| Verdict | Rationale | Source | License note |
|---|---|---|---|
| ADAPT | Linux paste has a clean X11/Wayland split with runtime tool-availability probing (xdotool for X11; wtype/kwtype/dotool/ydotool fallback chain for Wayland) — maps directly onto our X11-first + Wayland-adapter (R3) plan. | `handy:src-tauri/src/clipboard.rs:105` | n/a (pattern only) |
| ADOPT | KDE-specific Wayland detection (`is_kde_wayland`) gates `wtype` off (no `zwp_virtual_keyboard` support on KDE) and routes to `kwtype` instead — concrete compositor-quirk workaround worth reusing when building the S13/S40 Wayland adapter. | `handy:src-tauri/src/utils.rs:126` | MIT — verbatim detection logic requires attribution |
| AVOID | `paste_tx` (receipt-sequenced clipboard-paste engine) is macOS/Windows-only (`NSPasteboard` promise callback / Win32 delayed rendering) with zero Linux/X11 implementation — nothing transfers to our X11-first clipboard-paste-with-save/restore requirement; the X11 equivalent must be built from scratch. | `handy:src-tauri/src/paste_tx/mod.rs:40` | n/a |
| AVOID | Secure-input detection (`IsSecureEventInputEnabled`, Carbon-tap fallback) is macOS-only, tied to CGEventTap semantics; not applicable to our X11/evdev-first stack, and no Linux equivalent exists in Handy to adapt. Relevant only when we reach S40 (macOS). | `handy:src-tauri/src/secure_input.rs:121` | n/a |
| AVOID | Accessibility-permissions onboarding is a macOS-only React component gated on `type() === "macos"`; no analogous frontmost-app-context/permission-onboarding logic exists for Linux/X11 to reuse now — relevant only at S40. | `handy:src/components/AccessibilityPermissions.tsx:25` | n/a |

**Verdict counts:** 1 ADOPT, 1 ADAPT, 3 AVOID

## Axis 5 — Packaging & build (feeds S42, plus S12/S40 build docs)

| Verdict | Rationale | Source | License note |
|---|---|---|---|
| ADOPT | `build.rs` bakes an `$ORIGIN`-relative rpath on Linux so the binary finds a co-located shared whisper.cpp lib instead of relying on ldconfig/system paths — directly reusable for a CUDA `.so` + CPU-fallback module layout. | `handy:src-tauri/build.rs:18-20` | MIT — verbatim rpath snippet requires attribution |
| ADOPT | Dedicated CI script stages/verifies native backend libs post-build, hard-failing if the CPU-fallback module is missing — good template for a justfile step ensuring the CUDA build always ships a working CPU/tiny fallback (matches our locked decision #1). | `handy:scripts/ci/stage-transcribe-libs.sh:42-51` | MIT — verbatim script reuse requires attribution |
| ADAPT | Per-target `transcribe-cpp` feature flags (`dynamic-backends` + `vulkan` on Linux, `metal` statically on macOS) is the right *shape* for our feature-flag scheme, but Handy's GPU story is Vulkan/Metal, not CUDA — swap the Linux feature for `cuda` and keep the flag-per-platform structure. | `handy:src-tauri/Cargo.toml:153-163` | n/a (pattern, not verbatim text) |
| ADAPT | GH Actions release matrix (macOS arm64+x86_64, Ubuntu 22.04/24.04 x86_64+arm64, Windows x64+arm64) via one reusable parameterized `build.yml` is a good skeleton for our arch-linux + macOS matrix, but needs a CUDA runner/toolkit-install step added — Handy has none. | `handy:.github/workflows/release.yml:47-69` | n/a |
| AVOID | Handy has no CUDA/cudarc build path anywhere (build.rs, Cargo.toml, CI) — its native-build env-var incantations are Vulkan SDK / glslc / `VULKAN_SDK`, which do not transfer to our CUDA-locked stack and must not be copied as-is. Our CUDA build-env-var learnings (already documented per S12/S42 scope) remain our own source of truth. | `handy:BUILD.md:45-53` | n/a |

**Verdict counts:** 2 ADOPT, 2 ADAPT, 1 AVOID

## Overall verdict totals

| Axis | ADOPT | ADAPT | AVOID | Total |
|---|---|---|---|---|
| 1 — Hotkey (S31) | 4 | 0 | 1 | 5 |
| 2 — Overlay/HUD (S32) | 3 | 1 | 1 | 5 |
| 3 — Model manager (S12) | 2 | 2 | 1 | 5 |
| 4 — Wayland/macOS (S13/S40) | 1 | 1 | 3 | 5 |
| 5 — Packaging/build (S42) | 2 | 2 | 1 | 5 |
| **Total** | **12** | **6** | **7** | **25** |

No AVOID finding invalidates a locked decision in the master plan — the CUDA
AVOID (axis 5) reinforces locked decision #1 (whisper-rs+CUDA) rather than
contradicting it: Handy simply offers no reusable CUDA build incantations, so
S12/S42's own documented CUDA env-var learnings (handoff §Learnings) remain
the authority, not Handy's Vulkan path.

## License note (MIT)

Handy is MIT-licensed. Every entry marked "verbatim reuse requires
attribution" means: if we copy code, comments, constants, or scripts
near-verbatim from Handy into dictate-agent, we must retain/attach an MIT
attribution notice (e.g. a `NOTICE` file or a source-header comment crediting
cjpais/Handy) per MIT's copyright/permission-notice requirement. Entries
marked "pattern only" or "n/a" describe architectural approaches we would
reimplement independently — no attribution obligation attaches to an
unprotectable pattern/idea, only to copied expression.

## Direct actions for our slices

- **S31 (Hotkey service):** Adopt the single-owner-thread + mpsc-command
  pattern and the release-grace debounce window to defeat X11 auto-repeat
  flapping (axis 1, `transcription_coordinator.rs:147`). Design the
  cancel-chord as a statically-grabbed key, not dynamic register/unregister
  (axis 1 AVOID).
- **S32 (Tauri UI v1 — HUD overlay):** Adopt the separate always-on-top
  non-focusable `WebviewWindow` overlay architecture and the throttled
  `mic-level` event-bridge for the audio-level HUD (axis 2). Adapt the
  gtk-layer-shell Linux layering code as the Wayland best-effort fallback
  behind an X11-native always-on-top primary path — do not treat Handy's
  click-through as solved (no `set_ignore_cursor_events`); S32 must design
  its own click-through verification.
- **S12 (STT engine v2 / model manager):** Adopt the chunked sha256
  verify-then-delete pattern and hf-hub revision-pinned cache resolution.
  Adapt the static catalog-manifest idea (stripped to single-family
  whisper) and the resumable-download hardening for the CPU/tiny fallback
  path. Do not adopt Handy's "no forced first-run download" flow — S12
  should default-pull on first CLI invocation.
- **S13/S40 (Injection v2 / macOS port):** Adapt the X11-vs-Wayland tool
  probing chain (xdotool / wtype-kwtype-dotool-ydotool) and the KDE-Wayland
  compositor-quirk detection for the S13 Wayland adapter stub. Handy's
  macOS-specific paste-transaction engine, secure-input detection, and
  accessibility-permission onboarding are AVOID for now (no Linux
  transferability) but become relevant references when S40 (macOS port)
  starts.
- **S42 (Packaging, CI, install story):** Adopt the `$ORIGIN`-relative rpath
  build.rs trick and the CI native-lib staging/verification script. Adapt
  the per-platform feature-flag shape and the GH Actions release matrix
  structure, substituting a CUDA toolkit/runner step for Handy's Vulkan SDK
  step. Do not port Handy's Vulkan build env-vars — irrelevant to our
  CUDA-locked stack.

## Open questions / follow-ups

- Whispering (epicenter-md/whispering) and VoiceInk secondary passes were
  not performed in this session — time-boxed to the primary Handy deep
  audit across 5 axes. Recommend a lighter single-pass follow-up research
  session if either project offers meaningfully different UX patterns
  (e.g. Whispering's cross-platform Tauri v2 + Rust core is architecturally
  close enough to warrant at least a model-manager and overlay comparison).
- True click-through (cursor-event passthrough) is unsolved in Handy itself
  (axis 2 AVOID) — this remains open design work for S32, not something we
  can crib from either reference project on the evidence gathered so far.
