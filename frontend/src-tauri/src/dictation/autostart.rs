//! macOS login launch via LaunchAgent with `--hidden` (tray-first).
//! SMAppService is linked for Login Items compatibility helpers on macOS 13+.

use anyhow::{anyhow, Result};
use std::path::PathBuf;

const LAUNCH_AGENT_LABEL: &str = "com.meetily.ai.login";

/// Register or unregister login launch (tray-first via `--hidden`).
pub fn set_launch_at_login(enabled: bool) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        macos::set_enabled(enabled)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = enabled;
        Err(anyhow!("Launch at login is only supported on macOS"))
    }
}

pub fn is_launch_at_login_enabled() -> bool {
    #[cfg(target_os = "macos")]
    {
        macos::is_enabled()
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}

/// True when the process was started with `--hidden` (login / tray launch).
pub fn launched_hidden() -> bool {
    std::env::args().any(|a| a == "--hidden" || a == "--from-login")
}

/// True when launched for a one-shot product dictation latency self-test.
pub fn wants_dictation_self_test() -> bool {
    std::env::args().any(|a| a == "--dictation-self-test")
}

#[cfg(target_os = "macos")]
mod macos {
    use super::*;
    use std::fs;
    use std::process::Command;

    // Keep ServiceManagement linked for future SMAppService Login Items UI.
    #[link(name = "ServiceManagement", kind = "framework")]
    extern "C" {}

    pub fn set_enabled(enabled: bool) -> Result<()> {
        if enabled {
            install_launch_agent()?;
            log::info!("Dictation launch-at-login enabled (LaunchAgent with --hidden)");
        } else {
            remove_launch_agent()?;
            log::info!("Dictation launch-at-login disabled");
        }
        Ok(())
    }

    pub fn is_enabled() -> bool {
        agent_plist_path().map(|p| p.exists()).unwrap_or(false)
    }

    fn agent_plist_path() -> Result<PathBuf> {
        let home = dirs::home_dir().ok_or_else(|| anyhow!("no home dir"))?;
        Ok(home
            .join("Library/LaunchAgents")
            .join(format!("{LAUNCH_AGENT_LABEL}.plist")))
    }

    fn app_executable() -> Result<PathBuf> {
        std::env::current_exe().map_err(|e| anyhow!("current_exe: {e}"))
    }

    fn install_launch_agent() -> Result<()> {
        let exe = app_executable()?;
        let plist_path = agent_plist_path()?;
        if let Some(parent) = plist_path.parent() {
            fs::create_dir_all(parent).map_err(|e| anyhow!("LaunchAgents dir: {e}"))?;
        }

        let exe_str = exe.to_string_lossy();
        let plist = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{LAUNCH_AGENT_LABEL}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{exe_str}</string>
    <string>--hidden</string>
  </array>
  <key>RunAtLoad</key>
  <true/>
  <key>LimitLoadToSessionType</key>
  <string>Aqua</string>
</dict>
</plist>
"#
        );
        fs::write(&plist_path, plist).map_err(|e| anyhow!("write LaunchAgent: {e}"))?;

        if let Some(uid) = current_uid() {
            let domain = format!("gui/{uid}");
            let _ = Command::new("launchctl")
                .args(["bootout", &domain, &plist_path.to_string_lossy()])
                .output();
            let status = Command::new("launchctl")
                .args(["bootstrap", &domain, &plist_path.to_string_lossy()])
                .status();
            if matches!(status, Ok(s) if s.success()) {
                return Ok(());
            }
        }
        let _ = Command::new("launchctl")
            .args(["load", "-w", &plist_path.to_string_lossy()])
            .status();
        Ok(())
    }

    fn remove_launch_agent() -> Result<()> {
        let plist_path = agent_plist_path()?;
        if plist_path.exists() {
            if let Some(uid) = current_uid() {
                let _ = Command::new("launchctl")
                    .args([
                        "bootout",
                        &format!("gui/{uid}"),
                        &plist_path.to_string_lossy(),
                    ])
                    .output();
            }
            let _ = Command::new("launchctl")
                .args(["unload", "-w", &plist_path.to_string_lossy()])
                .output();
            fs::remove_file(&plist_path).map_err(|e| anyhow!("remove LaunchAgent: {e}"))?;
        }
        Ok(())
    }

    fn current_uid() -> Option<u32> {
        let output = Command::new("id").arg("-u").output().ok()?;
        if !output.status.success() {
            return None;
        }
        String::from_utf8(output.stdout).ok()?.trim().parse().ok()
    }
}
