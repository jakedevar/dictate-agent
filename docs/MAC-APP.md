# The Mac app (DictateHub), local dictated and hub fallback

DictateHub is a menu-bar dictation app for a Mac. You hold a hotkey and speak.
By default it sends the clip to **dictated on the Mac** through a private Unix
socket. If the Mac daemon is unavailable, Auto can send it through the SSH
tunnel to this Linux hub. Both run the same Rust text chain; the Mac pastes
the text at your cursor. No cloud service is involved.

```
Mac (Auto tries local first)                     Hub (fallback)
DictateHub.app --> dictated/run/api.sock
DictateHub.app -- HTTP/1.1 over private UDS        dictated
  run/hub.sock ======== reverse SSH -R =======>  127.0.0.1:7313 API
  (0700 directory, no TCP listener)               bearer token required
```

The Mac app's build, install and permission notes live under `mac/`.
The hub setup below is optional fallback provisioning. The hub's
`dictated.service` is currently stopped intentionally; leave it stopped
unless the operator explicitly authorizes enabling the fallback.

## Mac-local endpoint and a second client

The local daemon serves only `~/Library/Application Support/dictated/run/api.sock`
(0700 parent, 0600 socket). Its separate token is
`~/Library/Application Support/dictated/api-token` (0600), read directly by
DictateHub on each request. Models live under
`~/Library/Application Support/dictated/models/`. DictateHub records and pastes;
dictated accepts uploaded audio without opening a microphone or injecting text.
Each API client can transcribe only its own audio and has no history, config,
context or dictionary access. Token rotation is owned by dictated.

Another local app or shell can send a WAV to the same API:

```bash
mac/scripts/local-transcribe.sh clip.wav

# Equivalent curl request: shell builtin printf passes the token on stdin.
printf 'header = "Authorization: Bearer %s"\n' \
  "$(cat "$HOME/Library/Application Support/dictated/api-token")" |
  curl --disable --silent --show-error --fail-with-body --max-time 120 \
    --unix-socket "$HOME/Library/Application Support/dictated/run/api.sock" \
    -K - --header 'Content-Type: audio/wav' --data-binary @clip.wav \
    http://localhost/v1/transcribe
```

The wrapper prints the response JSON, propagates curl errors, validates token
characters before building curl config, and never retries. It reads the token
without copying it to another file or putting it in argv. `DICTATE_LOCAL_DIRECTORY`
overrides the local daemon directory for isolated tests. `remote.sh test` runs
the shell argv/stdin checks and the wrapper against a Swift UDS stub, without
the local daemon or hub. Do not use `-H "Authorization: Bearer …"` with the token.

## Endpoint selection

Settings offers Auto / Mac only / Hub only / Custom URL. Auto retries the hub
only for a missing socket, refused connection, a timeout before any request
bytes are submitted, or an explicit HTTP 503 rejection (not ready). A local
401 is an error. Once sending starts, a failure or timeout cannot cause fallback:
the server may already have accepted the audio. A missing local token alongside
an existing socket is an error; an absent installation can fall back. Custom
URLs and the hub each use the saved hub token, never the Mac token.

Saved `tunnel` and the old loopback default migrate to Auto once. Explicit Hub
only selections persist. Settings and the status item tooltip show
`Using: Mac (local)` or `Using: Hub (fallback)` from the last dictation. Logs
record the endpoint without dictation text. Test connection probes both Mac
and hub independently, including when one fails authentication; Custom probes
only the selected URL. Probe results do not replace the last dictation endpoint.

## Security model

- **Hub loopback, Mac private socket.** `dictated` listens on `127.0.0.1:7313`
  only. SSH creates `~/Library/Application Support/DictateHub/run/hub.sock`
  on the Mac in an owned 0700 directory. No Mac TCP listener is requested,
  so `GatewayPorts` cannot widen exposure. Other local users cannot traverse
  the socket directory; same-user processes still need the bearer token.
- **The hub dials out.** The tunnel is an ordinary outbound SSH connection from
  the hub to the Mac, using the hub's existing key for the Mac. The hub's
  firewall is not changed and the hub opens no new listener.
- **Encrypted.** Requests and transcripts cross the network only inside the SSH
  session.
- **Bearer token on every request**, loopback included: a 256-bit `dct1_…`
  token in `$XDG_DATA_HOME/dictated/api-token` (0600). A process on the Mac
  without the token gets `401`. The Mac app keeps its copy in
  `~/Library/Application Support/DictateHub/api-token` (0600, directory 0700).
- **The narrowest grant.** A network peer may only transcribe audio it sends.
  It can never type on the hub, open the hub's microphone, or read history,
  config, the dictionary or window context. The route is forced to `type` and
  nothing is injected on the hub. See
  `thoughts/shared/plans/2026-10-07-s33-network-api-security-design.md`.
- **Stream-local forwarding must be allowed.** The running Mac sshd must allow
  remote Unix-socket forwards (`AllowStreamLocalForwarding yes` or `remote`).
  `ExitOnForwardFailure` fails the actual request if it is forbidden. No
  on-disk sshd policy is treated as proof of the running daemon's policy.
- **Rotation.** `dictated --rotate-api-token >/dev/null && just mac-token`
  issues a new token, copies it to the Mac, and makes a running `dictated`
  refuse the old one from its next request on.

## Hub setup (once)

1. **Check SSH works without a prompt.** The tunnel runs under systemd with
   `BatchMode=yes`. It cannot type a passphrase or accept a new host key:

   ```bash
   systemd-run --user --wait --pipe --collect ssh -o BatchMode=yes mac-mini true && echo ok
   ```

   If this fails, give the hub a passphrase-less key for the Mac (or run an
   agent that systemd's user manager can see), add the Mac's host key to
   `~/.ssh/known_hosts` with one interactive `ssh mac-mini`, and use an
   `~/.ssh/config` `Host mac-mini` entry for the user and key.

2. **Enable the API on loopback.** Merge `config/mac-hub-api.toml` (installed
   as `~/.local/share/dictate-agent/mac-hub-api.toml`) into
   `~/.config/dictate-agent/config.toml`, replacing any `[api]` block. Then:

   ```bash
   dictated --check-config          # must exit 0
   dictated --api-token >/dev/null  # create the token if absent (never paste it anywhere)
   systemctl --user restart dictated
   dictate doctor                   # "Network API: listening on 127.0.0.1:7313 (loopback only)"
   ```

3. **Install the tunnel:** `just install-mac-tunnel` (or
   `make install-mac-tunnel`). This installs `~/.local/bin/dictate-mac-tunnel`
   and `~/.config/systemd/user/dictate-mac-tunnel.service`, then enables and
   starts it. `dictate doctor` then shows `Mac app tunnel: … is running`, and
   `journalctl --user -u dictate-mac-tunnel` shows `starting private Unix-socket tunnel`. To check directory preparation without
   forwarding: `dictate-mac-tunnel --check`. Confirm connectivity with the
   authenticated check below or the app's **Test connection**.

4. **Copy the token to the Mac:** `just mac-token` (`--host other-mac` for
   another host). It prints where it wrote the token and that the Mac's copy
   matches. It never prints the token. On the Mac it writes an exclusive
   `mktemp` file inside the 0700 directory and renames it over `api-token`, so
   a pre-planted symlink is never followed: a symlinked directory is refused,
   a symlink at `api-token` is replaced, not written through.

5. **Check from the Mac** (prints the daemon's status JSON, then `401`):

   ```bash
   ssh mac-mini 'printf "header = \"Authorization: Bearer %s\"\n" "$(cat ~/Library/Application\ Support/DictateHub/api-token)" | curl -s --unix-socket "$HOME/Library/Application Support/DictateHub/run/hub.sock" -K - http://127.0.0.1:7313/v1/status'
   ssh mac-mini 'curl -s --unix-socket "$HOME/Library/Application Support/DictateHub/run/hub.sock" -o /dev/null -w "%{http_code}\n" http://127.0.0.1:7313/v1/status'
   ```

   The token goes to curl as a config file on its stdin (`-K -`), written by
   the shell's built-in `printf`. Never put it in a `-H "...Bearer $(cat …)"`
   argument: that lands in curl's command line, which any process on the Mac
   can read with `ps`. `scripts/test-mac-hub.sh` runs these two lines with a
   fake curl and fails if the token reaches an argv.

## Private socket lifecycle

The wrapper prepares the owned 0700 socket directory over a forward-free SSH
session with a local 20-second deadline (`DICTATE_MAC_PREPARE_SECS`). It
refuses symlinked directories and non-socket destination entries, enforces
Darwin's 103-byte path limit, and removes the stale socket before each start.
This supports the Mac's `StreamLocalBindUnlink no` without changing sshd.
The default sshd `StreamLocalBindMask 0177` produces a 0600 socket; directory
permissions provide isolation even if a server uses a looser socket mask.
A stopped forward may leave a socket inode; it cannot carry traffic and the
next start removes it.

The wrapper then supervises
`ssh -N -T -R <mac-socket>:127.0.0.1:7313 mac-mini`, with BatchMode,
StrictHostKeyChecking, ExitOnForwardFailure, keepalives, no agent/X11
forwarding and no shared control connection. SSH host aliases containing
additional forwards are refused. There is no GatewayPorts preflight, no
Mac TCP forward and no post-start checker that could stall while exposed.
Configuration refusals exit 78 (not retried). SSH/link failures exit 1 and
are retried. A stop signal terminates/reaps SSH and exits 0.

The app defaults to **Auto**. Saved `tunnel` and `http://127.0.0.1:7313`
settings migrate to this default; custom HTTP(S) URLs remain available in
Settings. File mode and hotkey dictation share `DictationClient` and `HubClient`.
HTTP over UDS sends `Host: 127.0.0.1:7313` with a per-request deadline.
For isolated tests, `--endpoint unix:/absolute/path/hub.sock` selects another
socket without changing saved settings.

Tests: `scripts/test-mac-hub.sh`, `just mac-tunnel-sshd-test` (real isolated
sshd with `GatewayPorts yes`, no TCP listener), `just mac-test` (Swift UDS
stub and URLSession tests), and `just mac-tunnel-smoke` (isolated hub + Mac).

## Another Mac, socket or hub port

Use a drop-in (`systemctl --user edit dictate-mac-tunnel`):

```ini
[Service]
Environment=DICTATE_MAC_HOST=my-other-mac
Environment=DICTATE_HUB_API_PORT=7313
# Environment="DICTATE_MAC_SOCKET=/absolute/private/run/hub.sock"
```

A custom socket's parent must already be owned by the Mac user and must not
be a symlink; the wrapper tightens it to 0700. For a custom socket select
`unix:/absolute/private/run/hub.sock` as the app endpoint. If the hub API uses
another port, add `"127.0.0.1:7313"` to `[api] allowed_hosts`, because the UDS
client always sends that Host spelling. Remove old `DICTATE_MAC_PORT` and
sshd/lsof override variables from existing drop-ins; they no longer apply.
Upgrade the app with `mac/scripts/remote.sh install` alongside the tunnel.
The app is installed at `/Applications/DictateHub.app` (no sudo; an older
`~/Applications` copy is migrated away and its login item moved), so Launchpad
and Spotlight find it. Opening it again while it runs shows the Permissions
window (or Settings once everything is granted). Old build bundles under
`~/dictate-mac-build/` are unregistered and deleted by `remote.sh clean-stale`
(run at the end of every `install`). Details: `mac/README.md`.

## When something is down

`dictate doctor` has two checks for this:

| Check | Means |
|---|---|
| `network_api` ok | listening on loopback |
| `network_api` FAIL "enabled but DOWN: …" | it did not start (port taken, no token, bad TLS file). Dictation on the hub is **unaffected**: the API is optional and its failure never stops `dictated`. Fix the cause and restart `dictated`. |
| `mac_tunnel` ok | the tunnel unit is running |
| `mac_tunnel` warn "refused to start" | the socket path or SSH alias configuration was refused. `dictate-mac-tunnel --check` says why |
| `mac_tunnel` warn | the unit is restarting or failed: `systemctl --user status dictate-mac-tunnel`, `journalctl --user -u dictate-mac-tunnel` |

Tunnel behaviour:

- The tunnel retries every 10 s, forever, while the Mac sleeps, is offline or
  reboots. `ServerAliveInterval=15`/`CountMax=3` notices a dead link in about
  45 s.
- `ExitOnForwardFailure=yes`: if the running sshd refuses stream-local
  forwarding or cannot bind the socket, SSH exits and systemd retries.
- An invalid `[api]` (for example a LAN bind without `allow_lan`) is a config
  error and stops `dictated` at startup like any other config error. Run
  `dictated --check-config` before restarting.

## Removing it

```bash
just uninstall-mac-tunnel        # disable, stop and remove the unit and its wrapper
dictated --rotate-api-token >/dev/null   # invalidate the Mac's copy
```

Then set `[api] enabled = false` and restart `dictated` to close the port.
