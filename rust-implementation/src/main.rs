use rustix::fs::{flock, FlockOperation};
use std::env;
use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::Command;

const LOCK_FILE_PATH: &str = "/run/snac-dispatcher.lock";
const LOG_TAG: &str = "snac-dispatcher";
const EXEC_TIMEOUT_SECS: &str = "5";

struct Config {
    target_ssid: String,
    target_eth_name: String,
    binary_path: String,
}

struct UserContext {
    username: String,
    uid: String,
}

fn log_syslog(message: &str, is_error: bool) {
    let prefix = if is_error { "[ERROR] " } else { "[INFO] " };
    let full_msg = format!("{}{}", prefix, message);
    let _ = Command::new("logger")
        .args(["-t", LOG_TAG, &full_msg])
        .status();
}

fn parse_config_value(raw: &str) -> Result<String, String> {
    let mut s = raw.trim();

    // Check if value starts with a quote
    if let Some(quote_char) = s.chars().next().filter(|&c| c == '"' || c == '\'') {
        return match s[1..].find(quote_char) {
            Some(end_quote_idx) => Ok(s[1..1 + end_quote_idx].to_string()),
            None => Err(format!("unterminated {} quote", quote_char)),
        };
    }

    // Strip a trailing comment, but only when '#' is preceded by whitespace —
    // an unquoted value legitimately containing '#' (e.g. an SSID) must not be truncated.
    if let Some(pos) = s.find(" #").or_else(|| s.find("\t#")) {
        s = &s[..pos];
    }

    s = s.trim();

    // Strip single pair of surrounding quotes if still present
    if (s.starts_with('"') && s.ends_with('"')) || (s.starts_with('\'') && s.ends_with('\'')) {
        if s.len() >= 2 {
            s = &s[1..s.len() - 1];
        }
    }

    Ok(s.trim().to_string())
}

fn load_config(config_path: &Path) -> Result<Config, String> {
    if !config_path.exists() {
        return Err(format!("Config file missing at {}", config_path.display()));
    }

    let file = File::open(config_path).map_err(|e| format!("Failed to open config: {}", e))?;
    let reader = BufReader::new(file);

    let mut target_ssid = String::new();
    let mut target_eth_name = String::new();
    let mut binary_path = String::new();

    for (line_num, line_res) in reader.lines().enumerate() {
        let line = line_res.map_err(|e| format!("Error reading line {}: {}", line_num + 1, e))?;
        let trimmed = line.trim();

        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        if let Some((key, val_raw)) = trimmed.split_once('=') {
            let key = key.trim();
            let parsed_val = parse_config_value(val_raw)
                .map_err(|e| format!("Error on line {}: {}", line_num + 1, e))?;

            match key {
                "TARGET_SSID" => target_ssid = parsed_val,
                "TARGET_ETH_NAME" => target_eth_name = parsed_val,
                "BINARY_PATH" => binary_path = parsed_val,
                _ => {}
            }
        }
    }

    if binary_path.is_empty() {
        return Err("BINARY_PATH is not defined in snac.config".into());
    }

    Ok(Config {
        target_ssid,
        target_eth_name,
        binary_path,
    })
}

fn resolve_target_user() -> Result<UserContext, String> {
    // Find active non-root user with a running D-Bus session
    let entries = fs::read_dir("/run/user").map_err(|e| format!("Cannot read /run/user: {}", e))?;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(uid_str) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if uid_str == "0" || !path.join("bus").exists() {
            continue;
        }

        // A failure resolving this one candidate (transient spawn error, etc.) shouldn't
        // abort the scan — keep looking at the remaining UIDs.
        let Ok(output) = Command::new("id").args(["-nu", uid_str]).output() else {
            continue;
        };

        if output.status.success() {
            let username = String::from_utf8_lossy(&output.stdout).trim().to_string();
            return Ok(UserContext {
                username,
                uid: uid_str.to_string(),
            });
        }
    }
    Err("No active desktop user session with a valid D-Bus socket found in /run/user/".into())
}

/// Runs `args` as `user`'s desktop session (their D-Bus bus + XDG runtime dir), via
/// `timeout <secs> runuser -u <user> -- env DBUS_SESSION_BUS_ADDRESS=... XDG_RUNTIME_DIR=... <args>`.
fn run_as_user(user: &UserContext, args: &[&str]) -> Result<std::process::Output, String> {
    let dbus_env = format!("DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/{}/bus", user.uid);
    let xdg_env = format!("XDG_RUNTIME_DIR=/run/user/{}", user.uid);

    let mut full_args: Vec<&str> = vec![
        EXEC_TIMEOUT_SECS,
        "runuser",
        "-u",
        &user.username,
        "--",
        "env",
        dbus_env.as_str(),
        xdg_env.as_str(),
    ];
    full_args.extend_from_slice(args);

    Command::new("timeout")
        .args(&full_args)
        .output()
        .map_err(|e| format!("Failed to run command as user {}: {}", user.username, e))
}

fn send_notification(user: &UserContext, message: &str, urgency: &str) {
    let _ = run_as_user(user, &["notify-send", "-u", urgency, "SNAC", message]);
}

/// Splits one line of `nmcli -t` output into fields, undoing nmcli's terse-mode escaping
/// (`\:` for a literal colon inside a field, `\\` for a literal backslash) so field values
/// containing ':' (e.g. an SSID or connection name) aren't split apart.
fn parse_nmcli_terse_line(line: &str) -> Vec<String> {
    let mut fields = Vec::new();
    let mut current = String::new();
    let mut chars = line.chars();

    while let Some(c) = chars.next() {
        match c {
            '\\' => match chars.clone().next() {
                Some(next @ (':' | '\\')) => {
                    current.push(next);
                    chars.next();
                }
                _ => current.push(c),
            },
            ':' => {
                fields.push(std::mem::take(&mut current));
            }
            _ => current.push(c),
        }
    }
    fields.push(current);
    fields
}

fn run_nmcli(args: &[&str]) -> Result<String, String> {
    let mut full_args: Vec<&str> = vec![EXEC_TIMEOUT_SECS, "nmcli"];
    full_args.extend_from_slice(args);

    let output = Command::new("timeout")
        .args(&full_args)
        .output()
        .map_err(|e| format!("Failed to run nmcli: {}", e))?;

    if !output.status.success() {
        let err_msg = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(format!(
            "nmcli {} exited with {}: {}",
            args.join(" "),
            output.status,
            err_msg
        ));
    }

    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn is_target_active_systemwide(config: &Config) -> Result<(bool, String), String> {
    // 1. Check WiFi SSIDs system-wide
    if !config.target_ssid.is_empty() {
        let text = run_nmcli(&["-t", "-f", "active,ssid", "dev", "wifi"])?;
        for line in text.lines() {
            let fields = parse_nmcli_terse_line(line);
            if let [active, ssid] = fields.as_slice()
                && active == "yes"
                && ssid == &config.target_ssid
            {
                return Ok((true, format!("Target WiFi SSID matched: '{}'", ssid)));
            }
        }
    }

    // 2. Check Active Connection Names system-wide
    let text = run_nmcli(&["-t", "-f", "NAME,DEVICE,TYPE", "connection", "show", "--active"])?;
    for line in text.lines() {
        let fields = parse_nmcli_terse_line(line);
        if let Some(conn_name) = fields.first()
            && ((!config.target_eth_name.is_empty() && conn_name == &config.target_eth_name)
                || (!config.target_ssid.is_empty() && conn_name == &config.target_ssid))
        {
            return Ok((true, format!("Target connection matched: '{}'", conn_name)));
        }
    }

    Ok((false, "No active connection matches target configuration".into()))
}

fn run_systemctl_user(user: &UserContext, action: &str) -> Result<bool, String> {
    let output = run_as_user(user, &["systemctl", "--user", action, "snac.service"])?;

    if !output.status.success() {
        let err_msg = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(if err_msg.is_empty() {
            format!("systemctl {} returned non-zero exit code", action)
        } else {
            err_msg
        });
    }

    Ok(true)
}

fn is_service_active(user: &UserContext) -> bool {
    run_as_user(user, &["systemctl", "--user", "is-active", "--quiet", "snac.service"])
        .is_ok_and(|o| o.status.success())
}

fn main() {
    let args: Vec<String> = env::args().collect();
    let interface = args.get(1).map(|s| s.as_str()).unwrap_or("unknown");
    let action = args.get(2).map(|s| s.as_str()).unwrap_or("unknown");

    // Filter relevant NetworkManager action events
    match action {
        "up" | "down" | "vpn-up" | "vpn-down" | "dhcp4-change" | "dhcp6-change" | "connectivity-change" => {}
        _ => return,
    }

    // Acquire lock to prevent race conditions across overlapping events
    let lock_file = match File::create(LOCK_FILE_PATH) {
        Ok(f) => f,
        Err(e) => {
            log_syslog(&format!("Failed to open lock file {}: {}", LOCK_FILE_PATH, e), true);
            return;
        }
    };

    if let Err(e) = flock(&lock_file, FlockOperation::LockExclusive) {
        log_syslog(&format!("Failed to acquire lock: {}", e), true);
        return;
    }

    // Resolve active desktop user
    let user = match resolve_target_user() {
        Ok(u) => u,
        Err(e) => {
            log_syslog(&format!("User resolution error: {}", e), true);
            let _ = flock(&lock_file, FlockOperation::Unlock);
            return;
        }
    };

    // Load configuration from user's home directory
    let config_path = PathBuf::from(format!("/home/{}/.config/snac/snac.config", user.username));
    let config = match load_config(&config_path) {
        Ok(cfg) => cfg,
        Err(e) => {
            log_syslog(&e, true);
            send_notification(&user, &format!("SNAC Config Error: {}", e), "critical");
            let _ = flock(&lock_file, FlockOperation::Unlock);
            return;
        }
    };

    // Verify binary exists
    if !Path::new(&config.binary_path).exists() {
        let err = format!("Binary not found at specified path: {}", config.binary_path);
        log_syslog(&err, true);
        send_notification(&user, &err, "critical");
        let _ = flock(&lock_file, FlockOperation::Unlock);
        return;
    }

    // Perform system-wide identity check. On failure (e.g. nmcli unreachable), skip
    // reconciliation entirely rather than guessing — neither starting nor stopping is safe
    // without a real answer.
    let (matched, match_reason) = match is_target_active_systemwide(&config) {
        Ok(r) => r,
        Err(e) => {
            log_syslog(&format!("Network status check failed, skipping this event: {}", e), true);
            let _ = flock(&lock_file, FlockOperation::Unlock);
            return;
        }
    };
    let service_active = is_service_active(&user);

    log_syslog(
        &format!(
            "Event: '{}' on interface '{}' | System-wide match: {} ({}) | Service active: {}",
            action, interface, matched, match_reason, service_active
        ),
        false,
    );

    if matched && !service_active {
        log_syslog("Starting snac.service...", false);
        match run_systemctl_user(&user, "start") {
            Ok(_) => send_notification(&user, "Sophos auto-connected.", "normal"),
            Err(e) => {
                log_syslog(&format!("Start failed: {}", e), true);
                send_notification(&user, &format!("Failed to start Sophos: {}", e), "critical");
            }
        }
    } else if !matched && service_active {
        log_syslog("Stopping snac.service...", false);
        match run_systemctl_user(&user, "stop") {
            Ok(_) => send_notification(&user, "Sophos disconnected.", "normal"),
            Err(e) => log_syslog(&format!("Stop failed: {}", e), true),
        }
    }

    let _ = flock(&lock_file, FlockOperation::Unlock);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_value_parsing() {
        let case1 = "TARGET_SSID=\"NFSU_students\"";
        let case2 = "TARGET_SSID=\"NFSU_students\" # college wifi";
        let case3 = "TARGET_SSID=NFSU_students # college wifi";
        let case4 = "TARGET_SSID=NFSU_students";

        assert_eq!(parse_config_value(case1.split_once('=').unwrap().1).unwrap(), "NFSU_students");
        assert_eq!(parse_config_value(case2.split_once('=').unwrap().1).unwrap(), "NFSU_students");
        assert_eq!(parse_config_value(case3.split_once('=').unwrap().1).unwrap(), "NFSU_students");
        assert_eq!(parse_config_value(case4.split_once('=').unwrap().1).unwrap(), "NFSU_students");
    }

    #[test]
    fn test_config_value_bare_hash_not_treated_as_comment() {
        // A '#' with no preceding whitespace is part of the value, not a comment marker.
        let case = "TARGET_SSID=Net#5";
        assert_eq!(parse_config_value(case.split_once('=').unwrap().1).unwrap(), "Net#5");
    }

    #[test]
    fn test_config_value_unterminated_quote_is_error() {
        let case = "BINARY_PATH=\"/usr/bin/caa";
        assert!(parse_config_value(case.split_once('=').unwrap().1).is_err());
    }

    #[test]
    fn test_parse_nmcli_terse_line_unescapes_colons() {
        let fields = parse_nmcli_terse_line(r"Office\:LAN:eth0:802-3-ethernet");
        assert_eq!(fields, vec!["Office:LAN", "eth0", "802-3-ethernet"]);
    }
}
