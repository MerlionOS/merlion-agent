//! Background-service management for `merlion gateway`.
//!
//! On macOS we install a `LaunchAgent` plist under
//! `~/Library/LaunchAgents/ai.merlion.gateway.plist` and drive it with
//! `launchctl`. On Linux we install a `--user` systemd unit at
//! `~/.config/systemd/user/merlion-gateway.service` and drive it with
//! `systemctl --user`.
//!
//! The service runs `<current_exe> gateway run` in the foreground, with
//! stdout/stderr redirected to `~/.merlion/logs/gateway.{log,error.log}`.
//! Tokens come from `~/.merlion/.env`, which `merlion-config` loads at
//! startup — launchd's empty inherited env doesn't matter here.

use anyhow::{anyhow, Context, Result};
use std::path::{Path, PathBuf};
use std::process::{Command as StdCommand, Output};

#[cfg(target_os = "macos")]
pub const SERVICE_LABEL: &str = "ai.merlion.gateway";
#[cfg(target_os = "macos")]
const PLIST_NAME: &str = "ai.merlion.gateway.plist";
#[cfg(target_os = "linux")]
const UNIT_NAME: &str = "merlion-gateway.service";

#[derive(Debug, Clone)]
pub enum ServiceState {
    NotInstalled,
    Stopped {
        unit_path: PathBuf,
    },
    Running {
        unit_path: PathBuf,
        pid: Option<u32>,
        last_exit: Option<i32>,
    },
    Unknown {
        unit_path: PathBuf,
        detail: String,
    },
}

fn merlion_exe() -> Result<PathBuf> {
    std::env::current_exe().context("resolving current executable path")
}

fn log_paths() -> (PathBuf, PathBuf) {
    let logs = merlion_config::merlion_home().join("logs");
    (logs.join("gateway.log"), logs.join("gateway.error.log"))
}

#[cfg(target_os = "macos")]
fn unit_path() -> Result<PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| anyhow!("no home directory"))?;
    Ok(home.join("Library/LaunchAgents").join(PLIST_NAME))
}

#[cfg(target_os = "linux")]
fn unit_path() -> Result<PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| anyhow!("no home directory"))?;
    Ok(home.join(".config/systemd/user").join(UNIT_NAME))
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn unit_path() -> Result<PathBuf> {
    Err(anyhow!(
        "service management is only supported on macOS (launchd) and Linux (systemd --user); \
         use `merlion gateway run` to run in the foreground"
    ))
}

pub fn install() -> Result<()> {
    let exe = merlion_exe()?;
    let path = unit_path()?;
    let (stdout, stderr) = log_paths();
    // pre-create the log directory so the daemon doesn't fail to redirect
    if let Some(parent) = stdout.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating log dir {}", parent.display()))?;
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating unit dir {}", parent.display()))?;
    }

    #[cfg(target_os = "macos")]
    let body = render_plist(&exe, &stdout, &stderr);
    #[cfg(target_os = "linux")]
    let body = render_systemd_unit(&exe, &stdout, &stderr);
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    let body: String = {
        let _ = (&exe, &stdout, &stderr);
        return Err(anyhow!("unsupported platform"));
    };

    std::fs::write(&path, body)
        .with_context(|| format!("writing service definition to {}", path.display()))?;
    println!("wrote {}", path.display());

    #[cfg(target_os = "macos")]
    {
        // bootout first in case a stale registration exists, then bootstrap.
        let _ = launchctl_bootout();
        launchctl_bootstrap(&path)?;
        println!("loaded service {SERVICE_LABEL}");
    }
    #[cfg(target_os = "linux")]
    {
        systemctl(&["daemon-reload"])?;
        systemctl(&["enable", "--now", UNIT_NAME])?;
        println!("enabled and started {UNIT_NAME}");
    }
    Ok(())
}

pub fn uninstall() -> Result<()> {
    let path = unit_path()?;

    #[cfg(target_os = "macos")]
    {
        let _ = launchctl_bootout();
    }
    #[cfg(target_os = "linux")]
    {
        let _ = systemctl(&["disable", "--now", UNIT_NAME]);
    }

    if path.exists() {
        std::fs::remove_file(&path).with_context(|| format!("removing {}", path.display()))?;
        println!("removed {}", path.display());
    } else {
        println!(
            "no service definition at {} (already uninstalled?)",
            path.display()
        );
    }

    #[cfg(target_os = "linux")]
    {
        let _ = systemctl(&["daemon-reload"]);
    }
    Ok(())
}

pub fn start() -> Result<()> {
    let path = unit_path()?;
    if !path.exists() {
        return Err(anyhow!(
            "no service installed at {}. Run `merlion gateway install` first, \
             or `merlion gateway run` to run in the foreground.",
            path.display()
        ));
    }
    #[cfg(target_os = "macos")]
    {
        launchctl_kickstart()?;
        println!("started {SERVICE_LABEL}");
    }
    #[cfg(target_os = "linux")]
    {
        systemctl(&["start", UNIT_NAME])?;
        println!("started {UNIT_NAME}");
    }
    Ok(())
}

pub fn stop() -> Result<()> {
    let path = unit_path()?;
    if !path.exists() {
        return Err(anyhow!("no service installed at {}.", path.display()));
    }
    #[cfg(target_os = "macos")]
    {
        // `launchctl kill SIGTERM <target>` exits 3 ("No process to signal")
        // when the daemon isn't currently running — treat that as a no-op so
        // `merlion gateway stop` is idempotent.
        match launchctl_kill("SIGTERM") {
            Ok(()) => println!("stopped {SERVICE_LABEL}"),
            Err(e) if e.to_string().contains("No process to signal") => {
                println!("{SERVICE_LABEL} is already stopped");
            }
            Err(e) => return Err(e),
        }
    }
    #[cfg(target_os = "linux")]
    {
        systemctl(&["stop", UNIT_NAME])?;
        println!("stopped {UNIT_NAME}");
    }
    Ok(())
}

pub fn restart() -> Result<()> {
    let path = unit_path()?;
    if !path.exists() {
        return Err(anyhow!("no service installed at {}.", path.display()));
    }
    #[cfg(target_os = "macos")]
    {
        launchctl_kickstart_restart()?;
        println!("restarted {SERVICE_LABEL}");
    }
    #[cfg(target_os = "linux")]
    {
        systemctl(&["restart", UNIT_NAME])?;
        println!("restarted {UNIT_NAME}");
    }
    Ok(())
}

pub fn service_status() -> Result<ServiceState> {
    let path = match unit_path() {
        Ok(p) => p,
        Err(_) => return Ok(ServiceState::NotInstalled),
    };
    if !path.exists() {
        return Ok(ServiceState::NotInstalled);
    }

    #[cfg(target_os = "macos")]
    {
        let out = run_capture("launchctl", &["print", &launchctl_target()]);
        match out {
            Ok(o) if o.status.success() => {
                let text = String::from_utf8_lossy(&o.stdout);
                let pid = parse_field(&text, "pid =").and_then(|s| s.parse::<u32>().ok());
                let last_exit =
                    parse_field(&text, "last exit code =").and_then(|s| s.parse::<i32>().ok());
                if pid.is_some() {
                    Ok(ServiceState::Running {
                        unit_path: path,
                        pid,
                        last_exit,
                    })
                } else {
                    Ok(ServiceState::Stopped { unit_path: path })
                }
            }
            Ok(o) => {
                // Service not loaded (bootout'd) but plist exists on disk.
                let detail = format!(
                    "launchctl print exited {}: {}",
                    o.status,
                    String::from_utf8_lossy(&o.stderr).trim()
                );
                let _ = detail;
                Ok(ServiceState::Stopped { unit_path: path })
            }
            Err(e) => Ok(ServiceState::Unknown {
                unit_path: path,
                detail: format!("launchctl print failed: {e}"),
            }),
        }
    }
    #[cfg(target_os = "linux")]
    {
        let out = run_capture("systemctl", &["--user", "show", UNIT_NAME, "--no-page"]);
        match out {
            Ok(o) if o.status.success() => {
                let text = String::from_utf8_lossy(&o.stdout);
                let active = parse_kv(&text, "ActiveState").unwrap_or_default();
                let pid = parse_kv(&text, "MainPID")
                    .and_then(|s| s.parse::<u32>().ok())
                    .filter(|&p| p != 0);
                let last_exit =
                    parse_kv(&text, "ExecMainStatus").and_then(|s| s.parse::<i32>().ok());
                if active == "active" || active == "activating" {
                    Ok(ServiceState::Running {
                        unit_path: path,
                        pid,
                        last_exit,
                    })
                } else {
                    Ok(ServiceState::Stopped { unit_path: path })
                }
            }
            Ok(o) => Ok(ServiceState::Unknown {
                unit_path: path,
                detail: format!(
                    "systemctl show exited {}: {}",
                    o.status,
                    String::from_utf8_lossy(&o.stderr).trim()
                ),
            }),
            Err(e) => Ok(ServiceState::Unknown {
                unit_path: path,
                detail: format!("systemctl show failed: {e}"),
            }),
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        Ok(ServiceState::NotInstalled)
    }
}

#[cfg(target_os = "macos")]
fn launchctl_target() -> String {
    // gui/<uid>/<label> is the modern domain-target for per-user agents.
    format!("gui/{}/{SERVICE_LABEL}", users_uid())
}

#[cfg(target_os = "macos")]
fn users_uid() -> u32 {
    // Shell out to `id -u`. We could pull in libc::getuid() but adding a
    // crate dep for a one-line lookup is overkill; this is invoked at most
    // a handful of times per CLI invocation.
    match run_capture("id", &["-u"]) {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout)
            .trim()
            .parse::<u32>()
            .unwrap_or(501),
        _ => 501,
    }
}

#[cfg(target_os = "macos")]
fn launchctl_bootstrap(plist: &Path) -> Result<()> {
    let target = format!("gui/{}", users_uid());
    run_checked(
        "launchctl",
        &[
            "bootstrap",
            &target,
            plist
                .to_str()
                .ok_or_else(|| anyhow!("non-utf8 plist path"))?,
        ],
    )
}

#[cfg(target_os = "macos")]
fn launchctl_bootout() -> Result<()> {
    run_checked("launchctl", &["bootout", &launchctl_target()])
}

#[cfg(target_os = "macos")]
fn launchctl_kickstart() -> Result<()> {
    run_checked("launchctl", &["kickstart", &launchctl_target()])
}

#[cfg(target_os = "macos")]
fn launchctl_kickstart_restart() -> Result<()> {
    run_checked("launchctl", &["kickstart", "-k", &launchctl_target()])
}

#[cfg(target_os = "macos")]
fn launchctl_kill(signal: &str) -> Result<()> {
    run_checked("launchctl", &["kill", signal, &launchctl_target()])
}

#[cfg(target_os = "linux")]
fn systemctl(args: &[&str]) -> Result<()> {
    let mut full = vec!["--user"];
    full.extend_from_slice(args);
    run_checked("systemctl", &full)
}

fn run_checked(program: &str, args: &[&str]) -> Result<()> {
    let out = StdCommand::new(program)
        .args(args)
        .output()
        .with_context(|| format!("spawning {program}"))?;
    if !out.status.success() {
        return Err(anyhow!(
            "{program} {} failed ({}): {}",
            args.join(" "),
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(())
}

fn run_capture(program: &str, args: &[&str]) -> Result<Output> {
    StdCommand::new(program)
        .args(args)
        .output()
        .with_context(|| format!("spawning {program}"))
}

/// Extract `field <value>` from a `launchctl print` block. Returns the trimmed
/// value with surrounding `"` and `;` stripped.
#[cfg(target_os = "macos")]
fn parse_field(haystack: &str, prefix: &str) -> Option<String> {
    for line in haystack.lines() {
        if let Some(rest) = line.trim().strip_prefix(prefix) {
            let v = rest.trim().trim_end_matches(';').trim();
            let v = v.trim_matches('"');
            if !v.is_empty() {
                return Some(v.to_string());
            }
        }
    }
    None
}

/// Extract `Key=Value` from `systemctl show` output.
#[cfg(target_os = "linux")]
fn parse_kv(haystack: &str, key: &str) -> Option<String> {
    let prefix = format!("{key}=");
    for line in haystack.lines() {
        if let Some(v) = line.strip_prefix(&prefix) {
            return Some(v.trim().to_string());
        }
    }
    None
}

#[cfg(target_os = "macos")]
fn render_plist(exe: &Path, stdout_log: &Path, stderr_log: &Path) -> String {
    // Hand-rolled XML — pulling in a plist crate just for this would be
    // overkill. Path components do not need XML-escaping for the typical
    // characters present in $HOME, and we control the rest verbatim.
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{label}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{exe}</string>
        <string>gateway</string>
        <string>run</string>
    </array>
    <key>EnvironmentVariables</key>
    <dict>
        <key>PATH</key>
        <string>/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin</string>
    </dict>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>ProcessType</key>
    <string>Background</string>
    <key>StandardOutPath</key>
    <string>{stdout}</string>
    <key>StandardErrorPath</key>
    <string>{stderr}</string>
    <key>LimitLoadToSessionType</key>
    <string>Aqua</string>
</dict>
</plist>
"#,
        label = SERVICE_LABEL,
        exe = exe.display(),
        stdout = stdout_log.display(),
        stderr = stderr_log.display(),
    )
}

#[cfg(target_os = "linux")]
fn render_systemd_unit(exe: &Path, stdout_log: &Path, stderr_log: &Path) -> String {
    format!(
        "[Unit]\n\
         Description=Merlion Agent gateway daemon\n\
         After=network-online.target\n\
         Wants=network-online.target\n\
         \n\
         [Service]\n\
         Type=simple\n\
         ExecStart={exe} gateway run\n\
         Restart=on-failure\n\
         RestartSec=5\n\
         StandardOutput=append:{stdout}\n\
         StandardError=append:{stderr}\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n",
        exe = exe.display(),
        stdout = stdout_log.display(),
        stderr = stderr_log.display(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "macos")]
    #[test]
    fn parse_field_handles_launchctl_format() {
        let sample = r#"{
            "Label" = "ai.merlion.gateway";
            "PID" = 1234;
            pid = 1234;
            "LastExitStatus" = 0;
            last exit code = 0;
        };"#;
        assert_eq!(parse_field(sample, "pid ="), Some("1234".to_string()));
        assert_eq!(
            parse_field(sample, "last exit code ="),
            Some("0".to_string())
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn parse_field_missing_returns_none() {
        assert_eq!(parse_field("no match here", "pid ="), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn parse_kv_handles_systemctl_show() {
        let sample = "ActiveState=active\nMainPID=4242\nExecMainStatus=0\n";
        assert_eq!(parse_kv(sample, "ActiveState"), Some("active".into()));
        assert_eq!(parse_kv(sample, "MainPID"), Some("4242".into()));
        assert_eq!(parse_kv(sample, "ExecMainStatus"), Some("0".into()));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn render_plist_contains_required_keys() {
        let body = render_plist(
            Path::new("/usr/local/bin/merlion"),
            Path::new("/tmp/gateway.log"),
            Path::new("/tmp/gateway.error.log"),
        );
        assert!(body.contains("ai.merlion.gateway"));
        assert!(body.contains("<string>gateway</string>"));
        assert!(body.contains("<string>run</string>"));
        assert!(body.contains("/usr/local/bin/merlion"));
        assert!(body.contains("/tmp/gateway.log"));
        assert!(body.contains("/tmp/gateway.error.log"));
        assert!(body.contains("KeepAlive"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn render_systemd_unit_contains_required_keys() {
        let body = render_systemd_unit(
            Path::new("/usr/local/bin/merlion"),
            Path::new("/tmp/gateway.log"),
            Path::new("/tmp/gateway.error.log"),
        );
        assert!(body.contains("ExecStart=/usr/local/bin/merlion gateway run"));
        assert!(body.contains("StandardOutput=append:/tmp/gateway.log"));
        assert!(body.contains("StandardError=append:/tmp/gateway.error.log"));
        assert!(body.contains("Restart=on-failure"));
        assert!(body.contains("[Install]"));
    }
}
