# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

SNAC (Sophos Network AutoConnect) dispatcher — a small Rust binary invoked by NetworkManager as a
`dispatcher.d` script. It detects whether the machine is on a configured "trusted" network (by WiFi
SSID or connection/interface name) and, if so, starts a per-user systemd unit (`snac.service`) that
runs some other binary (e.g. a VPN/proxy client); if not, it stops that unit. The whole project is a
single Rust source file plus two plain-text config artifacts — there is no multi-crate structure.

## Commands

- Build: `cargo build --release` — produces `target/release/snac_dispatcher`, which NetworkManager
  invokes with `<interface> <action>` as argv (this is the dispatcher script contract).
- Test: `cargo test` — currently one unit test, `test_config_value_parsing`, covering
  `parse_config_value`'s quote/comment stripping.
- Run a single test: `cargo test test_config_value_parsing`.

There is no linter/formatter config beyond the Cargo defaults; `cargo fmt` / `cargo clippy` can be
used but aren't wired into any CI in this repo.

## Architecture

Everything lives in `src/main.rs` and runs as a one-shot process per NetworkManager event (not a
daemon). `main()` is the entry point and orchestrates, in order:

1. **Argument/action filtering** — argv is `[binary, interface, action]` per the NM dispatcher
   convention. Only a fixed set of actions (`up`, `down`, `vpn-up`, `vpn-down`, `dhcp4-change`,
   `dhcp6-change`, `connectivity-change`) are processed; anything else exits immediately.
2. **Locking** — an flock on `/run/snac-dispatcher.lock` (via `rustix::fs::flock`) serializes
   overlapping dispatcher invocations, since NetworkManager can fire multiple events in quick
   succession.
3. **User resolution** (`resolve_target_user`) — scans `/run/user/<uid>` for a non-root UID with an
   active `bus` socket to find the logged-in desktop user, since this binary runs as root (invoked by
   NetworkManager) but needs to act on that user's systemd `--user` session and notification bus.
4. **Config loading** (`load_config` / `parse_config_value`) — reads
   `/home/<user>/.config/snac/snac.config`, a shell-env-like `KEY=value` format supporting quoted
   values and trailing `#` comments. Recognized keys: `TARGET_SSID`, `TARGET_ETH_NAME`,
   `BINARY_PATH` (required; the others are optional and either may be blank).
5. **Network match check** (`is_target_active_systemwide`) — shells out to `nmcli` twice: once for
   active WiFi SSIDs, once for active connection names (covers both WiFi-by-SSID and
   ethernet-by-connection-name matching). This is a systemwide check, independent of which
   `interface` triggered the event.
6. **Service reconciliation** — compares the match result against current state
   (`is_service_active`) and calls `run_systemctl_user` to `start`/`stop` `snac.service` as needed,
   each via `timeout 5 runuser -u <user> -- env DBUS_SESSION_BUS_ADDRESS=... XDG_RUNTIME_DIR=...
   systemctl --user ...`. All privileged-user shellouts follow this same
   `runuser`+`env`+`DBUS_SESSION_BUS_ADDRESS`+`XDG_RUNTIME_DIR` pattern (also used by
   `send_notification` for `notify-send`).
7. **Logging/notification** — `log_syslog` shells out to `logger` (tagged `snac-dispatcher`);
   `send_notification` shells out to `notify-send` as the target user, for both error conditions
   (missing config, missing binary, systemctl failures) and normal state transitions.

`snac.config` is the user-facing config file (installed at `~/.config/snac/snac.config`) — it's also
read directly by `snac.service` as an `EnvironmentFile`, so its `BINARY_PATH` value both drives the
dispatcher's existence check and is substituted into the service's `ExecStart=`. `snac.service` is a
systemd `--user` unit template (`%h` = the user's home) meant to be installed under
`~/.config/systemd/user/`.

## Constraints that aren't obvious from the code

These come from real regressions hit while building this (full history in `vision.md`); breaking any
of them reintroduces a previously-fixed bug:

- **`snac.config` values are never shell-expanded.** `parse_config_value` is plain string splitting,
  not a shell eval, so `$HOME`/`~` in a value is stored literally and then fails the binary-existence
  check. (The `BINARY_PATH="$HOME/bin/caa"` in the repo's own `snac.config` is an example of this
  trap, not a working value — real deployments need a literal absolute path.)
- **Never `systemctl --user enable snac.service`.** The `[Install]` section in `snac.service` exists
  for completeness, but the dispatcher's own `start`/`stop` calls in `run_systemctl_user` are meant to
  be the only thing that starts the unit. Enabling it lets the service autostart at login and race
  ahead of real network readiness.
- **Network matching must stay system-wide**, not scoped to the `interface` argv passed by the firing
  event — `is_target_active_systemwide` checks all interfaces deliberately, because the VPN binary's
  own tunnel interface fires unrelated dispatcher events that would otherwise look like a disconnect
  and kill a healthy session.
- **No sudoers config is used or needed** — privileged-user shellouts use `runuser`, not `sudo`
  (`sudo` couldn't be made to accept the `env VAR=value` argv pattern used here).

## Project docs

`README.md` is the deployment/config/troubleshooting reference (install steps, file locations, the
`runuser`-not-`sudo` and never-`enable` rules, a symptom→root-cause→fix table). `vision.md` is the
living project journal — environment details (Arch/Hyprland/NetworkManager), full diagnostic history
behind each fix, and current status. Check `vision.md` for status/history; check `README.md` for
"how do I deploy/fix this."
