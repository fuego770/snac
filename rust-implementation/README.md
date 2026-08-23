# SNAC — Sophos Network AutoConnect

## Overview

SNAC watches NetworkManager for connection changes and starts a Sophos VPN/auth binary (`caa`) when
you're on a configured trusted network (WiFi SSID or wired connection name), stopping it the moment
you leave that network — no manual connect/disconnect.

## Installation

Assumes Arch Linux, NetworkManager, and systemd `--user` sessions. Arch-specific steps are flagged.

**1. Prerequisites**

- A Rust toolchain new enough for edition 2024 (`rustc` ≥ 1.85). Arch: `sudo pacman -S rust`.
- NetworkManager running as `NetworkManager.service` (capital N/M — case-sensitive unit name).

**2. Clone and build**

```bash
git clone https://github.com/fuego770/snac.git
cd snac/rust-implementation
cargo build --release
```

**3. Install the dispatcher binary to a stable location**

Don't point the dispatcher entry directly at `target/release/`— it breaks on `cargo clean` or a repo
move.

```bash
sudo install -Dm755 target/release/snac_dispatcher /usr/local/bin/snac-dispatcher
```

**4. Wire the NetworkManager dispatcher entry**

```bash
sudo ln -sf /usr/local/bin/snac-dispatcher /etc/NetworkManager/dispatcher.d/99-snac
```

A single symlink, not a copy — updates land automatically on the next `install` above.

**5. Create the config file** (before doing any dry run — see [Troubleshooting](#known-limitations--troubleshooting))

```bash
mkdir -p ~/.config/snac
cp snac.config ~/.config/snac/snac.config
$EDITOR ~/.config/snac/snac.config
```

Set these three keys:

| Key | How to get the value |
|---|---|
| `TARGET_SSID` | `nmcli -t -f active,ssid dev wifi \| grep '^yes:' \| cut -d: -f2-` |
| `TARGET_ETH_NAME` | `nmcli -t -f NAME,DEVICE,TYPE connection show --active` — copy the `NAME` field |
| `BINARY_PATH` | Absolute path to `caa` (or whichever binary you're auto-starting) |

> [!IMPORTANT]
> Matching is exact-string and case-sensitive against what `nmcli` reports. `"Wired Connection 1"` and
> `"Wired connection 1"` are different strings — get the value from the `nmcli` command above, don't
> type it from memory or the connection's display name in a GUI.

> [!WARNING]
> Config values are never shell-expanded — `parse_config_value` is plain string splitting, not a
> shell eval. `BINARY_PATH="$HOME/bin/caa"` is stored as the literal string `$HOME/bin/caa` and will
> fail the binary-existence check. Use a literal absolute path, e.g. `BINARY_PATH="/home/you/bin/caa"`.

Either `TARGET_SSID` or `TARGET_ETH_NAME` may be left blank if you only need to match one.

**6. Install the systemd user service**

```bash
mkdir -p ~/.config/systemd/user
cp snac.service ~/.config/systemd/user/snac.service
systemctl --user daemon-reload
```

Run `daemon-reload` as your regular user — `sudo systemctl daemon-reload` reloads the *system*
instance, not `--user`, and won't pick this up.

**7. Verify**

```bash
sudo /usr/local/bin/snac-dispatcher <your-interface> up
journalctl -t snac-dispatcher -n 5 --no-pager
```

Should log a match/no-match decision with no `[ERROR]` lines. From here, NetworkManager fires this
automatically on real connection events — no manual invocation needed.

## File locations

| File | Path |
|---|---|
| Config | `~/.config/snac/snac.config` |
| Systemd user service | `~/.config/systemd/user/snac.service` |
| Dispatcher source | `src/main.rs`, built via `cargo build --release` |
| Installed dispatcher binary | `/usr/local/bin/snac-dispatcher` |
| Active dispatcher entry | `/etc/NetworkManager/dispatcher.d/99-snac` (symlink → installed binary) |

## Permissions & deployment

> [!WARNING]
> Never run `systemctl --user enable snac.service`. The `[Install]` section in `snac.service` exists
> for completeness only — the dispatcher's own `start`/`stop` calls are meant to be the *only* thing
> that starts the unit. Enabling it lets the service autostart at login and race ahead of real network
> readiness, which was a confirmed root cause of failures in the original bash version.

No sudoers configuration is used or needed. The dispatcher runs as root (invoked by NetworkManager)
and acts on your user session via `runuser -u <user> -- env DBUS_SESSION_BUS_ADDRESS=... \
XDG_RUNTIME_DIR=... <command>` rather than `sudo` — `sudo` can't be made to accept the `env VAR=value`
argv pattern this needs, and `runuser` requires no additional privilege configuration.

## Monitoring & health checks

Two independent log streams — don't conflate them:

```bash
journalctl -t snac-dispatcher -f       # the dispatcher itself (root, one-shot per NM event)
journalctl --user -u snac.service -f   # caa's own output
```

`caa`'s stdout is fully block-buffered when not attached to a tty, which can make individual log-line
timestamps misleading (a whole session's output can land under one flush timestamp). `snac.service`
wraps `caa` with `stdbuf -oL -eL` and sets `StandardOutput=journal`/`StandardError=journal` so
systemd's own `Started`/`Stopped` timestamps stay trustworthy even if in-process line timestamps
aren't.

## Known limitations / troubleshooting

| Symptom | Root cause | Fix |
|---|---|---|
| Healthy session gets killed while still connected | Matching scoped to the interface named in the firing NM event — `caa`'s own tunnel interface fires unrelated events that look like a disconnect | Dispatcher checks network state system-wide on every event, regardless of which interface fired it |
| Service starts on every boot/login, ignoring actual network state | `snac.service` was `enable`d, autostarting via `WantedBy=default.target` | Never enable the unit — only the dispatcher's `start`/`stop` calls manage it |
| Target network is active but SNAC never starts `caa` | `TARGET_SSID`/`TARGET_ETH_NAME` doesn't exact-match `nmcli`'s reported name (case, extra whitespace) | Re-derive the value with the `nmcli` commands in [Installation](#installation) rather than typing it from memory |
| `BINARY_PATH` "not found" despite the binary existing | Shell variables (`$HOME`, `~`) in config values are stored literally, never expanded | Use a literal absolute path |
| Brief `Config file missing at /home/<other-user>/...` errors right after boot, before you log in | A display manager's own greeter session can briefly be the only `/run/user/<uid>` with a D-Bus socket, so `resolve_target_user` picks it momentarily | Harmless — resolves to your own session once you log in; on a genuinely single-user machine this doesn't recur |
| `systemctl --user status snac.service` → `Unit snac.service not found` | Unit file isn't at `~/.config/systemd/user/snac.service`, or `daemon-reload` was run as root instead of as your user | Re-check the file's location and re-run `systemctl --user daemon-reload` as your regular user |

## License

MIT — see [LICENSE](LICENSE).
