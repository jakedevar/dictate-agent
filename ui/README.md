# dictate-ui — the desktop UI (S32)

An optional, local-only client of `dictated`. The daemon is fully functional
without it; the UI only speaks the public protocol (`dictate-proto`, NDJSON
over the daemon's Unix socket) and never links whisper, CUDA or `dictate-core`.

It has three parts:

- **Flow bar** — a small pill at the bottom centre of the monitor under the
  pointer. It appears the moment recording starts (live audio level and an
  elapsed timer), shows progress while transcribing and formatting, then a
  brief ✓ / ✗ / cancelled before it hides. It never takes keyboard focus and
  clicks pass straight through it (see [The Flow bar on X11](#the-flow-bar-on-x11)).
- **Tray icon** — idle / recording / processing / error / daemon offline, with
  Start/Stop dictation, Cancel, Open hub, Run doctor, and Quit UI (the daemon
  keeps running).
- **Hub window** — Home (words per minute, words today and this week, streak,
  a 14-day chart, recent dictations), History (full-text search, copy,
  privacy-mode indicator), Notes (the voice-note scratchpad: search, copy, delete,
  *Dictate a note*), Dictionary (entries and suggestions), Settings (a
  structured editor for common keys plus a raw TOML editor), and Doctor (every
  `dictate doctor` check with its fix, problems first).

## Build and run

Requirements (Arch): `webkit2gtk-4.1`, `gtk3`, `libayatana-appindicator`,
`bun`, and `cargo tauri` (`cargo install tauri-cli --version '^2'`).

```bash
just ui-build      # release binary: target/release/dictate-ui
just ui-dev        # hot-reloading frontend against the running daemon
just ui-test       # typecheck + bun tests, Rust bridge/HUD tests, clippy
just ui-test-x11   # the focus/click-through test in a private Xvfb + i3
```

`ui/src-tauri` is **not** a member of the root Cargo workspace (it is listed
under `exclude`), so `cargo test --workspace` never needs WebKit. It still
builds into the repository's `target/` directory, because the root
`.cargo/config.toml` sets `target-dir`.

`dictate-ui --hub` opens the hub at start; otherwise the app starts in the
tray (and opens the hub only if no tray is available).

The UI finds the daemon at `$XDG_RUNTIME_DIR/dictate-agent/dictated.sock`, or
`$DICTATE_SOCKET` when set. It reconnects on its own with backoff; while the
daemon is down the hub says so and the tray icon shows "offline".

### Autostart with i3

Documented, not installed. Add to `~/.config/i3/config`:

```text
exec --no-startup-id ~/.local/bin/dictate-ui
```

(copy `target/release/dictate-ui` there first). The bar needs no i3 rule; if
a window manager ignores its hints, see the fallback rule below.

## The Flow bar on X11

Above everything, the bar must never take focus: the daemon pastes into the
focused window, so a bar that grabbed focus on show would receive the user's
own dictation. `src-tauri/src/x11.rs` documents each mechanism:

| Property | Mechanism |
|---|---|
| never focused by the WM | `WM_HINTS.input = False` |
| not focused when mapped | `_NET_WM_USER_TIME = 0` |
| floats in i3 with no rule | `_NET_WM_WINDOW_TYPE_NOTIFICATION` |
| above, on every workspace, off taskbars | `_NET_WM_STATE_ABOVE`, `_STICKY`, `_SKIP_TASKBAR`, `_SKIP_PAGER` |
| click-through | an **empty** XShape input region on the toplevel |
| no black box without a compositor | an XShape bounding region cut to the pill |

The window is created once and only mapped and unmapped afterwards; its
position is computed per show from the monitor under the pointer and that
monitor's scale factor (`src-tauri/src/hud.rs`, unit-tested).

`ui/tests/x11/hud-focus.sh` verifies this end to end in a private Xvfb with
i3 (and `focus_follows_mouse yes`, the harder case) over two full show/hide
cycles: `xev` holds focus and logs what it receives, so the test observes that
keystrokes typed while the bar is up and a click at the bar's centre are both
delivered to the window underneath, that X and i3 focus never move, that i3
floats the bar by itself, and that it sits bottom-centre. It unsets the real
`DISPLAY`, runs under a private D-Bus session (so the tray icon never reaches
the real desktop) and uses private XDG directories.

**Fallback rule** for a window manager (or an i3 config) that ignores the
hints:

```text
for_window [class="dictate-ui" title="^Flow bar$"] floating enable, sticky enable, border none
no_focus [class="dictate-ui" title="^Flow bar$"]
```

**Wayland** is a best-effort follow-up, not implemented: there the bar would
need the layer-shell protocol (`gtk-layer-shell`, overlay layer, no keyboard
interactivity, empty input region) behind the X11-native path.

## Settings and the configuration file

Settings are read and written through the daemon (`get_config` /
`set_config`, local connections only), never by the UI touching the file:

- every write is validated by the same loader `dictated --check-config` uses;
  a bad value is refused with the offending key highlighted, and nothing is
  written;
- writes keep comments, ordering and unknown keys, are atomic (temp file,
  fsync, rename) and leave one backup, `config.toml.bak`;
- each save sends the file revision it was based on, so if the file changed
  in the meantime (another window, or an edit by hand) the save is refused
  and the page reloads instead of overwriting it;
- the raw TOML editor validates as you type with a dry run;
- only `history.privacy_mode` applies live; everything else is listed as
  needing `systemctl --user restart dictated`.

A file that still configures the LLM pass through the deprecated `[grammar]`
section is edited through `[grammar]`: writing `[format.llm]` keys into it
would create that section, which takes precedence and would silently discard
the `[grammar]` settings.

## Frontend choices

- **Preact** for the hub: about 49 KB of JavaScript (17 KB gzipped) for the
  whole hub, and it starts instantly; React would triple that for nothing this
  UI uses.
- **No framework** for the Flow bar (about 2.4 KB): it must paint within a
  frame of being mapped, and it has four moving parts.
- Strict TypeScript, bun as the package manager with `bun.lock` committed,
  Vite for the build.
- No network access at runtime and no telemetry: the CSP in `tauri.conf.json`
  allows only the app's own assets and Tauri IPC, no fonts or images are
  fetched, and the capability file grants event listening only (no
  filesystem, shell, HTTP or opener plugins).

## Layout

```text
ui/
  index.html, hud.html     the two webviews
  src/hub/                 hub app (Preact) and pages
  src/hud/                 the Flow bar (plain TS + CSS)
  src/lib/                 protocol types, Tauri command wrappers, pure view
                           models (hudModel, settings, dictionary, doctor, format)
  tests/                   bun unit tests; x11/hud-focus.sh
  src-tauri/               the Rust side: bridge (socket, handshake, reconnect,
                           ~30 fps audio_level throttle), commands (1:1 with
                           protocol commands), hud (visibility + placement),
                           x11 (overlay hints), icons, tray
  docs/screenshots/        synthetic-data screenshots
```
