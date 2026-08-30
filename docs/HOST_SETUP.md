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
| GET    | `/capabilities` | Bearer    | What this node declares it can do — see below |
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

> ⚠️ **Windows hosts: disable lock-on-wake first, or `/sleep` strands the box.**
> The suspend deliberately leaves wake events armed (`disableWakeEvent: false`),
> so Wake-on-LAN brings the host back in about a second. But Windows' *default*
> is to lock the console session on resume, and the credential UI lives on the
> Winlogon **secure desktop** — a capture/streaming process running in the
> user's session can neither capture it nor inject input into it. The result is
> a host that is powered on, answering on the network, and unreachable from the
> couch: the only way back in is to physically attach a keyboard and type the
> password.
>
> **Autologin does not cover this.** Autologin applies at *logon*; resuming from
> S3 is not a logon (the session is already there, merely locked) and Windows
> has no supported auto-unlock. The lock has to be prevented:
>
> ```powershell
> # "Require a password when a computer wakes" -> No, for every power scheme.
> # 0e796bdb-... is CONSOLELOCK under the SUB_NONE subgroup.
> $k = 'HKLM:\SOFTWARE\Policies\Microsoft\Power\PowerSettings\{0e796bdb-100d-47d6-a2d5-f7d2daa51f51}'
> New-Item -Path $k -Force | Out-Null
> Set-ItemProperty -Path $k -Name ACSettingIndex -Value 0 -Type DWord
> Set-ItemProperty -Path $k -Name DCSettingIndex -Value 0 -Type DWord
> ```
>
> Use the **Group Policy key above rather than a per-scheme
> `powercfg /SETACVALUEINDEX ... CONSOLELOCK 0`**: the policy applies to every
> scheme, so a driver install or a Windows update that flips the active scheme
> cannot silently re-arm the lock. In the homelab this is
> `windows_common_disable_lock_on_wake` in homelab-ansible's `windows-common`
> role.

### `GET /capabilities`

What this node declares it can do, so a client builds its UI and registers its
routes from the answer instead of probing. Same wire type
(`tv_shell_protocol::Capabilities`) the daemon serves over its `capabilities` IPC
command — see [IPC_PROTOCOL.md](IPC_PROTOCOL.md#capabilities) for the field table
and the forward-compatibility rule for unknown feature names.

```json
{"node_id":"desktop-2","kind":"sidecar","agent_version":"0.6.0","platform":"windows","features":["steam_library","game_launch","sleep"]}
```

Bearer-authenticated like every route but `/art/{appid}`: the feature set is an
inventory of what is reachable on this machine, and the daemon that consumes it
already holds a token.

**`node_id` resolution order:** `TV_SHELL_MQTT_DEVICE_ID` → the machine hostname
(`COMPUTERNAME` on Windows, else `/proc/sys/kernel/hostname` on Linux, else an
exported `HOSTNAME`) → `"tv-shell-host"`. Procfs outranks `HOSTNAME` because it
is the kernel's answer: `HOSTNAME` is a bash-only variable a systemd service
normally never sees, and an exported one (container, `docker exec`, inherited
shell) can be stale. The MQTT device id leads because it is already the sidecar's
explicit, never-derived identity — reusing it keeps one machine from answering to
two names. Resolved once at startup and logged; a dual-boot machine must set the
**same** id on both boots, exactly as [MQTT.md](MQTT.md) requires.

**Features report what is wired, not what would succeed now** — a closed Steam
does not drop `game_launch`. `steam_library` is unconditional; `game_launch` and
`sleep` are gated on `target_os` linux/windows, matching the `cfg` on
`steam::quit` and `power::suspend` (macOS is a CI target only: it can open a
launch URL but can never see or stop a running game, so claiming `game_launch`
there would make `/quit` a silent no-op).

## Environment

| Var | Default | Meaning |
|-----|---------|---------|
| `TV_SHELL_HOST_TOKEN` | none — see below | Bearer token. **Set it** to a stable value so the daemon can be paired. |
| `TV_SHELL_HOST_PORT`  | `47995` | Listen port (chosen outside Sunshine's 47984–47990 range). |
| `TV_SHELL_HOST_BIND`  | `0.0.0.0` | Listen address (all LAN interfaces). |

> ⚠️ **Breaking change (S4 fail-closed): the sidecar now refuses to start on a
> non-loopback bind with no `TV_SHELL_HOST_TOKEN`.** Previously an unset token
> made the sidecar mint a weak one (derived from boot time + pid — a small
> search space) and keep serving `:47995` — which accepts `/launch`, `/quit`,
> and `/sleep` (a machine-wide suspend) — over `0.0.0.0` with no real secret.
> That is no longer a safe default:
>
> - **`TV_SHELL_HOST_BIND` is loopback** (`127.0.0.1`/`::1`, the default LAN
>   bind is `0.0.0.0` so this only applies if you've explicitly narrowed it) and
>   `TV_SHELL_HOST_TOKEN` is unset: unchanged behavior — the sidecar starts and
>   generates a token, now with the OS CSPRNG (`ring::rand::SystemRandom`)
>   instead of the old boot-time/pid scramble, and still logs it once at
>   startup so you can copy it into the daemon's config.
> - **`TV_SHELL_HOST_BIND` is non-loopback** (the default `0.0.0.0`, or any LAN
>   address) and `TV_SHELL_HOST_TOKEN` is unset: **the sidecar now refuses to
>   start**, with an error naming `TV_SHELL_HOST_TOKEN` explicitly. **A fresh
>   install that never set the token will no longer come up** until you either
>   set `TV_SHELL_HOST_TOKEN` or narrow `TV_SHELL_HOST_BIND` to `127.0.0.1`.
>
> There is no escape-hatch flag for this one (unlike the daemon's
> `[dev].allow_insecure_lan`) — set a token before deploying to a LAN-reachable
> host.

Generate a token once and reuse it on both ends:

```bash
openssl rand -hex 16
```

### MQTT (optional)

The sidecar can also publish its state to an MQTT broker and accept a few
commands there — additive, never a replacement for the HTTP routes above. It has
no config file, so every knob is an env var. Topics, entity keys and the Home
Assistant discovery contract: [MQTT.md](MQTT.md).

| Var | Default | Meaning |
|-----|---------|---------|
| `TV_SHELL_MQTT_BROKER` | unset | `mqtts://host:8883` or `mqtt://host:1883`. **Unset ⇒ MQTT off entirely.** |
| `TV_SHELL_MQTT_DEVICE_ID` | — | Explicit device id. **Required once the broker is set**, and **identical on both boots** of a dual-boot machine. |
| `TV_SHELL_MQTT_USERNAME` | unset | Broker username. Both-or-neither with the password. |
| `TV_SHELL_MQTT_PASSWORD` | unset | The password itself, not a path — Windows has no mode bits. |
| `TV_SHELL_MQTT_CA_FILE` | unset | PEM CA bundle. **Optional** — unset uses the platform trust store, which is the normal path. |
| `TV_SHELL_MQTT_HEARTBEAT_SECS` | `30` | Floor republish interval. Must be > 0. |
| `TV_SHELL_MQTT_KEEPALIVE_SECS` | `60` | MQTT keepalive. Must be > 0. |

The `GAME_SHELL_*` prefix is still honoured as a fallback, as it is for every
other variable here.

**The id is never derived** from hostname or OS: this box is one physical machine
that dual-boots, and a derived id would register two Home Assistant devices for
it, one of which is offline by construction. A broker with no `DEVICE_ID` is a
configuration error, not a guess.

**A bad MQTT configuration disables MQTT only** — the sidecar starts normally and
every HTTP route above still serves, because the TV's Steam widget depends on
them. The failure is logged at `error` at startup; the symptom is "no device in
Home Assistant", never a dead Steam row. The environment is read once, so any
change (credential rotation included) needs a sidecar restart.

> **Windows trust note.** On Linux these extend the `0600` `host.env`. On Windows
> they extend the per-user environment variables, which are ACL-protected and
> readable by any process running as that user — **the same trust model as the
> bearer token already deployed there, so no regression**, but not parity with
> Linux.

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

5. **If you will use `/sleep`, disable lock-on-wake** — see the warning under
   [`POST /sleep`](#post-sleep). Without it the host suspends fine and wakes
   fine, but resumes to a lock screen no streaming client can get past, and the
   only way back in is a physically attached keyboard. This is *not* solved by
   autologin.

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

# Authenticated — node identity + declared feature set
curl -H "Authorization: Bearer <token>" http://<host-ip>:47995/capabilities

# Public — cover art for an appid (e.g. 1245620)
curl -o /tmp/art.jpg http://<host-ip>:47995/art/1245620
```

A populated `/library` and a Steam row on the TV home screen means pairing
worked. If `/library` is empty, confirm Steam is installed and has at least one
game; if it 401s, the daemon's token doesn't match the host's.
