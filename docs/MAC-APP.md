# The Mac app (DictateHub) and its hub

DictateHub is a menu-bar dictation app for a Mac. You hold a hotkey and speak.
It sends the clip to **this hub**, the Linux machine running `dictated`, which
transcribes it with its local models (Silero VAD, Whisper on CUDA, the text
chain, the optional Ollama pass). The Mac pastes the text at your cursor. No
cloud service is involved.

```
Mac                                               Hub (this machine)
DictateHub.app                                    dictated
  POST http://127.0.0.1:7313/v1/transcribe          network API on 127.0.0.1:7313
  Authorization: Bearer <token>                       (loopback only, token required)
          |                                               ^
          +--- Mac loopback 127.0.0.1:7313 ===== ssh -R ==+
                (sshd listens here)          dictate-mac-tunnel.service (hub dials out;
                                             verifies loopback-only, else refuses)
```

This page covers the **hub side**. The Mac app's own build, install and
permission notes live with it under `mac/`.

## Security model

- **Loopback on both ends, enforced.** `dictated` listens on `127.0.0.1:7313`
  only. The tunnel asks the Mac's sshd for `-R 127.0.0.1:7313:...`, but that
  is only a request: the Mac's sshd decides where the port listens, and
  `GatewayPorts yes` there binds every interface even for an explicit
  `127.0.0.1` (the forward still "succeeds"). So the unit runs
  `dictate-mac-tunnel`, which makes loopback-only a checked condition, not an
  assumption (see [The loopback guard](#the-loopback-guard)). No other machine
  on either network can reach the port.
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
- **The Mac must keep `GatewayPorts no`** (macOS's default; no file sets it).
  `yes` or `clientspecified`, anywhere in its sshd config, including inside a
  `Match` block, makes the tunnel refuse to start.
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
   `journalctl --user -u dictate-mac-tunnel` shows `preflight ok` and
   `tunnel up: … (verified loopback only)`. To check the Mac's sshd policy
   first, without forwarding anything: `dictate-mac-tunnel --check` (after
   the install) or `scripts/dictate-mac-tunnel --check` (before).

4. **Copy the token to the Mac:** `just mac-token` (`--host other-mac` for
   another host). It prints where it wrote the token and that the Mac's copy
   matches. It never prints the token. On the Mac it writes an exclusive
   `mktemp` file inside the 0700 directory and renames it over `api-token`, so
   a pre-planted symlink is never followed: a symlinked directory is refused,
   a symlink at `api-token` is replaced, not written through.

5. **Check from the Mac** (prints the daemon's status JSON, then `401`):

   ```bash
   ssh mac-mini 'printf "header = \"Authorization: Bearer %s\"\n" "$(cat ~/Library/Application\ Support/DictateHub/api-token)" | curl -s -K - http://127.0.0.1:7313/v1/status'
   ssh mac-mini 'curl -s -o /dev/null -w "%{http_code}\n" http://127.0.0.1:7313/v1/status'
   ```

   The token goes to curl as a config file on its stdin (`-K -`), written by
   the shell's built-in `printf`. Never put it in a `-H "...Bearer $(cat …)"`
   argument: that lands in curl's command line, which any process on the Mac
   can read with `ps`. `scripts/test-mac-hub.sh` runs these two lines with a
   fake curl and fails if the token reaches an argv.

## The loopback guard

`dictate-mac-tunnel` (`scripts/dictate-mac-tunnel`) is the unit's ExecStart. On
every start it:

1. **Preflight, before any forward exists.** Over a forward-less ssh session
   (`ClearAllForwardings=yes`) it reads the Mac's sshd policy two ways:
   `sshd -G` (the effective global value; works without root on macOS) and
   the text of `/etc/ssh/sshd_config` plus every file it `Include`s,
   recursively (macOS: `sshd_config.d/*`, which includes `crypto.conf`). The
   text scan is what sees a `GatewayPorts` inside a `Match` block, which
   `sshd -G` cannot evaluate without root. It refuses if any `GatewayPorts`
   value anywhere is not `no`, if a file exists but cannot be read, if an
   `Include` cannot be followed, if `sshd -G` fails, or if the reply is
   truncated.
2. **Starts** `ssh -N -T -R 127.0.0.1:MAC_PORT:127.0.0.1:HUB_PORT` with
   BatchMode, StrictHostKeyChecking=yes, ExitOnForwardFailure, no agent or X11
   forwarding and no shared control connection.
3. **Verifies on the Mac** with `lsof -nP -iTCP:MAC_PORT -sTCP:LISTEN` that
   every listener is `127.0.0.1` or `[::1]`. Anything else (`*`, a LAN
   address) kills the tunnel at once. No lsof on the Mac is also a refusal;
   no listener within 20 s is a retry.

A refusal exits 78 and the unit's `RestartPreventExitStatus=78` keeps it down
(`failed`) rather than retrying every 10 s; `dictate doctor` says
`refused to start`. `dictate-mac-tunnel --check` prints the reason. Fix the Mac,
then `systemctl --user restart dictate-mac-tunnel`. Network trouble (the Mac
asleep, no route) is exit 1, which `Restart=` retries as before.

Limits, stated plainly: the preflight reads the configuration sshd *would*
use. If the Mac's sshd were started some other way (its own `-f` file, or a
changed config it has not reloaded), the post-start lsof check is the backstop,
and between the forward appearing and that check (about a second: the
check polls every 0.25 s) the port could be reachable on the Mac's network. Every request still
needs the bearer token.

Tests: `scripts/test-mac-hub.sh` (part of `just check-cpu`; stub ssh, every
refusal path) and `just mac-tunnel-sshd-test` (a real isolated sshd on
127.0.0.1, plus a private network namespace in which sshd really runs
`GatewayPorts yes` and the post-start check kills the wildcard forward).

## Another Mac, or other ports

Use a drop-in, not an edit to the unit (`systemctl --user edit dictate-mac-tunnel`):

```ini
[Service]
Environment=DICTATE_MAC_HOST=my-other-mac
# Environment=DICTATE_HUB_API_PORT=7313   the hub's [api] bind port
# Environment=DICTATE_MAC_PORT=7313       what the Mac app connects to
```

Keep the two ports equal. The API accepts `Host: 127.0.0.1:<its own port>`
(and `localhost:<port>`). A different Mac-side port means the Mac app sends a
`Host` the API refuses with `421`, unless you add that spelling, for example
`"127.0.0.1:17313"`, to `[api] allowed_hosts`.

## When something is down

`dictate doctor` has two checks for this:

| Check | Means |
|---|---|
| `network_api` ok | listening on loopback |
| `network_api` FAIL "enabled but DOWN: …" | it did not start (port taken, no token, bad TLS file). Dictation on the hub is **unaffected**: the API is optional and its failure never stops `dictated`. Fix the cause and restart `dictated`. |
| `mac_tunnel` ok | the tunnel unit is running |
| `mac_tunnel` warn "refused to start" | the loopback guard refused: the Mac's sshd could publish the port beyond its loopback, or could not be checked. `dictate-mac-tunnel --check` says why |
| `mac_tunnel` warn | the unit is restarting or failed: `systemctl --user status dictate-mac-tunnel`, `journalctl --user -u dictate-mac-tunnel` |

Tunnel behaviour:

- The tunnel retries every 10 s, forever, while the Mac sleeps, is offline or
  reboots. `ServerAliveInterval=15`/`CountMax=3` notices a dead link in about
  45 s.
- `ExitOnForwardFailure=yes`: if the Mac's port is still held, for example by
  the sshd of a session that died without a FIN, ssh exits and retries rather
  than staying up with no tunnel. macOS's sshd drops such a session once TCP
  gives up on it. Setting `ClientAliveInterval 30` on the Mac shortens that
  wait, but it is optional.
- An invalid `[api]` (for example a LAN bind without `allow_lan`) is a config
  error and stops `dictated` at startup like any other config error. Run
  `dictated --check-config` before restarting.

## Removing it

```bash
just uninstall-mac-tunnel        # disable, stop and remove the unit and its wrapper
dictated --rotate-api-token >/dev/null   # invalidate the Mac's copy
```

Then set `[api] enabled = false` and restart `dictated` to close the port.
