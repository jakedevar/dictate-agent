# dictate-agent wire protocol v1

The message contract shared by the local control plane (S02), the network API
server (S33), and the Tauri UI (S32). Defined in `crates/dictate-proto`;
this document is the language-neutral reference so S32 and S33 can implement
against it without reading Rust.

Everything here is pinned by `crates/dictate-proto/tests/golden.rs`. If this
document and that test disagree, **the test is right** — please fix the doc.

- **Protocol version:** 1
- **Binary frame version:** 1 (versioned independently)
- **Encoding:** UTF-8 JSON; binary audio frames are little-endian

---

## 1. Compatibility rule

Within one protocol version the format is **additive-only**.

| | |
|---|---|
| **MAY be added** | a new optional field (with a default that preserves old behavior); a new variant of an open enum; a new command, event, result, or error code; a new capability flag (defaults to `false`) |
| **MUST NOT change** | renaming or removing a field or variant; changing a field's type, units, or meaning; making an optional field required; changing an enum's tagging or a struct's nesting |

Breaking any "MUST NOT" requires incrementing the protocol version.

### Handling the unknown

| Situation | Required behavior |
|---|---|
| Unknown **field** | Ignore it. No type uses `deny_unknown_fields`. |
| Unknown **open-enum value** (state, route, mode, error code, skip reason, inject method, audio encoding) | Accept and preserve it verbatim. A relay must be able to re-emit it unchanged. |
| Unknown **event** or **result** | Degrade to a generic "unknown" and carry on. |
| Unknown **command** | **Fail.** The server must answer `unsupported_command`. |
| Unknown **binary frame kind** | Skip it using the header's explicit length; the stream stays in sync. |

The command/event asymmetry is deliberate: a client that ignores an event it
does not understand loses nothing, but a server that silently drops a command
leaves the caller waiting forever for an effect that will never happen.

---

## 2. Envelope

Used on multiplexed connections (the UDS socket, the WebSocket). `kind`
discriminates direction, `v` is the protocol version, `id` correlates a
response to its request.

```json
{"kind":"request", "v":1, "id":1, "command":{"type":"stop"}}
{"kind":"response","v":1, "id":1, "result":{"type":"ack"}}
{"kind":"response","v":1, "id":1, "error":{"code":"busy","message":"..."}}
{"kind":"event",   "v":1, "event":{"type":"state_changed", ...}}
```

- `id` may be a JSON number or a string. `42` and `"42"` are different ids.
- A response carries exactly one of `result` or `error` — never both, never
  neither.
- Events carry no `id`; they are unsolicited.

**The envelope is optional.** `POST /v1/transcribe` takes a bare command body
and returns a bare result body — do not wrap a stateless HTTP request in a
correlation id it does not need.

**Framing.** One WebSocket text message is one envelope. On the unix socket,
messages are newline-delimited JSON (safe: JSON escapes newlines inside
strings, so a raw `\n` is always a delimiter).

---

## 3. Handshake and capabilities

### Request

```json
{"type":"handshake",
 "protocol_version":1,
 "supported_versions":[1],
 "client":{"name":"dictate-cli","version":"0.2.0","kind":"cli"}}
```

`client.kind` ∈ `cli` | `desktop_ui` | `remote` | `automation`. It is a **hint**
— authorization is the server's decision, never the client's self-declaration.

### Response

```json
{"type":"handshake",
 "protocol_version":1,
 "supported_versions":[1],
 "server":{"name":"dictated","version":"0.2.0"},
 "capabilities":{ "features":{...}, "audio_formats":[...], "routes":[...], "limits":{...} }}
```

If there is no version overlap the server answers `unsupported_version` rather
than closing the connection, so the client can report why it was refused.

### Capabilities are per-connection, not per-server

The same daemon offers text injection on its local socket and must refuse it to
a phone on the LAN. **Do not cache capabilities across transports.**

`features` — every flag is a boolean that **defaults to `false`**. An omitted or
unrecognized flag means "not permitted", which is the fail-safe direction.

| Flag | Grants |
|---|---|
| `text_injection` | may cause text to be typed into the host's focused app |
| `host_capture` | may switch on the host's microphone |
| `transcribe_upload` | may submit its own audio for transcription |
| `streaming_audio` | may open a binary audio stream |
| `partial_transcripts` | server emits `partial` events (currently always `false`) |
| `audio_level_events` | server emits `audio_level` events |
| `history_read` | may query history |
| `history_write` | may purge history |
| `dictionary_read` / `dictionary_write` | may read / modify the dictionary |
| `snippets_read` / `snippets_write` | may read / modify snippets |
| `config_read` / `config_write` | may read / modify configuration |
| `wake_word` | wake-word listener exists in this build |
| `headless` | no desktop session attached; injection can never become available |
| `privacy_mode` | transcripts are not being persisted |
| `diagnostics` | may run `diagnose` (names host paths, installed models, permission problems) |

`routes` restricts which routes the connection may invoke — a subset decision
rather than an on/off one. A remote client may be allowed `type` (returned as
text) while being denied `timer`, which would run `systemd-run` on the host.

`routes` is **deny-by-default**: an empty list permits *nothing*, and every
server must populate it explicitly. This matches `features` beside it, where an
omitted flag reads as "not permitted". The alternative — reading an empty list
as "unspecified, therefore everything" — was fail-*open* on precisely the paths
that execute code on the host (`timer` runs `systemd-run`, `local` invokes the
LLM), and it meant any peer that simply omitted the field was granted both.

`limits` — `max_message_bytes` (default 1048576), `max_audio_frame_bytes`
(default 1048576), `max_audio_ms` (optional), `max_concurrent_sessions`
(default 1).

Two reference capability sets:

| | local (trusted) | remote (transcription only) |
|---|---|---|
| `text_injection` | ✅ | ❌ |
| `host_capture` | ✅ | ❌ |
| `transcribe_upload` / `streaming_audio` | ✅ | ✅ |
| config / dictionary / snippets / history | ✅ | ❌ |
| `routes` | all five | `["type"]` |

---

## 4. State machine

```
Idle → Recording → Transcribing → Formatting → Injecting → Done
         │             │              │            │
         └─────────────┴──────────────┴────────────┴──→ Error
         └─────────────┴──────────────┴────────────┴──→ Cancelled
```

States: `idle` `recording` `transcribing` `formatting` `injecting` `done`
`error` `cancelled`. Terminal: `done`, `error`, `cancelled`.

Transition rules:

1. Any non-terminal state may move to any terminal state — **cancellation is
   reachable from everywhere**.
2. A terminal state may only reset to `idle`.
3. Otherwise a transition must move **strictly forward**. Stages may be skipped
   (VAD disabled, LLM skipped by the skip-rules, or a remote client ending at
   `formatting` because it takes delivery instead of having text injected), but
   never revisited.
4. An unknown state is **not** treated as terminal — a newer daemon may have
   added an intermediate stage.

---

## 5. Commands

All commands are objects tagged with `type`.

| Command | Fields | Required feature |
|---|---|---|
| `handshake` | *(Hello, flattened)* | — |
| `start_dictation` | `mode`, `options?` | `host_capture` |
| `toggle` | — | `host_capture` |
| `stop` | — | any session capability |
| `cancel` | — | any session capability |
| `get_status` | — | — |
| `subscribe` | `events?` (names; empty = all) | — |
| `unsubscribe` | — | — |
| `get_config` | `path?` | `config_read` |
| `set_config` | `entries[]`, `document?`, `dry_run?`, `base_revision?` | `config_write` |
| `list_dictionary` | `query?`, `limit?` | `dictionary_read` |
| `upsert_dictionary_entry` | `entry` | `dictionary_write` |
| `delete_dictionary_entry` | `id` | `dictionary_write` |
| `list_dictionary_suggestions` | `limit?` | `dictionary_read` + `history_read` |
| `list_snippets` | `query?`, `limit?` | `snippets_read` |
| `upsert_snippet` | `snippet` | `snippets_write` |
| `delete_snippet` | `id` | `snippets_write` |
| `query_history` | `query` | `history_read` |
| `get_history_analytics` | — | `history_read` |
| `purge_history` | — | `history_write` |
| `transcribe_audio` | `audio`, `options?` | `transcribe_upload` |
| `begin_audio_stream` | `format`, `options?` | `streaming_audio` |
| `end_audio_stream` | `stream_id` | `streaming_audio` |
| `diagnose` | `quick?` | `diagnostics` |

`handshake`, `get_status`, `subscribe`, and `unsubscribe` need no capabilities —
they are how a client discovers everything else. A command whose feature is not
granted must be answered `forbidden`.

`mode` ∈ `toggle` | `push_to_talk` | `one_shot` | `wake_word` (default `toggle`).

`toggle` atomically starts a toggle-mode session when idle, or stops the
active recording session. The daemon resolves that branch in its single-writer
engine actor; clients MUST use it for toggle controls rather than reading
`get_status` and then sending `start_dictation` or `stop`. A toggle during a
later pipeline stage is answered `busy`.

### `options` (SessionOptions)

Every field is optional, and **omitted means "use the server's configured
default", never "off"**.

| Field | Meaning |
|---|---|
| `route` | force a route, bypassing the router |
| `inject` | whether to inject. Omitted lets the server decide from capabilities — correct for both local (inject) and remote (don't). Asking `true` without `text_injection` is `forbidden`, **not** a silent downgrade. |
| `format_llm` | run the LLM formatting pass |
| `use_dictionary` | apply dictionary and snippets |
| `language` | BCP-47 hint |
| `app` | target app, for per-app profiles |
| `privacy` | do not persist this session |

### Configuration

Settings are addressed by **dotted path** with an opaque JSON value:

```json
{"type":"set_config","entries":[{"path":"whisper.model","value":"large-v3-turbo"}]}
```

The config schema is deliberately **not** modeled in the protocol. Every key a
later slice adds would otherwise be a protocol change, and the UI would need a
rebuild to expose a setting the daemon already supports. The cost is that the
protocol cannot type-check a setting; validation is the daemon's job, reported
as `config_invalid`.

**Implemented by `dictated` since S32**, for local connections only
(`remote_transcription_only` grants neither capability):

- `get_config` answers a `config` result. `values` is every key with the value
  the configuration file resolves to, built-in defaults included — what the
  daemon runs after a restart. A whole-tree read also carries
  `file: {path, exists, document, revision}` (the user's file, comments and
  all, and an opaque token that changes whenever its text does) and the
  loader's `warnings` (unknown keys, mapped legacy spellings — the list
  `dictated --check-config` prints). If the file on disk cannot be resolved,
  `errors` says why and `values` falls back to the running configuration.
  `path` reads a subtree; a path that names nothing is `not_found`.
- `set_config` either edits `entries` in place or replaces the file with
  `document` (the raw-editor path; send one or the other, never both —
  `invalid_params`). A `null` value removes the key so its default applies;
  an object value is merged key by key into that section. Entry edits keep the
  file's comments, ordering, legacy sections and unknown keys, and keep the
  inline comment of a replaced value.
- Every write is validated by the same loader the daemon starts with. Anything
  it would refuse — wrong type, bad value, unknown key, unparseable TOML — is
  `config_invalid` with `detail: {"path": <offending key or null>, "errors":
  [...]}`, and nothing is written.
- Writes are atomic (temp file, fsync, rename; a symlinked file is written
  through to its target) and keep one backup, `<file>.bak`.
- The result lists `applied` (keys whose resolved value changed) and
  `restart_required` (keys whose value on disk differs from what the running
  daemon uses — a plain read reports these too). Only `history.privacy_mode`
  applies live today; `get_status` reports the privacy mode in force.
- `dry_run: true` validates and reports without writing; `file.document` is
  then the text that would have been written (`file.revision` stays that of
  the file on disk).
- **No lost updates.** Send the `file.revision` an edit was based on as
  `base_revision`; if the file has changed since (another client's write or a
  hand edit), the write is `conflict` with `detail: {"revision": <current>}`
  and nothing is written. Independently of `base_revision`, the daemon
  re-reads the file just before the rename and answers `conflict` if it
  changed while the write was being validated. Without `base_revision` a
  write applies to whatever the file holds at the time.

```json
{"type":"set_config","entries":[],"document":"[grammar]\nenabled = true\n","dry_run":true}
← {"type":"config","values":{...},"applied":["grammar.enabled"],"restart_required":["grammar.enabled"],
   "file":{"path":".../config.toml","exists":true,"document":"[grammar]\nenabled = true\n"},"dry_run":true}
```

---

## 6. Audio input

Three sources, because there are three real transports:

```json
{"source":"inline","format":{...},"data":"<base64>"}   // single JSON request
{"source":"stream","stream_id":3}                       // binary frames on this connection
{"source":"body","format":{...}}                        // HTTP body / multipart
```

`format` is `{"encoding":..., "sample_rate_hz":?, "channels":?}` with encoding
∈ `pcm_f32le` | `pcm_s16le` | `wav`. Rate and channels are optional for `wav`,
which describes itself. The pipeline's native format is 16 kHz mono
`pcm_f32le`; anything else is resampled.

Inline audio is base64 (standard alphabet, padded) and ~33% larger than the
binary paths — honor `limits.max_message_bytes`.

### `transcribe_audio` on the local socket (S03)

The local daemon implements `transcribe_audio` for `source: "inline"`. The
`stream` and `body` sources belong to the network transports (S33) and are
answered `capability_unavailable` here.

- **Formats.** `wav` (RIFF; PCM 8/16/24/32-bit or 32-bit float, 4–192 kHz,
  1–8 channels), or raw `pcm_s16le` / `pcm_f32le` whose `format` must declare
  `sample_rate_hz` and `channels` (a missing one is `invalid_params`). The
  daemon downmixes to mono and resamples to 16 kHz with a band-limited
  (Kaiser-windowed sinc) resampler — never by dropping samples, which would
  alias.
- **Limits.** `limits.max_audio_ms` bounds the clip (checked from the WAV
  header before any sample is decoded) and `limits.max_message_bytes` bounds
  the request. Exceeding either is `payload_too_large` with `detail`
  `{limit, actual}`. Undecodable audio — bad header, truncated data, a partial
  final frame, NaN samples, an out-of-range rate or channel count — is
  `audio_format_unsupported`.
- **Session semantics.** The upload runs as an ordinary session in the
  engine's single slot: `busy` when one is already running, cancellable with
  `cancel` (from the uploading connection, from another connection with
  `host_capture`, or by SIGUSR2), and cancelled automatically if the uploading
  connection goes away. It never touches the microphone, media players or
  earcons, and it skips the `recording` state (`idle → transcribing → …`).
  `stop` on it is `invalid_state`.
- **Delivery.** The response to the request **is** the transcript
  (`result.type = "transcript"`); it is held until the pipeline settles, so a
  client must not apply a short read timeout. `session_started`-style events are
  published to subscribers as usual (the uploading connection receives its own
  events after the response, because it is busy waiting for it).
- **Hang-up.** Do not half-close the write side of the socket while waiting for
  the transcript: the daemon reads EOF as "the caller is gone" and cancels the
  session, so that an interrupted `dictate transcribe --inject` types nothing.
  Keep the connection open until the response arrives.
- **Injection.** Off unless `options.inject` is `true` *and* the connection has
  `text_injection` (`forbidden` otherwise). Unlike `start_dictation`, an
  omitted `inject` never means "the connection's default": an upload delivers
  text to its caller unless told otherwise.
- **Timings.** For an upload the `capture` stage reports the decode + resample
  time (`ran`), since acquiring the audio is what that stage measures.
- **Privacy.** `options.privacy: true` (or a daemon in privacy mode) persists
  nothing about the session, exactly as for a live dictation.

---

## 7. Binary audio frames

Little-endian, 20-byte fixed header + payload:

| Offset | Size | Field |
|---|---|---|
| 0 | 4 | magic `"DCTA"` |
| 4 | 1 | frame version (1) |
| 5 | 1 | kind: 1 = audio, 2 = end, 3 = abort |
| 6 | 2 | flags — bit 0 = LAST |
| 8 | 4 | `stream_id` |
| 12 | 4 | `seq`, per-stream, from 0 |
| 16 | 4 | `payload_len` |
| 20 | … | payload |

Sample rate, channels, and encoding are **absent by design** — declared once in
`begin_audio_stream`. Repeating them 50×/second would be pure overhead and would
let a stream contradict its own declaration mid-flight.

`payload_len` is explicit so the same encoding works on a WebSocket (message
boundaries given) and on the unix socket (a byte stream). Decoders **must**
reject `payload_len` > `max_audio_frame_bytes` (default 1 MiB) *before*
allocating — it is attacker-controlled.

An unknown `kind` is not an error: skip `20 + payload_len` bytes and continue.

---

## 8. Events

| Event | Fields |
|---|---|
| `state_changed` | `session_id`, `from`, `to`, `at_ms?` |
| `partial` | `session_id`, `seq`, **`hypothesis`**, `at_ms?` |
| `final` | `session_id` + *Transcript, flattened* |
| `injection_resolved` | `session_id`, `outcome` |
| `error` | `session_id?`, `error` |
| `audio_level` | `session_id`, `rms`, `peak?`, `at_ms?` |

### ⚠ A partial is never injectable text

`partial` carries **`hypothesis`**, `final` carries **`text`**. The field names
differ on purpose: a client reading `.text` off a partial gets `undefined`, so
the mistake fails immediately instead of silently typing a half-finished
sentence.

A hypothesis has not been through corrections, the dictionary, snippets, the
formatting rules, or the LLM pass, and will be contradicted by the `final` that
follows. It is a HUD affordance only. Each `partial` **replaces** the previous
one for that session (they are not appended); `seq` orders them.

Partials are currently never emitted — `features.partial_transcripts` is
`false`. The event is specified now so enabling it later is additive.

`audio_level` is high-frequency (throttle to ~30/s). Clients must tolerate these
being dropped or coalesced; no correctness depends on receiving every one.

---

## 9. Transcript

The same payload appears flattened in the `final` event and as the `transcript`
result of `transcribe_audio`. Sharing it is why the HTTP and WebSocket paths
cannot drift apart.

```json
{
  "text": "Hello there.",
  "raw_text": "hello there",
  "route": "type",
  "timings": { ... },
  "injection": {"status":"injected","method":"paste","chars":12},
  "word_count": 2,
  "model": "large-v3-turbo"
}
```

Routes: `type` | `timer` | `local` | `edit` | `command`. `timer` (systemd-run)
and `local` (Ollama) are preserved features, not legacy.

### Timings

Six stages — `capture`, `vad`, `stt`, `fmt_rules`, `fmt_llm`, `inject` — plus
`total_ms` and `audio_ms`. Each stage is one of:

```json
{"status":"ran","ms":412.0}
{"status":"skipped","reason":"below_min_words"}
{"status":"failed","ms":3000.0,"error":"connection refused"}
{"status":"not_reported"}
```

The four states exist so three situations stay distinguishable that a bare
number would conflate:

- a stage that **ran and cost ~0ms** (the pure-Rust rules layer) — `ran` with a
  near-zero `ms`;
- a stage **deliberately not run** — `skipped`, carrying why;
- a stage that **burned time and then failed open** (the LLM pass when Ollama
  hangs) — `failed`; that time is still in the user's latency budget and must
  not vanish from the accounting;
- a stage **nobody reported** — `not_reported`, also the default, so a field
  added by a future revision reads as "no data" rather than a fabricated zero.

`skipped` reasons: `disabled`, `below_min_words`, `route_not_eligible`,
`not_supported`, `no_speech_detected`, `not_permitted`,
`dependency_unavailable`, `cancelled`, `too_long` (S21: the input exceeded the
stage's length limit — the LLM pass above `[format.llm.chunking] max_words` —
and the previous stage's output was kept).

`total_ms` is reported, not derived: it includes scheduling and queueing that
belong to no single stage, so it is normally *greater* than the sum. The gap is
itself the signal. `audio_ms` allows a real-time factor.

This is the field the ≤1.0s p50 budget is measured from, and the one S12 records
real turbo-CUDA numbers through.

### Injection outcome

Deliberately **not** a boolean:

```json
{"status":"injected","method":"paste|keystroke|input_method","chars":12}
{"status":"awaiting_consent","backend":"portal","consent_id":"req-7"}
{"status":"consent_denied","backend":"portal","detail":"..."}
{"status":"unavailable","backend":"none","reason":"no display server"}
{"status":"delivered"}
{"status":"skipped","reason":"not_permitted"}
{"status":"failed","error":{...}}
```

- **`awaiting_consent` is not terminal.** On GNOME/KDE Wayland, synthetic input
  goes through an asynchronous, user-consent-gated portal, so a `final` event may
  report a pending injection with the resolution arriving later as
  `injection_resolved`. A client **must not** report success or failure while
  this status stands. X11 never produces it.
- **`delivered`** means the caller received text and nothing was injected
  anywhere — the normal outcome for `POST /v1/transcribe`. It is a success, not
  a skip and not a failure.
- **`unavailable`** means the capability does not exist here (headless,
  unsupported compositor, LAN connection). Nothing malfunctioned.

---

## 10. Errors

```json
{"code":"busy","message":"a session is already running","detail":{...},"retry_after_ms":500}
```

`detail` and `retry_after_ms` are optional. `message` must not contain
transcript text when the daemon is in privacy mode.

| Group | Codes | HTTP |
|---|---|---|
| Protocol | `unsupported_version`, `unsupported_command`, `capability_unavailable`, `consent_required` | 501 |
| | `malformed_request`, `invalid_params`, `audio_format_unsupported`, `config_invalid`, `handshake_required` | 400 |
| Auth | `unauthorized` | 401 |
| | `forbidden`, `consent_denied` | 403 |
| | `payload_too_large` | 413 |
| | `rate_limited` | 429 |
| State | `busy`, `invalid_state`, `conflict` | 409 |
| | `not_found`, `no_active_session` | 404 |
| | `cancelled` | 499 |
| | `timeout` | 504 |
| Pipeline | `model_unavailable`, `audio_device_error` | 503 |
| | `stt_failed`, `formatting_failed`, `injection_failed`, `history_error`, `internal` | 500 |

Retryable: `busy`, `rate_limited`, `timeout`, `internal`, `audio_device_error`,
`model_unavailable`. An **unknown** code must not be retried blindly and maps to
500.

Authentication and authorization codes are *modeled* here but not implemented by
the protocol crate — bearer-token auth is S33's mechanism.

---

## 11. Results

| Result | Payload |
|---|---|
| `ack` | — |
| `handshake` | ServerHello |
| `status` | Status |
| `session_started` / `session_stopped` / `session_cancelled` | `session_id` |
| `config` | `values`, `path?`, `applied[]`, `restart_required[]`, `file?` (`path`, `exists`, `document`, `revision?`), `warnings[]`, `errors[]`, `dry_run?` |
| `dictionary` / `snippets` | `entries[]` / `snippets[]` |
| `dictionary_entry` / `snippet` | the stored record, with server-assigned `id` |
| `deleted` | `id` |
| `history` | `items[]`, `total?`, `next_offset?` |
| `history_analytics` | `overall_wpm?`, `words_today`, `words_by_day[]`, streaks |
| `transcript` | Transcript |
| `audio_stream_opened` | `stream_id`, `session_id` |
| `diagnostics` | `checks[]` (see §12b) |

`status` reports `state`, `session?`, `daemon{name,version,protocol_version,pid?,uptime_ms?}`,
`model{name,loaded,backend?}`, and this connection's `capabilities`.
`model.backend` (`cuda` / `cpu`) is load-bearing: a CPU-fallback latency number
is not comparable to a CUDA one.

Two further **optional** fields exist because a silently failing dependency is
invisible everywhere else:

- `formatter{enabled, model?, health, detail?}` — the formatting pass's
  *observed* health: `disabled` | `unchecked` | `ok` | `model_missing` |
  `unreachable` | `failing`. The pass fails open, so a formatter whose model is
  not installed looks exactly like one with nothing to fix; `model_missing` is
  how a client tells them apart.
- `audio{capture_enabled, input_open, pre_roll_ms?}` — `input_open: true` while
  idle means the microphone is held open for pre-roll; `pre_roll_ms = 0` keeps it
  `false` between recordings; `capture_enabled: false` means audio-less mode.

An absent field means "this daemon does not report it", never "healthy".

### History records

A history entry's `text` being **absent** (privacy mode, or a session that
failed before producing text) is distinct from an **empty string** (the user
said nothing). Do not conflate them.

`purge_history` removes every persisted interaction (and its FTS index entry)
while keeping the daemon's SQLite connection open. A daemon in global privacy
mode, or a `start_dictation` request with `options.privacy: true`, stores no
row at all; this is stronger than masking transcript text after the fact.

---

## 12. Reference flows

### Local dictation (S02) — implemented

Served by `dictated` over `$XDG_RUNTIME_DIR/dictate-agent/dictated.sock`.
Connections there are granted `Capabilities::local_trusted`, minus anything the
host cannot actually do (no display server withdraws `text_injection` and sets
`headless`) and minus features this build does not have (snippets, audio
streaming; config is implemented since S32). Those last are answered `unsupported_command` rather than `forbidden`
— "this build cannot" is a different fact from "you may not", and only one of
them is worth showing the user a setting for.

Two rules the daemon enforces that the wire format does not carry:

- **Session ownership.** `toggle`, `stop`, and `cancel` carry no session id, so the daemon
  decides: a connection may control the session it started, a trusted-local
  connection may also control an unowned host session (which is what lets one
  `dictate` invocation start a session and the next one stop it), and a signal
  outranks both. A connection without `host_capture` can never touch another
  peer's session — the property S33 needs before a phone is on the LAN.
- **The injection commit point.** Cancellation is reachable from every
  non-terminal state, including `injecting`, right up to the moment text is
  handed to the injector. Past that the injection cannot be undone, so a
  late `cancel` is answered `conflict` rather than reported as a success over
  text that has already been typed.



```
→ {"kind":"request","v":1,"id":1,"command":{"type":"handshake",...}}
← {"kind":"response","v":1,"id":1,"result":{"type":"handshake",...}}
→ {"kind":"request","v":1,"id":2,"command":{"type":"subscribe"}}
← {"kind":"response","v":1,"id":2,"result":{"type":"ack"}}
→ {"kind":"request","v":1,"id":3,"command":{"type":"start_dictation","mode":"push_to_talk"}}
← {"kind":"response","v":1,"id":3,"result":{"type":"session_started","session_id":"s1"}}
← {"kind":"event","v":1,"event":{"type":"state_changed","from":"idle","to":"recording",...}}
← {"kind":"event","v":1,"event":{"type":"audio_level","rms":0.3,...}}          × N
→ {"kind":"request","v":1,"id":4,"command":{"type":"stop"}}
← {"kind":"event","v":1,"event":{"type":"state_changed","from":"recording","to":"transcribing",...}}
← {"kind":"event","v":1,"event":{"type":"final","session_id":"s1","text":"...","timings":{...}}}
← {"kind":"event","v":1,"event":{"type":"state_changed","from":"injecting","to":"done",...}}
```

### One-shot upload (S33)

```
POST /v1/transcribe        Authorization: Bearer <token>
body = WAV bytes; query/JSON params map to
  {"type":"transcribe_audio","audio":{"source":"body","format":{"encoding":"wav"}},
   "options":{"inject":false}}

200 → {"type":"transcript","text":"...","route":"type","timings":{...},
       "injection":{"status":"delivered"}}
```

Errors return the error object with the HTTP status from §10.

### Streamed audio (S33)

```
→ {"type":"begin_audio_stream","format":{"encoding":"pcm_f32le","sample_rate_hz":16000,"channels":1}}
← {"type":"audio_stream_opened","stream_id":3,"session_id":"s2"}
→ <binary frame kind=1 stream=3 seq=0 …>                                       × N
→ <binary frame kind=2 stream=3 seq=N flags=LAST>     (or {"type":"end_audio_stream","stream_id":3})
← {"kind":"event",...,"event":{"type":"final","session_id":"s2","text":"...", "injection":{"status":"delivered"}}}
```

---

## 12b. Diagnostics

`diagnose` (`quick?: true` skips the slow checks) answers
`{"type":"diagnostics","checks":[…]}`. Each check is
`{id, title, status, detail, fix?}`; `status` ∈ `ok` | `warn` | `fail` |
`skipped`, and `fix` — a one-line remedy — is present for every `warn` and
`fail`. Warnings describe a working daemon the user may want to adjust; only
`fail` makes a report unhealthy (`dictate doctor` exits non-zero on it).

Stable check `id`s (order is the order to read them):

| `id` | Verifies |
|---|---|
| `daemon` | reachable, version, pid (added by the client) |
| `config` | the config file parsed; lists unknown keys and mapped legacy values |
| `stt_model` | the model file exists and, for catalog models, matches the pinned SHA-256 |
| `stt_backend` | the loaded backend is what the config asked for (CUDA verified against the GPU's process list when `nvidia-smi` is available) |
| `formatter` | the formatting pass's observed health (see `status.formatter`) |
| `dictionary` | the personal dictionary database opened (Skipped when `[dictionary] enabled = false`; Fail when it could not be opened, in which case dictation runs without a dictionary and the dictionary capabilities are withdrawn) |
| `ollama` | the Ollama server answers |
| `grammar_model` | the `[format.llm]` model ladder: Ok when the preferred (first) model is installed, Warn when a fallback rung will be used, Fail when none is installed (lists the installed alternatives). The id predates S21 and is kept stable |
| `local_model` | the `local` route's model is installed |
| `injection` | a display is present and an injection backend is available |
| `hotkeys` | every configured input device is readable, when hotkeys are enabled |
| `audio_input` | capture mode, and whether the microphone is held open while idle |
| `notifications` | a desktop session bus is reachable |
| `tools` | external programs the daemon shells out to |
| `legacy_pid` | who holds the legacy `dictate.pid` that `scripts/dictate-toggle` signals |

---

## 12a. Application context

`AppContext` names where a session's text is going. The context engine (S23)
resolves it from the focused window at session start and stop; a network client names
its target through `options.app` instead.

```json
{"app": "slack", "title": "#eng - Slack", "category": "chat", "profile": "work-chat"}
```

| Field | Meaning |
|---|---|
| `app` | Stable identifier: lowercased X11 `WM_CLASS` class, or the client-supplied name |
| `title` | Window title at context resolution (start or stop). Optional; privacy-sensitive, never persisted in privacy mode |
| `category` | Open enum: `terminal`, `editor`, `browser`, `chat`, `email`, `document`, `other` (default) |
| `profile` | Name of the configured profile that matched, if any |

`Tone` is the register the formatting pass aims for — open enum `formal`,
`neutral` (default), `casual`, `very_casual`. Neutral cleans text up without
shifting register; the others are opt-in per category or profile.

**Absence is normal.** A headless host, the network API, and GNOME/Wayland
without a Shell extension have no window context. Every consumer must treat a
missing context as an ordinary input with a defined default, never as an error.

---

## 13. Implementation checklist

**Any client**
- [ ] Ignore unknown fields; do not fail on an unknown event or result
- [ ] Preserve unknown open-enum values if you re-emit them
- [ ] Read `hypothesis` for partials and `text` for finals — never inject a hypothesis
- [ ] Treat `awaiting_consent` as unresolved and wait for `injection_resolved`
- [ ] Treat a missing capability flag as *denied*
- [ ] Do not cache capabilities across transports

**Any server**
- [ ] Answer `unsupported_command` for a command you cannot parse — never drop it
- [ ] Answer `unsupported_version` when there is no version overlap
- [ ] Enforce capabilities per connection; answer `forbidden`, do not silently downgrade
- [ ] Reject oversized `payload_len` before allocating
- [ ] Populate `capabilities.routes` explicitly
- [ ] Report every stage timing, using `skipped`/`not_reported` rather than zero

### App context and profiles (S23)

`get_context` is an additive discovery command requiring `features.context_read`.
Trusted local connections receive this capability; remote connections do not.
The response is `{"type":"context","context":null,"tone":"neutral"}` when
focus is unavailable/disabled, or includes an `AppContext` and the resolved
profile overrides, for example:

```json
{"type":"context","context":{"app":"com.mitchellh.ghostty","title":"Synthetic Claude fixture","category":"terminal","profile":"claude-code"},"tone":"neutral","inject":"paste","spoken_punctuation":false,"spoken_line_breaks":false}
```

Overrides are optional: omitted means inherit category/global configuration.
`inject` accepts `paste`, `type`, or `off`. All category tones are neutral;
terminal defaults disable spoken punctuation/line breaks while inheriting the
configured paste policy. They do not disable LLM formatting: the pass applies
its verbatim terminal policy (fillers, false starts, self-corrections and
punctuation only). A profile may override any of these, including
`llm_format: false` to skip the pass.
The thin `dictate context [--json]` CLI sends only this protocol command.

Sessions publish an additive `context_resolved` event before `transcribing`:

```json
{"type":"context_resolved","session_id":"s1","context":{"app":"slack","title":"Synthetic chat fixture","category":"chat"}}
```

`context` is explicitly `null` when absent. Focus/profile resolution happens
at local session acceptance before opening audio, and again when a live
session stops (explicit Stop, hold release, or VAD auto-stop). The user's
stop-time app selects formatting and injection policy. If that app differs,
a second `ContextResolved` is published before transcription. An unchanged
app retains its initial profile. Injection still goes to current focus at
delivery; the daemon never re-focuses or refuses a changed window. Caller-supplied `SessionOptions.app` resolves without a host
window title; uploads and remote clients must never query host focus.
`ContextResolved` events are withheld entirely from connections lacking
`context_read`, including subscribers to all events. Context titles are
sensitive. History stores only the stable app ID; privacy mode persists no
context. Wayland compositor adapters are deferred; absent context follows the
same default path as headless operation.

### Personal dictionary (S22)

`list_dictionary`, `upsert_dictionary_entry`, and `delete_dictionary_entry` are
implemented. `DictionaryEntry.apps` is an optional JSON array of application
identifiers, matched case-insensitively against `AppContext.app`; omitted/empty
means global. No context uses only global entries. Entries retain `id`, `phrase`,
`sounds_like`, `case_sensitive`, `enabled`, `source`, and server-owned `hit_count`.
Creates omit `id`; updates require an existing positive id. A duplicate canonical
phrase (Unicode default case-fold comparison) returns `conflict`, invalid strings return
`invalid_params`, and absent ids return `not_found`. Hit counts supplied by clients
are ignored. Lists include disabled entries; `query` filters canonical phrases and
`limit` bounds the result (up to 10,000 entries); omitted means all entries,
so JSONL export cannot silently truncate the dictionary.

`list_dictionary_suggestions` returns `dictionary_suggestions` with a
`suggestions` array. Each item contains a proposed `entry` (no id), `reason`
(`consistent_rewrite` or `recurring_term`), `count`, `days`, `first_seen`, and
`last_seen` (UTC RFC3339). `limit` defaults to 500 and is capped at 500. Mining
only reads history; requesting suggestions never inserts an entry. Explicit
acceptance is an `upsert_dictionary_entry` using the proposed entry, and requires
`dictionary_write`. Mining also requires `history_read`, since proposed terms
are derived from private history. Missing capabilities return `forbidden` before
store access.

Dictionary storage defaults to `$XDG_DATA_HOME/dictated/dictionary.db`, supports
`[dictionary] db_path`, and uses WAL with ordered `PRAGMA user_version`
migrations. Default matching rewrites only complete explicit aliases and re-cases
canonical phrases. Optional fuzzy single-token matching is disabled by default.
Recognizer bias merges a ranked natural glossary with `whisper.initial_prompt`,
hard capped at 400 characters/UTF-8 bytes. `SessionOptions.use_dictionary=false`
disables dictionary bias, replacement, and vocabulary access for that session.
Privacy sessions never increment dictionary hit counts.

The CLI supports `dictate dict list|add|rm|enable|disable|suggest|accept|import|export`.
Example: `dictate dict add "Kubernetes" --sounds-like "cube ernetties,kubernetties"
--app slack`. `rm`, `enable`, and `disable` take a canonical phrase or id;
`accept` takes a suggestion's canonical phrase. `import`/`export` accept a JSONL
file path or `-` (default stdin/stdout). Import remaps ids by phrase and leaves hit
counts server-owned. Import validates JSON before sending requests; record-level
validation/database failures are reported and earlier successful records remain
installed. `--case-sensitive` on `add` preserves alias case.
