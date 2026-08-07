---
date: 2026-08-07
researcher: claude
git_commit: 4f339376632d920c4f25f31bb4b2f79d089f13ca
branch: rsi/f24927f5
repository: dictate-agent
topic: "R3 — Wayland text-injection and window-context: 2026 state of the art"
tags: [research, wayland, text-injection, clipboard, window-context, libei, xdg-desktop-portal, hyprland, sway, kwin, gnome-shell, dictate-inject, dictate-context]
status: complete
type: research
---

# R3 — Wayland text-injection and window-context: 2026 state of the art

## Question

If we needed Wayland support later, what is the least-bad implementation path
per compositor family, and what does that imply for our trait boundaries
today?

**Locked decision (do not re-litigate here):** X11 is first-class and is
Jake's actual daily environment (arboard+enigo, migrating off
xdotool/xclip). There is **no Wayland migration planned.** This is a cheap
future-proofing spike whose only output is trait-level guidance for the
`dictate-inject` (S13) and `dictate-context` (S23) crates, per
`thoughts/shared/plans/2026-08-07-wisprflow-parity-master-slice-map.md`
(§R3, §S13, §S23, "Decisions recorded 2026-08-07 (Jake)" item 1).

## Summary verdicts

- **Injection:** no single Wayland path covers all three desktop families —
  wlroots compositors get a synchronous, no-prompt path (`wtype` /
  `virtual-keyboard-unstable-v1`); GNOME and (to a lesser extent) KDE only get
  an async, per-session-consent-gated path (portal RemoteDesktop, libei/libeis
  backed).
- **Clipboard:** save/paste/restore is moderately reliable on wlroots
  compositors (`ext-data-control-v1`, widely adopted 2024–2026) and
  unreliable-to-unsupported on GNOME (no in-compositor data-control API by
  design; portal Clipboard API still unshipped as of this research).
- **Active-window context:** every compositor family requires a
  compositor-specific escape hatch; GNOME is the only one requiring an
  unofficial, user-installed Shell extension — there is no first-party path.

## 1. Text injection

### wlr-virtual-keyboard-unstable-v1 (`zwp_virtual_keyboard_manager_v1`) + `wtype`

- Protocol proposed via swaywm/wlroots PR #999 (2019); consumed by `wtype`
  (github.com/atx/wtype), the Wayland analogue of `xdotool type`.
- **wlroots-based compositors** (sway, Hyprland, river, labwc, and other
  wlroots/Smithay compositors) implement it. Recurring `wtype` issues (#29,
  #34, #45 — "Compositor does not support the virtual keyboard protocol")
  are specifically about *non-wlroots* compositors failing, which is
  corroborating evidence the wlroots family is the reliable set.
- **GNOME/Mutter does NOT implement it**, as of the two still-open GNOME
  GitLab tracker items:
  - gitlab.gnome.org/GNOME/mutter/-/issues/4124 ("Implement virtual-keyboard-v1 protocol")
  - gitlab.gnome.org/GNOME/mutter/-/work_items/1974 ("Support virtual-keyboard-unstable-v1")
  - Practically confirmed by `wtype` erroring "Compositor does not support the
    virtual keyboard protocol" on GNOME, and independently by Handy's own
    issue tracker (below).
- **KDE/KWin implements it** (moderate confidence — corroborated less
  rigorously than the GNOME non-support finding, but consistent with KDE's
  generally more permissive stance toward wlr-family protocols).
- Maturity as of 2026: the protocol itself is stable/frozen in practice
  (unchanged for years despite the "unstable-v1" name); the `wtype` CLI is a
  small, essentially finished utility with low recent commit velocity — not
  actively evolving software, but not abandoned either.

### xdg-desktop-portal RemoteDesktop (`org.freedesktop.portal.RemoteDesktop`)

- Spec: exposes `NotifyPointerMotion`, `NotifyKeyboardKeycode`,
  `NotifyKeyboardKeysym`, etc. **2026 wrinkle:** these legacy `Notify*` D-Bus
  methods error out once a session has negotiated the newer libei/EIS
  connection — the portal has bifurcated into a legacy D-Bus-notify path and
  a libei-native path that cannot be mixed within one session.
- Backend implementations:
  - `xdg-desktop-portal-gnome` — implements RemoteDesktop, backed by
    Mutter's remote-desktop API + libei.
  - `xdg-desktop-portal-kde` — implements RemoteDesktop; open KWin bug
    (mail-archive KDE bug #489021) where `NotifyKeyboardKeysym` mismaps
    keycodes for non-default keyboard layouts — relevant for typing
    accented/non-ASCII dictation output.
  - `xdg-desktop-portal-wlr` — exists but wlroots compositors more commonly
    inject directly via `wtype`/virtual-keyboard rather than routing through
    this portal.
  - `xdg-desktop-portal-generic` — newer standalone backend for compositors
    lacking their own portal, speaks `ext-*`/`wlr-*` protocols directly.
- **Permission/consent prompts:** every RemoteDesktop-portal session
  requires an explicit, per-session user consent dialog (screen-picker-style,
  like the ScreenCast portal) — not a silent/background grant. Persistent
  "remember this choice" behavior is inconsistent and still an open request
  (deskflow issue #8869, "Persist Remote Desktop XDG Desktop Portal dialog");
  the existence of a community tool built specifically to auto-click through
  this dialog (`deskflow/accept-portal-dialog`) is itself evidence the
  friction is real. For a dictation app that must inject transparently and
  repeatedly, this dialog is a first-class UX cost, not a one-time setup
  step — expect it at minimum once per app-launch/session, compositor-
  dependent.

### libei / libeis (Peter Hutterer, Red Hat)

- Announced stable (1.0.0) May 2023. **Current release: libei 1.6.0**,
  packaged in Arch `extra`, built 2026-05-15 — actively maintained through
  mid-2026, not stalled.
- **GNOME/Mutter:** shipped and in production. `gnome-remote-desktop`
  switched to libei starting with GNOME 45 (2023); this is default/shipped
  plumbing, not opt-in, in current GNOME (Ubuntu 26.04, Fedora 44 both ship
  it).
- **KDE/KWin:** active but less mature — KWin's libeis backend
  (invent.kde.org/plasma/kwin MR !5496) exposes **no public listening
  socket**; clients must go through the consent-gated RemoteDesktop portal
  only. A follow-up MR (!6178, July 2024) shows KDE actively tuning
  Xwayland-app consent-prompt friction, and a draft MR (!5412) explores
  libei for input-capture (pointer confinement) separately from injection.
  Net: functional, portal-gated like GNOME, but had roughly two more years
  of production hardening on GNOME by the time KDE's backend appeared.
- **wlroots:** no evidence of adoption as a primary path — wlroots
  compositors keep favoring the simpler, unmediated (no consent dialog, no
  portal indirection) `virtual-keyboard-unstable-v1` protocol.
- **Rust ecosystem signal:** `wdotool` (github.com/cushycush/wdotool),
  an xdotool-compatible tool built on libei + wlroots protocols, shows
  libei is consumable from Rust outside GNOME/KDE's own stacks, but is a
  small/new project — not yet a dependency to build production code on.

### What Handy / VoiceInk / similar tools actually ship (2026)

- **Handy** (github.com/cjpais/Handy) README states explicitly: "Limited
  support for Wayland display server." Tool selection by platform: X11 →
  `xdotool`; Wayland → `wtype` (preferred) or `dotool`; fallback `enigo`,
  which Handy's own README calls out as having "limited compatibility,
  especially on Wayland." Open issues corroborate real breakage: #429
  (first character dropped on GNOME/Wayland direct-paste), #121 (clipboard
  paste fails under Wayland, multiple distros), #949 (global shortcuts
  broken on COSMIC/Sway/Hyprland — root cause: Tauri's global-shortcut
  plugin needs `zwp_keyboard_shortcuts_inhibit_manager_v1`), #1239 (0.8.2
  "completely fails on Wayland"). Handy's Discussion #718 (Feb 2026)
  independently enumerates the same three GNOME options this report found:
  (1) RemoteDesktop portal via DBus, (2) a GNOME Shell extension using
  Clutter to synthesize input directly ("less future-proof"), (3) a new
  wtype-compatible tool built against the RemoteDesktop portal — PR #689 is
  reportedly Handy's shipped answer, worth reading directly if/when this
  work is picked up.
- **VoiceInk** is macOS-only; not a Linux/Wayland comparator.
- **Vocalinux** (github.com/jatinkrmalik/vocalinux) is the most
  GNOME-friendly architecture found: it types via a **custom IBus input-
  method engine**, which works anywhere IBus is the active input method and
  sidesteps the virtual-keyboard-protocol gap entirely, falling back to
  `wtype` where available and `xdotool` for XWayland apps. This is a
  materially different strategy from Handy/enigo's direct-key-synthesis
  approach and worth remembering as an alternative architecture if a real
  Wayland backend is ever built.
- **enigo** crate ships separate, feature-flagged Linux backends: a raw-
  Wayland-protocol backend (virtual-keyboard-based, "limited compatibility"
  per Handy) and an experimental libei backend, both explicitly non-default
  and flagged as buggy by the crate's own docs. Open bug #302: Wayland text
  injection does not respect custom keyboard layouts (Colemak) — the same
  keysym/layout mapping defect independently reported for the KDE
  RemoteDesktop-portal path (KDE bug #489021), suggesting keyboard-layout
  correctness is a cross-cutting, currently-unsolved problem across nearly
  every Wayland synthetic-input path, not a single protocol's bug.

## 2. Clipboard

### wl-clipboard / wlr-data-control → ext-data-control-v1

- `wlr-data-control-unstable-v1` is now legacy, superseded by
  `ext-data-control-v1`, merged into wayland-protocols v1.39 (December
  2024); currently in "Staging" status (can still take backward-compatible
  changes).
- **Compositor adoption of `ext-data-control-v1`** (wayland.app adoption
  table, moderate confidence — not independently re-verified entry by
  entry): Cage, COSMIC, GameScope, Hyprland (0.52.1), Jay, KWin (6.6),
  Labwc, Louvre, Mir, Muffin, niri (25.11), phoc, river (0.3.13), sway
  (1.11), Treeland, Wayfire, Weston — essentially the entire
  wlroots/Smithay ecosystem plus KWin 6.6.
- **KWin specifically:** in-progress port from `wlr-data-control` to
  `ext-data-control-v1` (MR !6606, last edited 2025-04-10), blocked on the
  upstream spec landing — which it did, in v1.39 (Dec 2024) — so this
  should be near-merged or merged by 2026, but final merge status was not
  independently confirmed in this pass. Treat as "in progress, likely
  landed, unverified."
- **GNOME/Mutter still does not support `wlr-data-control` or (per
  available evidence) `ext-data-control-v1`** in-compositor. This is an
  explicit, long-standing design position: GNOME treats unmediated global
  clipboard listening as a privacy/security concern and requires clipboard
  access to go through a portal `org.freedesktop.portal.Clipboard` instead.
  Tracking issue gitlab.gnome.org/GNOME/mutter/-/work_items/524 is still
  open. A GNOME/KDE portal-backed clipboard implementation is described in
  an Algora bounty listing as planned/funded work whose deliverables
  ("Upstream MRs: GNOME portal backend, KDE portal backend") had not yet
  landed as of the bounty's posting — i.e., **not production-ready as of
  this research.**

### Is clipboard save/paste/restore reliable on Wayland?

**Not reliably, and for structurally different reasons than X11's known
failure modes:**

- **wlroots compositors:** mechanically possible via `wl-clipboard`/
  `ext-data-control-v1`, but Wayland's clipboard ownership model is
  asynchronous and event-driven rather than X11's synchronous "own the
  selection, answer requests on demand" model. A background clipboard
  manager, or another data-control listener, can race a restore. Documented
  in wl-clipboard-rs issues #8 and #39.
- **GNOME/KDE:** breaks more fundamentally — no reliable programmatic
  clipboard-manager-style access exists without the (currently unshipped on
  GNOME) portal Clipboard API, or a focus-stealing hack. Even once shipped,
  a portal-mediated clipboard round-trip would carry the same per-session
  consent-prompt friction as RemoteDesktop input injection.
- **Net:** the clipboard round-trip — our *primary* X11 injection strategy —
  is the **weakest** of the three Wayland injection options, and weakest
  precisely on GNOME, which is also the compositor with the least mature
  direct-injection story. GNOME is the hardest target on both axes
  simultaneously.

## 3. Active-window context

Wayland's security model deliberately denies global window introspection;
every compositor requires its own escape hatch.

| Compositor | Mechanism | Notes |
|---|---|---|
| Hyprland | `hyprctl activewindow` (sync, `.socket.sock`) + `activewindow` event on the async `.socket2.sock` event stream | First-class, documented, ecosystem-used (waybar, etc.). Stable for Hyprland's cadence but compositor-specific — no formal cross-release stability guarantee like a Wayland protocol has. |
| sway / i3 | `sway-ipc(7)`: `GET_TREE` (full JSON layout) + `SUBSCRIBE` to `window` events (`IPC_EVENT_WINDOW`) | Backwards-compatible with i3's IPC (predates Wayland); mature client library `i3ipc-python` exists. Most standardized/longest-lived of the compositor-native mechanisms. |
| KDE/KWin | Scripting API (`workspace.stackingOrder`, `client.caption`, `client.resourceClass`) via the KWin scripting console (Alt+F2 → "wm console") or an installed persistent KWin script exposing its own DBus interface; `org.kde.KWin` DBus service exists but no simple stable "get focused window" DBus one-liner was found | Functional, used by community tiling scripts (Krohnkite etc.), but requires writing/installing a script — not a single built-in call. Scripts must be reloaded each KWin session unless packaged properly. |
| GNOME Shell | **No built-in mechanism.** Requires a user-installed Shell extension exposing a custom DBus interface: "Window Calls" (extensions.gnome.org/extension/4724, `org.gnome.Shell.Extensions.Windows.List`), "Window Calls Extended", or "Focused Window D-Bus" (flexagoon/focused-window-dbus, purpose-built for this exact gap) | These extensions work by running inside Mutter's process and using internal Clutter/Mutter APIs otherwise firewalled from external processes — the same "less future-proof" pattern flagged for GNOME text injection. **GNOME is the only family requiring unofficial, user-opt-in setup** for this capability; there is no portal equivalent to RemoteDesktop for window introspection. |

## Capability matrix

Rows = compositor family. Columns = inject / clipboard / active-window.
Maturity ratings: 🟢 solid/production, 🟡 works but has real caveats,
🔴 unavailable or requires unofficial user setup.

| Compositor | Inject | Clipboard | Active-window |
|---|---|---|---|
| **GNOME** | 🔴 no `virtual-keyboard-v1`; 🟡 RemoteDesktop portal, libei-backed, shipped since GNOME 45 (2023) but per-session consent dialog every time | 🔴 no in-compositor data-control API by design; portal Clipboard API still unshipped as of this research (bountied, not delivered) | 🔴 no built-in path; requires a user-installed unofficial Shell extension (Window Calls / Focused Window D-Bus) |
| **KDE (KWin)** | 🟡 `virtual-keyboard-v1` implemented (moderate confidence); 🟡 RemoteDesktop portal, libeis-backed but portal-only (no public socket), open keysym/layout bug (#489021) | 🟡 `wlr-data-control` supported, `ext-data-control-v1` port in progress (MR !6606, likely landed by 2026, unconfirmed) | 🟡 scripting console / installable KWin script exposing DBus; no single stable built-in call |
| **Hyprland** | 🟢 `virtual-keyboard-v1` native, no consent dialog | 🟢 `ext-data-control-v1` adopted (0.52.1+) | 🟢 `hyprctl activewindow` + `.socket2.sock` events — first-class, documented |
| **sway / i3** | 🟢 `virtual-keyboard-v1` native, no consent dialog | 🟢 `ext-data-control-v1` adopted (1.11+) | 🟢 sway-ipc `GET_TREE`/`SUBSCRIBE window` — most standardized, i3-heritage |
| **generic wlroots** (river, labwc, etc.) | 🟢 `virtual-keyboard-v1` typically implemented across the wlroots/Smithay family | 🟢 `ext-data-control-v1` widely adopted | 🔴/🟡 no standard protocol; compositor-specific or absent — treat as unsupported unless verified per-compositor |

Cross-cutting risk (not compositor-specific): **keyboard-layout mismapping**
appears independently in the KDE RemoteDesktop-portal keysym path (KDE bug
#489021) and in enigo's Wayland backend (issue #302, Colemak) — i.e., this
is a defect shared by essentially every current Wayland synthetic-input
mechanism, not an isolated bug in one protocol.

## Trait implications for S13/S23

The `Injector` (`dictate-inject`, S13) and `ContextProvider`
(`dictate-context`, S23) traits are being designed against X11 today. To let
a real Wayland backend slot in later **without a refactor**, the trait
contracts today must not assume any of the following, all of which hold on
X11 but do not hold uniformly across Wayland compositors:

1. **Injection is not always synchronous or process-local.** X11
   `arboard+enigo` calls complete in-process, synchronously. A GNOME/KDE
   Wayland backend goes through an async D-Bus portal session with a
   consent handshake — `Injector::inject()` must be async (or return a
   future/handle) and must be able to represent "pending user consent" as a
   distinct state, not just success/failure. (Directly informs S13's
   existing "never silently drop text — on inject failure, text goes to
   clipboard + notification" contract: a consent-dialog timeout is a
   distinct failure mode from a hard error and should probably surface
   differently in the notification.)
2. **Injection may require one-time or per-session user consent that the
   caller cannot bypass programmatically.** The trait needs a way to signal
   "backend requires interactive authorization" up to the daemon/notification
   layer, separate from "backend unavailable" — these should not collapse
   into one error variant.
3. **Clipboard save/restore is not guaranteed atomic or fast.** Do not
   assume `set_clipboard(); inject_paste(); restore_clipboard()` completes
   before another process observes or claims the selection. The trait
   should treat clipboard restore as best-effort with a documented race
   window, not a guarantee — and per-app injection policy (paste|type|off)
   should be overridable specifically because clipboard-paste is the
   *least* reliable strategy on the two compositors (GNOME, and KDE
   pre-port) where it matters most.
4. **Window class/title/app-id is not always retrievable, and "not
   retrievable" is a normal, expected state — not an error.**
   `ContextProvider` must treat missing window context as a first-class
   value (e.g. `Option<WindowContext>` with a reason, not a `Result` that
   treats absence as failure), because on GNOME this is the *default* state
   unless the user has opted into installing a third-party Shell extension.
   Per-app policy resolution (S13/S23's app-category defaults) must degrade
   gracefully to a sane default profile when context is unavailable, not
   error out.
5. **The mechanism for getting context is compositor-specific and
   fundamentally different in shape per compositor** — a socket protocol
   (Hyprland, sway), a scripted DBus service (KWin), or an external
   extension's DBus interface (GNOME) — not a single Wayland API. The trait
   should be designed so a "provider" is pluggable per compositor
   (essentially what the plan's "Wayland provider stub (compositor IPC
   adapters listed in R3)" already anticipates), and should not bake in an
   assumption that one implementation covers all of Wayland the way the X11
   backend covers all of X11.
6. **Do not assume "same keysym in → same character out."** Keyboard-layout
   mismapping is a live, unresolved cross-protocol issue on Wayland
   (KWin/portal and enigo both affected as of 2026). If/when a Wayland
   `Injector` backend is built, plan for a text-injection path (raw text →
   compositor types via IME/text-input protocol) as materially more
   layout-safe than a keysym-synthesis path — worth noting for backend
   selection, not something to solve in the trait shape today.

None of the above requires changing the X11 implementation now; they are
constraints on the trait *signature* (async injection outcome, optional/
reasoned context, pluggable provider-per-backend) so that adding Wayland
backends later is additive, not a breaking rewrite.

## References

See the inline citations above; primary sources used (dates noted where
available):

- GNOME Mutter issues #4124, work items #1974 and #524 (virtual-keyboard-v1
  and wlr-data-control, both still open as of this research)
- atx/wtype README and issues #29, #34, #45
- swaywm/wlroots PR #999 (virtual-keyboard-unstable-v1 origin, 2019)
- xdg-desktop-portal RemoteDesktop spec and `RemoteDesktop.xml`
- xdg-desktop-portal-kde `remotedesktop.cpp`; KDE bug #489021 (keysym/layout)
- deskflow issues #8869 and `accept-portal-dialog` tool (consent-dialog friction)
- libei 1.0.0 announcement (May 2023); Arch package libei 1.6.0 (built 2026-05-15)
- GNOME/gnome-remote-desktop MR !198 (libei switch, GNOME 45, 2023)
- KWin MRs !5496 (libeis backend), !6178 (Xwayland consent tuning, Jul 2024), !5412 (Eis draft)
- wayland-protocols 1.39 announcement (Dec 2024, ext-data-control-v1)
- wl-clipboard issue #242; KWin MR !6606 (ext-data-control port, last edited 2025-04-10)
- wl-clipboard-rs issues #8, #39 (clipboard race conditions)
- Hyprland Wiki IPC page; sway-ipc(7) man page; i3ipc-python docs
- KWin scripting API docs (develop.kde.org)
- GNOME Shell extensions: Window Calls, Window Calls Extended, Focused Window D-Bus
- cjpais/Handy README, issues #429, #121, #949, #1239, Discussion #718, PR #689
- jatinkrmalik/vocalinux (IBus-engine architecture)
- enigo crate docs (crates.io/lib.rs) and issue #302 (Colemak layout bug)
- cushycush/wdotool (libei-based Rust tool, evidence of libei's Rust-ecosystem reach)

Research gathered via web search 2026-08-07; several claims (KWin MR !6606
final merge status, exact `ext-data-control-v1` per-compositor adoption
table from wayland.app, KDE's virtual-keyboard-v1 support) carry moderate
rather than high confidence — flagged inline above. One `wayland.app`
auto-extracted claim (that Mutter 49.2 supports `virtual-keyboard-unstable-v1`
and `wlr-foreign-toplevel-management-unstable-v1`) was discarded as
contradicted by corroborated, still-open GNOME GitLab tracker evidence.
