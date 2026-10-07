---
date: 2026-10-07
author: Claude Opus 5.5 (S33 worker ee0f4c1b, Issue #1470)
type: security_design
status: proposed — needs ONE pre-merge security review (other model family) before merge
slice: S33 network API server (`dictate-server`)
base_commit: 783b849
parent_plan: thoughts/shared/plans/2026-08-07-wisprflow-parity-master-slice-map.md
contract: thoughts/shared/plans/2026-09-29-wave2-integration-contract.md
---

# S33 network API: security design

`dictate-server` puts `dictate-proto` on TCP (HTTP + WebSocket, axum) so a
future thin client such as a phone, or a second machine for sync, can send
audio and get text back. The daemon's existing control plane is a
unix socket. The socket is mode 0600, it sits in a 0700 runtime directory, and it
is checked by `SO_PEERCRED` (#1479). That is a strong local boundary, and a TCP
listener weakens every part of it. Any local UID can reach loopback TCP.
Browsers can reach it through DNS rebinding and CSRF. On a LAN bind, every
device on the network can reach it. This document sets the rules that keep the
network surface strictly smaller than the unix socket.

The design is ground truth for the S33 implementation on this branch. Where the
implementation differs from the slice spec, §11 records the deviation.

## 1. Assets

| Asset | Why it matters |
|---|---|
| Host desktop control: `text_injection`, `host_capture` (mic), `context_read` (window titles) | Typing into Jake's editor or switching his mic on from the network is the worst outcome. |
| Configuration: `set_config` rewrites a file the daemon trusts | A config write can change the Ollama host, the model, or the history path. That is persistent code and data redirection. |
| Transcripts: `final.text`, `raw_text`, history, dictionary | Personal speech. The repo is public, so fixtures stay synthetic. |
| Bearer token | One secret grants the network capability set. |
| Engine availability | There is one session slot. A remote upload makes a local dictation answer `busy`. |
| Host routes: `timer` (`systemd-run`), `local` (LLM) | These execute on the host. |

## 2. Threat model

### T1: LAN attacker

A device on the same Wi-Fi or LAN, or on the same tailnet when exposed that way.
It can send arbitrary TCP to any bound non-loopback address. On a plaintext
link it can sniff or spoof traffic: open Wi-Fi, a compromised AP, ARP spoofing.

- **Wins if** it gets any capability without the token, gets the token, gets
  past the network capability ceiling, or reads someone else's transcripts.
- **Defences:** no LAN bind unless explicitly opted in (§4). A 256-bit token is
  required on every request (§5). The LAN bind is refused without TLS or an
  explicit encrypted-tunnel acknowledgment (§6). The capability ceiling (§7) and
  session-scoped events (§8) apply to every peer. Rate limits and size limits
  apply too (§9).
- **Residual:** a token holder on the LAN can occupy the engine slot (DoS,
  bounded by §9). Under `allow_plaintext_lan` the defence is the tunnel's
  encryption, not ours.

### T2: Malicious local process

Two cases, because the unix socket already separates them.

- **Same UID.** Out of scope. Such a process can already use the unix socket,
  which grants `local_trusted`, strictly more than the network grant. It can
  also read the token file, but the network API gives it nothing new. The
  property that matters is that **the network grant is a strict subset of the
  same-UID unix grant**, so the API never widens what a same-UID process could
  do. A unit test enforces this.
- **Other UID** on a multi-user host. Loopback TCP has no peer-UID check, so
  without a token this attacker would have the API. Defences: the token is
  required on loopback too, and the token file is 0600 under the user's 0700
  data directory. The daemon refuses a token file that is group- or
  world-readable or owned by someone else (the `ssh` rule). Residual: **port
  squatting**. If `dictated` is down, another UID can bind the port and collect
  tokens from local HTTP clients. Local clients should use the unix socket. A
  LAN client should use TLS with a pinned certificate (§6). Jake's machine is
  single-user, so this is recorded rather than engineered away.

### T3: Browser CSRF and DNS rebinding against localhost

A web page Jake visits makes his browser send requests to
`http://127.0.0.1:<port>`. With DNS rebinding, the page re-resolves its own name
to 127.0.0.1, so the browser treats it as same-origin.

- **Wins if** it gets a state change, or any response it can read.
- **Defences, layered:**
  1. **No ambient authority.** There are no cookies and no session state. The
     only credential is an `Authorization: Bearer` header. A cross-origin page
     cannot add that header without a CORS preflight, and the server never
     answers a preflight. The server sends no `Access-Control-*` headers at all.
  2. **Origin rejection.** Any request with an `Origin` header is refused (403)
     before authentication, unless the value is listed in
     `[api] allowed_origins` (empty by default). Browsers send `Origin` on every
     WebSocket handshake and on cross-origin and non-GET requests. Native
     clients don't send it. This also closes cross-site WebSocket hijacking.
  3. **Host allow-list.** The `Host` header must be one of the bound
     address's literal forms (plus `localhost:<port>` on loopback) or an entry
     in `[api] allowed_hosts`. A rebinding attack arrives with
     `Host: evil.example:<port>` and gets 421.
  4. **The token.** Even if 1–3 failed, the page does not have the token.
- **Residual:** none known. Each layer alone defeats the classic attacks.

### T4: Out of scope

- Compromise of the user account itself (see T2, same UID).
- Off-LAN exposure. Use Tailscale or WireGuard, or `tailscale serve` in front
  of a loopback bind (Jake's decision, 2026-08-07, master plan decision 3).
  Never port-forward this to the internet.
- Client apps. The phone client is out of scope (server only).

## 3. Architecture: one code path

```
TCP accept (conn cap) ─► [TLS handshake, timeout] ─► hyper http1 (header timeout)
  ─► Host check ─► Origin check ─► per-IP throttle ─► bearer auth ─► route
       │
       ├─ POST /v1/transcribe  body ─► Command::TranscribeAudio{Inline}
       ├─ GET  /v1/status              ─► handshake + get_status
       └─ GET  /v1/ws  (upgrade)       ─► NDJSON-equivalent envelopes, text frames
                                            │
      dictate-server ──── Backend/Session traits (dictate-proto types only) ────
                                            │
      dictated::server::NetworkConnection  ─►  the SAME `process()`/`dispatch()`
         (Conn with network=true, owner=false,      the unix socket uses
          base grant = network grant)
```

- `dictate-server` depends on `dictate-proto` only, plus axum, hyper, rustls
  and similar. It owns the transport, authentication, limits and `ApiConfig`.
  It knows nothing about the engine.
- `dictated` implements the `Backend` trait with `NetworkConnection`. That is a
  thin wrapper over the existing per-connection `Conn` and the existing
  `process`/`dispatch` functions in `server.rs`. Parsing, handshake, the
  `is_implemented` check, `Command::is_permitted`, `resolve_upload_options`,
  `decode_upload`, `engine.transcribe`, session ownership and the cancel-on-hangup
  rule are all the unix-socket code. Nothing is reimplemented.
- `POST /v1/transcribe` rewrites the HTTP body into
  `AudioSource::Inline { format, data }`. `decode_upload` then applies the same
  limits it applies on the socket. `AudioSource::Body` is a transport-level
  convenience that never reaches the engine.
- Connection ids come from the daemon's one `ClientIdGen`, so a network peer
  is never confused with a socket peer.

## 4. Bind policy

`[api]` section, owned by `dictate-server` and aggregated in
`dictate-core::config::Config`:

| Key | Default | Rule |
|---|---|---|
| `enabled` | `false` | Without it there is no listener. Default config is no TCP at all, per the contract's "never add a network dependency by default". |
| `bind` | `127.0.0.1:7313` | Must be an `IP:port` literal. Hostnames are refused, because DNS could resolve one to a non-loopback address. |
| `allow_lan` | `false` | Required for any non-loopback `bind`. |
| `tls_cert` / `tls_key` | empty | Set both or neither. PEM. Enables HTTPS. |
| `allow_plaintext_lan` | `false` | Acknowledges that `bind` is on an encrypted tunnel interface (WireGuard/Tailscale). The only way to run a LAN bind without TLS. |
| `token_file` | `$XDG_DATA_HOME/dictated/api-token` | See §5. |
| `allowed_hosts` / `allowed_origins` | `[]` | §2 T3. |
| `expose_raw_text` | `false` | §7. |
| `max_upload_bytes` | 32 MiB | §9. |
| `max_audio_seconds` | 300 | §9. |
| `max_connections` / `max_ws_sessions` | 16 / 4 | §9. |
| `requests_per_minute` / `burst` | 120 / 20 | §9. |

Startup decision. It is a pure function, `ApiConfig::bind_plan()`, and it is
unit-tested exhaustively.

1. `enabled = false` → no listener. Every other key is ignored, but still
   validated so `--check-config` can report mistakes.
2. `bind` does not parse as `IP:port` → **refuse**.
3. Unspecified address (`0.0.0.0`, `::`) → **refuse**. Bind one interface. An
   all-interfaces bind on a laptop includes whatever café network it joins
   later.
4. Non-loopback, after canonicalizing IPv4-mapped IPv6, and `allow_lan = false`
   → **refuse**.
5. Non-loopback, with no TLS and `allow_plaintext_lan = false` → **refuse**.
6. Only one of `tls_cert` / `tls_key` set → **refuse**.
7. At listener start, the token file is missing, unreadable, malformed,
   group/other-accessible or owned by another UID → **refuse**.
   The daemon never mints a token on startup. Creating one is an explicit
   operator step (§5), so "the token is missing" has a precise meaning.

A refusal is a **fatal daemon startup error**: `dictated` exits non-zero with
the reason. It does not silently run without the API. Rules 2–6 are also config
validation errors, so `dictated --check-config` reports them before a restart.
Rule 7 is checked when the listener starts.

## 5. Authentication

- **Token.** `dct1_` followed by 43 base64url characters: 32 bytes (256 bits)
  from the OS CSPRNG. The prefix lets secret scanners and log scrubbers
  recognise the token, and versions the format.
- **Issuance (pairing).** `dictated --api-token` prints the token. If none
  exists it creates one first. It also prints the URL the API would serve and,
  when TLS is configured, the leaf certificate's SHA-256 fingerprint for the
  client to pin. The operator carries these to the phone out of band (copy
  and paste, or QR from another tool). Nothing is exchanged over the network.
- **Storage.** The token file is written by create-new into a temp file (0600),
  fsynced, then renamed into place. The directory is created 0700. The file
  must be a regular file (`O_NOFOLLOW`), owned by the daemon's euid, with no
  group or other bits. The token is never stored in `config.toml`, which
  `get_config` serves and which tends to get backed up or shared.
- **Rotation.** `dictated --rotate-api-token` replaces the file atomically.
  The running daemon picks the new token up on the next request, because the
  store re-reads the file when its (dev, inode, mtime, len) changes. The old
  token stops working immediately. **Revocation** is deleting the file: every
  request then fails closed (401).
- **Verification.** `Authorization: Bearer <token>` only. There are no query
  parameters (they end up in logs and proxy histories) and no cookies. The
  presented token and the stored token are both hashed with SHA-256, and the
  digests are compared in constant time, so the comparison leaks neither length
  nor prefix. One token is valid at a time.
- **Failure handling.** 401 with `WWW-Authenticate: Bearer` and a protocol
  `unauthorized` error body. Each failure counts toward the per-IP failure
  throttle (§9). The log line has the peer IP and the reason ("missing",
  "malformed" or "mismatch"). It never contains the presented value.
- **v1 limitation, recorded:** one token for all devices. Revoking a lost phone
  revokes every client. Per-device tokens with names and scopes (for the sync
  surface, §7) are the follow-up. They are additive, because the token file
  format can grow a line per device.

## 6. TLS versus plaintext

| Bind | Transport | Why |
|---|---|---|
| Loopback | Plain HTTP (TLS optional) | The bytes never leave the host. TLS would not stop a same-UID process, and another UID cannot sniff loopback. |
| LAN or tailnet IP, with `tls_cert`/`tls_key` | HTTPS (rustls, ring provider, TLS 1.2+, ALPN `http/1.1`) | Protects the token and the audio on Wi-Fi. The client pins the printed fingerprint, which also defeats port squatting and a spoofed server. |
| Tailnet or WireGuard IP, with `allow_plaintext_lan` | Plain HTTP over the tunnel | WireGuard already authenticates and encrypts peer to peer. This is the recommended phone setup. |
| Any other LAN, plaintext | **Refused** | A bearer token in cleartext on Wi-Fi is a token for anyone on the network. |

Recommended setup: keep `bind` on loopback and put `tailscale serve` in front
of it. That gives HTTPS with a real certificate and tailnet ACLs, and needs no
LAN bind at all. Add the `*.ts.net` name to `allowed_hosts`. The daemon does
**not** trust `X-Forwarded-For` or any other proxy header, so behind a proxy
every peer shares the per-IP throttle. That fails conservative.

Certificates are supplied by the operator, for example from `tailscale cert` or
a self-signed `openssl req`. The daemon does not generate certificates, which
keeps key management out of a voice daemon. The TLS handshake has a 10 s timeout
and runs in the per-connection task, so a slow handshake cannot block `accept`.

## 7. Capabilities for network peers

### 7.1 Grant derivation

The unix socket derives authority from the transport and `SO_PEERCRED`
(`restrict_to_owner`, #1479). A network peer derives authority from the
transport and the token. It **never** comes from what the client calls itself:
`ClientInfo.kind` stays a logging hint.

```
grant = Capabilities::remote_transcription_only()          // proto reference set
        with raw_text        = [api] expose_raw_text        // opt-in, default false
             streaming_audio = false                        // not implemented (§11)
             audio_level_events = false                     // uploads have no live level
             limits          = from [api] (§9)
             headless/privacy_mode = mirrored from the daemon (informational)
grant = network_ceiling(grant)   // hard clamp, dictate-server
grant = restrict_to_owner(grant, owner = false)            // server.rs, same as a foreign UID
```

`network_ceiling` clears `text_injection`, `host_capture`, `context_read`,
`config_read`, `config_write`, `diagnostics`, `history_read`, `history_write`,
`dictionary_*`, `snippets_*` and `wake_word`, and sets `routes` to `["type"]`.
It runs whatever the config says, so a future config key cannot widen the grant
by accident. A network connection is constructed with `owner = false`, so the
#1479 rule withdraws `config_read` and `config_write` a second time, in the
shared handshake. **`config_write` is unreachable from the network by default,
and by construction.** There is no key that grants it.

### 7.2 Per-command exposure (v1)

| Command | Network peer | Enforced by |
|---|---|---|
| `handshake`, `get_status`, `subscribe`, `unsubscribe` | ✅ (status scrubbed, events scoped, §8) | `is_permitted` (always) |
| `transcribe_audio` (WS inline; HTTP body) | ✅ `inject` must be absent or `false` | `transcribe_upload`; `resolve_options` answers `forbidden` to `inject:true` |
| `stop`, `cancel` | ✅ **own session only** | `is_permitted` plus engine ownership (`host_capture = false`, so own only) |
| `start_dictation`, `toggle` | ❌ forbidden | `host_capture` |
| `get_context` | ❌ forbidden | `context_read` |
| `get_config`, `set_config` | ❌ forbidden | `config_*`, cleared twice |
| `list_dictionary*`, `upsert/delete_dictionary_entry` | ❌ forbidden | `dictionary_*` |
| `*_snippet*` | ❌ unsupported (this build) | `is_implemented` |
| `query_history`, `get_history_analytics`, `purge_history` | ❌ forbidden | `history_*` |
| `diagnose` | ❌ forbidden (names host paths) | `diagnostics` |
| `begin_audio_stream`, `end_audio_stream` | ❌ unsupported (this build, §11) | `is_implemented` |
| route `timer` / `local` / any but `type` | ❌ forbidden, including when the router picks it from the words spoken | `allows_route` plus the pipeline's `allowed_routes` check |

The sync surface (feature #18: dictionary and snippet sync) is **not** exposed
in v1. It needs a scoped token (`sync` scope: dictionary and snippets read and
write, nothing else), which is the per-device-token follow-up. It is not a
widening of this grant.

### 7.3 `raw_text` gating

S01 deferred item 4 is resolved here. There is a new additive capability flag,
`Features::raw_text`. It defaults to `false`, `local_trusted()` sets it to `true`,
and `remote_transcription_only()` sets it to `false`.

- In the shared dispatch, a `Transcript` result has `raw_text` removed when the
  connection lacks the flag. The same applies to the `Transcript` inside a
  forwarded `final` event. It is one function, used by both transports.
- Local socket behaviour does not change, because local connections hold the
  flag.
- A network peer gets `raw_text` only when `[api] expose_raw_text = true`, which
  is for evaluation tooling on a trusted client.
- The protocol change is additive. Golden tests pin the new flag in the
  handshake. An older client ignores the field; an older daemon omits it, which
  reads as `false`. That is fail-safe: the client just doesn't show raw text it
  received anyway.

## 8. Events and status are scoped

Today `Conn::wants` forwards **every** bus event that matches a subscriber's
filter. A network subscriber would therefore receive the `final` text of Jake's
local dictations. For network connections:

- An event is forwarded only if its `session_id` belongs to a session this
  connection started. The session ids are recorded from `engine.transcribe`
  tickets, and the last 16 are kept.
- Events with no session id (connection-level `error`) are never forwarded from
  the bus. A network peer's own connection errors are written directly, as
  before.
- `context_resolved` still requires `context_read`, so it never reaches a
  network peer.
- `final` passes through the same `raw_text` redaction.
- `get_status` for a network connection drops `daemon.pid`, `audio` (device
  state), `formatter` (the Ollama host and model) and `session` (unless the
  session is the connection's own). It keeps `state`, which is accepted residual
  risk: a token holder learns whether the daemon is busy, which it needs in
  order to avoid getting `busy` errors.

## 9. Limits

| Limit | Default | Where |
|---|---|---|
| Concurrent TCP connections | 16 | accept loop (semaphore; excess closed immediately) |
| Concurrent WS sessions | 4 | before upgrade → 503 `busy` |
| TLS handshake | 10 s | per-connection task |
| HTTP header read | 10 s | hyper `header_read_timeout` (slowloris) |
| Upload body read | 60 s total | handler timeout → 408 `timeout` |
| Upload body size | `max_upload_bytes` = 32 MiB | streamed with a hard cap → 413 before buffering past the cap |
| Decoded audio duration | `max_audio_seconds` = 300 | `decode_upload` (`limits.max_audio_ms`) → 413 |
| WS message size | the encoded form of `max_upload_bytes` (base64 + 4 KiB) | `WebSocketUpgrade::max_message_size` |
| WS pipelined requests | 4 queued; more closes the connection (1008) | per-connection reader |
| WS idle | 300 s without a client message → close | session loop |
| Requests per peer IP | token bucket, 120/min, burst 20 (WS messages count) | pre-auth middleware → 429 `rate_limited` |
| Auth failures per peer IP | 5 per 60 s, then blocked 60 s | auth middleware → 429 |
| Throttle table | 1024 IPs; idle entries pruned | bounded memory |
| Engine | one session slot (existing) | a busy engine answers 409 `busy` |

Brute force is not what the auth throttle defends against: 256 bits cannot be
brute-forced. It keeps a misconfigured client from turning into log spam.

## 10. Logging privacy

- Never logged: the token, the `Authorization` header, any request body or
  audio, query strings, transcripts, `raw_text`, or the `app` hint. Access
  logs record the method, the path without its query, the status, the peer IP,
  the duration and byte counts.
- Auth failures log at `warn` with the peer IP and a reason class. Bind
  decisions log at `info`, and a LAN bind logs at `warn` (it is a posture
  change).
- The transcript text in logs is governed by the pipeline's existing rule:
  nothing in privacy sessions or privacy mode. Network sessions follow it
  unchanged. A client can always send `privacy: true`.
- Remote dictations are recorded in history like local uploads unless
  privacy is requested. They are Jake's dictations, from his phone.
- An integration test captures every log line from a network session and
  asserts that neither the token nor a privacy session's text appears.

## 11. Deviations from the slice spec

1. **No binary audio streaming over WS in v1.** The spec lists "control +
   events + binary audio frames". The documented flow answers
   `begin_audio_stream` with a live `session_id`, which needs an engine session
   fed by frames. That means changes to `engine.rs` and `pipeline.rs`, which
   #1478 owns during this slice. Binary frames are refused (close 1003).
   `begin_audio_stream` and `end_audio_stream` stay `unsupported_command`,
   exactly as on the socket. The thin-client path exists (`POST /v1/transcribe`,
   or `transcribe_audio` with inline audio over WS). Follow-up Issue: engine-fed
   streaming sessions.
2. **Token required on loopback too.** The spec says "bind 127.0.0.1 default,
   LAN opt-in" plus "bearer-token auth". Loopback TCP lacks the socket's UID
   check, so loopback is authenticated as well.
3. **The daemon never generates the token at startup.** The spec says
   "config-generated". It is generated by an explicit operator command, which
   makes "token missing" a startup refusal rather than a silent mint.
4. **Unspecified-address binds are refused.** This is a stricter reading of
   "LAN opt-in".
5. **No sync surface yet.** Dictionary and snippet sync needs scoped tokens
   (§7.2).

## 12. Verification plan (automated, CPU-only, 127.0.0.1 ephemeral ports)

- Bind policy: every refusal row in §4, plus the accept rows.
- Token: generate, format, entropy length, 0600, insecure-mode refusal,
  foreign-symlink refusal, rotation picked up live, deletion fails closed,
  constant-time verify, wrong token rejected.
- HTTP: missing, malformed and wrong token give 401. `Origin` gives 403.
  A foreign `Host` gives 421. An oversized body gives 413. The auth-failure
  throttle gives 429. A WAV upload returns the text with `injection: delivered`,
  without `raw_text` by default and with it under `expose_raw_text`.
  `inject=true` gives 403. `route=timer` gives 403.
- WS: lifecycle (connect, handshake, subscribe, transcribe, own `final`
  event, close). A WS upgrade with `Origin` is refused. Without a token it is
  refused. `set_config` and `get_config` are forbidden, and so are `toggle`,
  `diagnose` and history. Binary frames are refused. The **local session's
  `final` is never delivered to a network subscriber**. A network peer cannot
  `cancel` a local session. Disconnecting mid-upload cancels the peer's own
  session.
- Grant: the network grant is a strict subset of the local grant, and the
  ceiling holds whatever the config says.
- Logs: no token, and no private text.

## 13. Pre-merge security review: focus list

1. `ApiConfig::bind_plan` and the startup wiring. Is there any path that binds
   a non-loopback address without `allow_lan`, the token, and TLS or the tunnel
   acknowledgment?
2. Token store: file creation (temp, 0600, rename, O_NOFOLLOW), the ownership
   and mode checks, the reload trigger, the constant-time compare, and that no
   log or error carries the token.
3. Middleware order: Host, Origin, throttle and auth must all run before any
   handler or WS upgrade, for every route including 404s and the upgrade.
4. `NetworkConnection` in `server.rs`: the handshake uses the network grant,
   `owner = false`, the event scoping (session ownership set) and `raw_text`
   redaction, the status scrub, and drop leading to `disconnected` (cancelling
   the upload).
5. The capability ceiling and the strict-subset test. That `config_write` is
   unreachable from the network.
6. Resource bounds: body cap enforced while streaming, WS message cap, the
   pipelined-request queue, connection and session semaphores, timeouts,
   and the throttle table bound.
7. TLS: provider, protocol versions, handshake timeout, and that a plaintext
   LAN bind is impossible without `allow_plaintext_lan`.
8. Logging: access-log fields, auth-failure lines, and the privacy test.
