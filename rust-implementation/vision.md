# SNAC — Project Vision & Status

## What SNAC Is

SNAC (Sophos Network Auto Connect) is a lightweight Linux automation tool that detects when the machine connects to a specific network (a college WiFi SSID or a specific wired Ethernet connection) and automatically starts a Sophos authentication/VPN binary (`caa`). When the machine disconnects, or connects to any other network, SNAC stops the binary. Walk into class, get authenticated automatically, no manual steps; walk out, get cleanly disconnected.

This is a personal daily-driver tool for a single-user Arch Linux desktop, and also a portfolio piece demonstrating systems automation, root-cause debugging, and Rust — built alongside a Network Security / DFIR career track.

## Environment

- Arch Linux on a Dell G15 5535 (Ryzen 7640HS), paired with Hyprland, dual-boot with Windows
- `paru` as AUR helper
- NetworkManager as the network backend (`NetworkManager.service` — capital N/M, case-sensitive)
- `mako`/`dunst` for desktop notifications
- Target WiFi SSID: `NFSU_students`; target Ethernet connection name is machine-specific (found via `nmcli connection show --active`)
- The Sophos binary is `caa`, normally at `~/bin/caa` — always read from `BINARY_PATH` in config, never hardcoded

## Architecture, and Why It Looks This Way

**Event-driven, not polling.** A NetworkManager dispatcher fires on every network state change and reacts, rather than a loop checking on a timer — zero idle CPU/RAM cost, near-instant trigger latency. Every implementation since the first draft has preserved this.

**Rust, not bash or Python.** The first working implementation was bash. It ran, but was unreliable in ways that took real diagnostic work to pin down (see below). Python was considered as a second implementation and dropped in favor of one carefully-built Rust replacement, chosen for memory safety and to force more rigorous error handling than bash's implicit failure modes allow.

**System-wide network matching, not interface-scoped.** The single most important architectural fix, found via a full day's journal log analysis: checking only the interface named in the firing dispatcher event — rather than checking system-wide whether the target network is active on *any* interface — was the dominant cause of the bash version randomly killing healthy sessions.

**The dispatcher and the actual VPN process are two different things, logged in two different places.** `snac.service` (a systemd `--user` unit) runs `caa` directly and is what actually gets started/stopped. The dispatcher binary is a separate, short-lived process invoked by NetworkManager on every network event; its only job is deciding whether `snac.service` should be running right now. `logger -t snac-dispatcher` (dispatcher, root, no user session) vs `journalctl --user -u snac.service` (`caa` itself) — never conflate these.

## What's Been Built So Far

1. **Bash baseline** — dispatcher script + `snac.service` systemd unit + `snac.config` KEY=VALUE file. Functional but unreliable: the Sophos binary would sometimes fail to start, or start and die almost immediately, with no obvious pattern.
2. **Full-day diagnostic log collection and analysis** — a complete day of `journalctl` output covering both the dispatcher and `caa`'s own logs, used to find three confirmed root causes rather than guessing.
3. **Rust rewrite** — a single-purpose CLI binary (`snac_dispatcher`), built to fix all three confirmed root causes plus general robustness: locking, idempotency, structured logging, no panics on expected failure paths, dynamic active-user discovery instead of a hardcoded username.
4. **Deployment debugging** — the initial Rust build and the deployment steps around it had their own bugs, found and fixed (see below).
5. **Old bash deployment fully removed** from the system.

## Diagnosed Issues & Fixes — Don't Rediscover These

From the bash-era full-day log:
1. **Interface-scoped false stops.** The dispatcher only checked the interface named in the firing event. `caa` reports IP configuration changes constantly once connected (including from its own tunnel interface), and each change fired a dispatcher event for an interface that obviously wouldn't match the target — the old logic responded by stopping a perfectly healthy session. **Fixed**: the Rust dispatcher checks system-wide whether the target network is active on any interface before deciding to stop.
2. **`snac.service` was `enable`d**, autostarting at every boot/login via `WantedBy=default.target`, bypassing the dispatcher and racing ahead of real network readiness. **Fixed**: never `enable` the service — the dispatcher's own `start`/`stop` calls are the only trigger.
3. **`caa`'s stdout is fully block-buffered** under systemd (not a tty), so individual log-line timestamps within one session were misleading — a whole session's output could land under a single flush timestamp. Systemd's own `Started`/`Stopped` timestamps remained trustworthy throughout. **Fixed**: `snac.service`'s `ExecStart` wraps the binary with `stdbuf -oL -eL`, and explicitly sets `StandardOutput=journal`/`StandardError=journal`.

From building and deploying the Rust version:
4. **`sudo` environment-variable passing bug.** Environment variables were originally passed to `sudo` as two separate argv tokens instead of one combined `VAR=value` token — since `Command` execs directly with no shell, `sudo` couldn't parse this as a valid assignment and the call failed every time. **Fixed**: dropped `sudo` entirely in favor of `runuser -u <user> -- env VAR=value ...` — the dispatcher already runs as root, so `runuser` needs no sudoers configuration at all, which also simplified deployment.
5. **Shell variable non-expansion in config values.** `snac.config` was written with `BINARY_PATH="$HOME/bin/caa"`, but config parsing is plain string splitting with no shell involved — `$HOME` is never expanded and gets stored literally, which then fails the binary-existence check. **Fixed**: config values must be literal absolute paths, no `$HOME`/`~`/shell expansion of any kind.
6. **Deployment step-ordering mistake** (an instruction bug, not a code bug): an earlier deployment checklist had the manual dry-run test listed before the config-file-creation step, so every dry run failed with `Config file missing` regardless of whether the code was correct. **Fixed**: config file creation now comes before any dry run.
7. **`systemctl --user status snac.service` → "Unit snac.service not found."** Means the `systemd --user` manager has no record of the unit — narrows to: the file isn't actually at `~/.config/systemd/user/snac.service`, `daemon-reload` was run against the wrong systemd instance (`sudo systemctl daemon-reload` reloads the *system* instance, not `--user`), or the unit file has a syntax error severe enough that systemd silently skips it during a scan. **Status: confirmed resolved (2026-08-23)** — `systemctl --user status snac.service` now reports `Loaded: loaded ... disabled` as intended. Diagnostic commands, kept for reference if it recurs:
   ```bash
   ls -la ~/.config/systemd/user/snac.service
   systemctl --user daemon-reload
   systemctl --user list-unit-files | grep -i snac
   systemctl --user status snac.service
   systemd-analyze --user verify ~/.config/systemd/user/snac.service
   ```
8. **`TARGET_ETH_NAME` case mismatch silently defeated wired matching.** Live config had `"Wired Connection 1"` (capital C); `nmcli` reports the connection as `"Wired connection 1"` (lowercase c). Matching is exact-string/case-sensitive, so every dispatcher event logged `System-wide match: false` despite being on the target wired connection. **Fixed (2026-08-23)**: corrected the live config; documented in `README.md`'s troubleshooting table to re-derive the value from `nmcli` rather than typing it from memory.

## Known Config/Deployment Gotchas — Checklist

- Config file (`~/.config/snac/snac.config`) values are literal strings, no shell expansion — no `$HOME`, no `~`.
- Never `systemctl --user enable snac.service`.
- No sudoers file is needed — the dispatcher uses `runuser`, not `sudo`.
- Install the dispatcher binary to a stable location (`/usr/local/bin/snac-dispatcher`) and reference it via a single symlink at `/etc/NetworkManager/dispatcher.d/99-snac`, rather than pointing directly at a `target/release/` build directory (breaks on `cargo clean` or a repo move) or using per-implementation filenames toggled with `chmod` (risks two dispatchers active at once).
- `daemon-reload` must be run as the regular user (`systemctl --user daemon-reload`), not as root.

## File Layout

| File | Path |
|---|---|
| Config | `~/.config/snac/snac.config` |
| Systemd user service | `~/.config/systemd/user/snac.service` |
| Rust dispatcher source | in-repo, built via `cargo build --release` → `target/release/snac_dispatcher` |
| Installed dispatcher binary | `/usr/local/bin/snac-dispatcher` |
| Active dispatcher entry | `/etc/NetworkManager/dispatcher.d/99-snac` (symlink → installed binary) |

## Current Status

Code is complete and has been reviewed line-by-line — see "What's Been Built," plus a further
correctness/robustness pass on `src/main.rs` (2026-08-23): nmcli exit-status checking, nmcli calls
now `timeout`-wrapped, nmcli terse-output colon-escaping, config parsing edge cases (bare `#`,
unterminated quotes), and de-duplicated the `runuser`/`env` invocation pattern into one helper.
`cargo build --release` and `cargo test` both pass. Deployment is confirmed working end-to-end on
this machine: binary installed at `/usr/local/bin/snac-dispatcher`, dispatcher symlink live, unit
loaded and correctly *not* enabled, and the `TARGET_ETH_NAME` case-mismatch bug (issue 8 above) is
fixed. `README.md` and `LICENSE` (MIT) are written — see `README.md` for the deploy/config/troubleshoot
reference; this file remains the historical journal.

A separate, related need: the professor who administers the Sophos gateway asked for `caa`'s connection stability to be monitored and timed across a full day, since it intermittently drops sessions server-side. The journal-capture settings in `snac.service` exist specifically to make that data trustworthy.
