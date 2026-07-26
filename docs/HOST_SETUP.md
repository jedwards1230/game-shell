# Host Setup & Pairing

`tv-shell-host` is a small cross-platform sidecar you run **on the gaming PC**
(the Steam machine that Sunshine streams from). The tv-shell TV client's daemon
talks to it over HTTP to list installed Steam games, show their art on the home
screen, and launch a game into Big Picture before Moonlight streams it.

```
tv-shell client (TV)                      gaming PC (Steam host)
  tv-shell-input daemon ──HTTP :47995──▶ tv-shell-host
    [steam] url   (config.toml)              TV_SHELL_HOST_TOKEN  (env)
    [steam] token (bearer, must match the host token)
```

It never touches Sunshine config, so other Moonlight clients are unaffected.

> ⚠️ **Security — trusted LANs only.** The daemon talks to the host over
> **unencrypted HTTP** with a **static bearer token**. That's safe only on an
> isolated, trusted LAN where you control every host — a passive observer (ARP
> spoof, rogue DHCP, WiFi sniffer) on the same segment can capture the token.
> Do not expose the host to guest WiFi, untrusted networks, or the Internet
> without putting it behind TLS or a VPN (e.g. Tailscale). The token is a shared
> secret stored in plaintext on both machines and is not rotated automatically —
> rotate it by hand if it leaks or when moving to a different network.

## Endpoints

| Method | Path          | Auth        | Purpose |
|--------|---------------|-------------|---------|
| GET    | `/library`    | Bearer      | Enumerate installed Steam games (VDF/ACF) |
| POST   | `/launch`     | Bearer      | Navigate Big Picture to a game's page (user presses Play) |
| POST   | `/open-bpm`   | Bearer      | Open Big Picture's HOME screen (no game selected). No body |
| POST   | `/quit`       | Bearer      | Gracefully stop the running game for `{ appid }` (SIGTERM to its process group, like Steam's Stop). May report **nothing to quit** — see below |
| POST   | `/sleep`      | Bearer      | Suspend the host to RAM. No body. May be **refused** — see below |
| GET    | `/status`     | Bearer      | `{ version, running_appid, streaming }` — `running_appid` is **process-verified**, see below |
| GET    | `/art/{appid}`| **public**  | Local cover art (no bearer — QML `Image.source` can't send one; art isn't sensitive) |

### `POST /quit`

Request body is `{ "appid": <n> }`. The response is always the same three fields:

```jsonc
{ "ok": true,  "appid": 730,    "reason": null }          // a game process was signalled
{ "ok": false, "appid": 252950, "reason": "not running" } // nothing matched — nothing was quit
```

**"Nothing to quit" is HTTP 200 with `ok: false`, not an error status** — same
contract as `/sleep`'s refusal, so the caller can tell "stopped the game" from
"there was no game" instead of reading a bare 2xx as success.

### `GET /status` — `running_appid` liveness

`running_appid` reports a game only when a **live process** backs it:

- **Linux** scans `/proc` for Steam's `reaper SteamLaunch AppId=<n>` launcher, so
  the signal *is* the process table — it can't go stale.
- **Windows** reads `HKCU\Software\Valve\Steam\RunningAppID`, then cross-checks it
  against the process table (any running executable inside that appid's install
  directory, resolved from its own `appmanifest_<appid>.acf`). The registry value
  alone is only a *claim*: Steam does not clear it when its client crashes, so it
  can name a game that exited hours ago. The cross-check runs the same install-dir
  match `/quit` uses, so `/status` and `/quit` can't disagree.

The check is deliberately conservative — if the install directory can't be
resolved, or the process enumeration fails, `running_appid` is `null`. A phantom
"running" locks the shell's UI down (poster badge, focus restriction, context
menu); a missed one only omits a badge. It costs one process enumeration per
`/status` call, but **only when the registry claims a non-zero appid** — an idle
host short-circuits and pays nothing.

### `POST /sleep`

No request body. The response is always the same two fields:

```jsonc
{ "ok": true,  "reason": null }                      // accepted — suspend dispatched
{ "ok": false, "reason": "a game is running on the host" }   // refused
```

**A refusal is HTTP 200 with `ok: false`, not an error status** — "I decided not
to" is a normal answer, so the caller shows `reason` to the user instead of
retrying a transport failure. `reason` is always present (JSON `null` on
success), so a consumer binds one field either way.

The host refuses while:

- a Steam game is running (`running_appid` is set — process-verified, so a stale
  Windows `RunningAppID` no longer blocks sleep indefinitely), or
- Sunshine reports a live session — **active *or merely resumable***, the same
  `serverinfo` signal `/status`'s `streaming` field uses. A resumable session
  counts: sleeping would strand a client that still lists this host.

When both are true the running-game reason wins (deterministic, so the message
never depends on probe ordering). `ok: true` means the suspend was **accepted and
dispatched to the OS**, not that the machine is already asleep — the process may
be frozen before any confirmation could be sent.

On Linux the suspend is `systemctl suspend`. On Windows it is a `powershell`
shell-out to .NET `Application.SetSuspendState`, *not*
`rundll32 powrprof.dll,SetSuspendState` — that entry point ignores its arguments
and **hibernates** whenever hibernation is enabled. See `host/src/power.rs`.

> ⚠️ **`/sleep` raises the blast radius of a leaked bearer token.** Before this
> endpoint the token bought Steam library enumeration and game launch/quit;
> it now also buys the ability to **suspend the machine** — a trivial denial of
> service against the gaming PC and anyone streaming from it. Nothing else about
> the security model changed (still plaintext HTTP + a static shared secret on a
> trusted LAN), but rotate the token accordingly if it leaks.

## Environment

| Var | Default | Meaning |
|-----|---------|---------|
| `TV_SHELL_HOST_TOKEN` | random (logged at startup) | Bearer token. **Set it** to a stable value so the daemon can be paired. |
| `TV_SHELL_HOST_PORT`  | `47995` | Listen port (chosen outside Sunshine's 47984–47990 range). |
| `TV_SHELL_HOST_BIND`  | `0.0.0.0` | Listen address (all LAN interfaces). |

Generate a token once and reuse it on both ends:

```bash
openssl rand -hex 16
```

---

## Install path A — Ansible-managed (homelab)

On the gaming host the service is a managed `systemd --user` unit via the `desktop-common`
role. You don't run anything by hand — set the flags and apply:

```yaml
# host_vars/gaming-host.yaml
tv_shell_host_enabled: true
tv_shell_host_install_method: fetch          # download the released binary
tv_shell_host_version: "0.1.0"               # the host-v<version> release tag
tv_shell_host_binary_sha256: "<from the release checksums.txt>"
tv_shell_host_token: !vault | ...            # vault-encrypted; reuse the daemon's token
```

```bash
ansible-playbook playbooks/site-desktop.yml --tags tv-shell-host,firewall
```

The role installs the binary to `/usr/local/bin/tv-shell-host`, writes a `0600`
env file, enables linger so the unit survives logout/reboot, and opens the LAN
firewall to port 47995. See the role for the full variable list.

## Install path B — manual (any gaming PC)

1. **Download** the binary for your OS from the latest `host-v*`
   [release](https://github.com/jedwards1230/tv-shell/releases):

   | OS | Asset |
   |----|-------|
   | Linux | `tv-shell-host-x86_64-unknown-linux-musl` |
   | macOS (Apple Silicon) | `tv-shell-host-aarch64-apple-darwin` |
   | macOS (Intel) | `tv-shell-host-x86_64-apple-darwin` |
   | Windows | `tv-shell-host-x86_64-pc-windows-msvc.exe` |

   Verify it against the release `checksums.txt`, then install it
   (e.g. `install -m755 tv-shell-host-* /usr/local/bin/tv-shell-host`).

2. **Run it** with a stable token. Quick smoke test:

   ```bash
   TV_SHELL_HOST_TOKEN=<token> tv-shell-host
   # → tv-shell-host listening on 0.0.0.0:47995
   ```

   To keep it running, install it as a service. On Linux a user unit bound to the
   graphical session works (Steam must reach the live desktop session to launch a
   game) — mirror `roles/desktop-common/templates/tv-shell-host.service.j2` from
   the homelab-ansible repo. On Windows use Task Scheduler (at-logon); on macOS a
   launchd LaunchAgent.

3. **Open the firewall** to the LAN so the TV box can reach it:

   ```bash
   # Linux (firewalld)
   firewall-cmd --permanent --add-rich-rule='rule family="ipv4" \
     source address="192.0.2.0/24" port port="47995" protocol="tcp" accept'
   firewall-cmd --reload
   ```

   The `/24` source opens the port to the whole subnet — fine if every host on it
   is trusted. To tighten it, scope the rule to the TV client's IP only (e.g.
   `source address="192.0.2.50/32"`) so a guest or compromised device on the LAN
   can't reach the control surface.

### Windows

1. **Download** `tv-shell-host-x86_64-pc-windows-msvc.exe` from the latest
   `host-v*` [release](https://github.com/jedwards1230/tv-shell/releases) and
   verify it against `checksums.txt`.

2. **Set env vars** for the same user session Steam runs in: `TV_SHELL_HOST_TOKEN`
   (the **same** token used on the Linux boot, see the dual-boot note below),
   optionally `STEAM_PATH` (only if Steam isn't at the default
   `C:\Program Files (x86)\Steam`) and `TV_SHELL_HOST_PORT`.

3. **Open the firewall** (LAN-scoped):

   ```powershell
   New-NetFirewallRule -DisplayName "tv-shell-host" -Direction Inbound -Action Allow `
     -Protocol TCP -LocalPort 47995 -RemoteAddress 192.0.2.0/24
   ```

4. **Auto-start at logon** via Task Scheduler (no extra deps). Set the token as a
   persistent user env var first (Task Scheduler inherits the user environment),
   then register the task:

   ```powershell
   setx TV_SHELL_HOST_TOKEN <token>
   schtasks /Create /TN "tv-shell-host" /TR "\"C:\path\to\tv-shell-host.exe\"" `
     /SC ONLOGON /RL LIMITED /F
   ```

   Steam must be running in the same interactive session for launches to work.

> **Dual-boot note.** A dual-boot gaming PC may present the **same LAN IP from
> both OSes** or a **different IP per OS** (per-OS static leases / hostnames).
> Same IP: reuse the same `TV_SHELL_HOST_TOKEN` on both OSes so the TV daemon's
> single `[steam]` config (`url` + `token`) never has to change — whichever OS
> is booted answers on the same IP:port. Different IPs: declare one
> `[[steam.hosts]]` entry per OS identity (see `config/config.toml.example`)
> and switch the active one from the couch — Widgets ▸ Steam ▸ Server (the
> `steam-set-host` IPC). The widget's Wake card always targets the active
> entry's host.

> **Big Picture nav timing on Windows**: the daemon fires the `steam://nav/...`
> URL immediately with no "is Big Picture up yet?" wait (unlike Linux) — see
> `host/src/launch.rs`'s `wait_for_bigpicture` doc comment for why.

---

## Pair the daemon (the TV box)

Point the daemon at the host and give it the **same token**:

```toml
# ~/.config/tv-shell/config.toml on the tv-shell client
[steam]
url = "http://<host-ip>:47995"
# Either inline …
token = "<same token as TV_SHELL_HOST_TOKEN>"
# … or, preferred, a 0600 file: token_file = "~/.config/tv-shell/steam-token"
```

Restart the daemon to pick it up. Under config management this is typically
wired by your deployment role — set the Steam URL and token via your own host
variables and render `config.toml` from them.

Keep the token private: it's the same secret on both machines in plaintext. Prefer
`token_file` and `chmod 0600` it (the host's own env file too), keep it out of
shell history, and vault it in any config repo. It isn't rotated by default —
rotate it on both ends together if it leaks.

## Verify

```bash
# Authenticated — lists games
curl -H "Authorization: Bearer <token>" http://<host-ip>:47995/library

# Public — cover art for an appid (e.g. 1245620)
curl -o /tmp/art.jpg http://<host-ip>:47995/art/1245620
```

A populated `/library` and a Steam row on the TV home screen means pairing
worked. If `/library` is empty, confirm Steam is installed and has at least one
game; if it 401s, the daemon's token doesn't match the host's.
