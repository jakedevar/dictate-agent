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
                (sshd listens here)          dictate-mac-tunnel.service (hub dials out)
```

This page covers the **hub side**. The Mac app's own build, install and
permission notes live with it under `mac/`.

## Security model

- **Loopback on both ends.** `dictated` listens on `127.0.0.1:7313` only. The
  tunnel asks the Mac's sshd for `-R 127.0.0.1:7313:...`, so the port appears
  on the Mac's loopback and nowhere else. No other machine on either network
  can reach it.
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
- **Never `GatewayPorts yes` on the Mac.** macOS's default (`no`) makes sshd
  bind remote forwards to loopback regardless. `yes` would bind `0.0.0.0` and
  publish the API to the Mac's network.
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
   `make install-mac-tunnel`). This writes
   `~/.config/systemd/user/dictate-mac-tunnel.service`, then enables and
   starts it. `dictate doctor` then shows `Mac app tunnel: … is running`.

4. **Copy the token to the Mac:** `just mac-token` (`--host other-mac` for
   another host). It prints where it wrote the token and that the Mac's copy
   matches. It never prints the token.

5. **Check from the Mac** (prints the daemon's status JSON, then `401`):

   ```bash
   ssh mac-mini 'curl -s -H "Authorization: Bearer $(cat ~/Library/Application\ Support/DictateHub/api-token)" http://127.0.0.1:7313/v1/status'
   ssh mac-mini 'curl -s -o /dev/null -w "%{http_code}\n" http://127.0.0.1:7313/v1/status'
   ```

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
just uninstall-mac-tunnel        # disable, stop and remove the unit
dictated --rotate-api-token >/dev/null   # invalidate the Mac's copy
```

Then set `[api] enabled = false` and restart `dictated` to close the port.
