//! "Start when I log in" for a normal PC (server 0.9), without a VPS:
//! - Linux: a systemd user socket + service. systemd holds the ports, so the server starts on the
//!   first connection (also at login) and may stop itself when idle; the next connection starts it again.
//! - macOS: a LaunchAgent that starts it at login.
//! - Windows: an entry under HKCU\...\Run that starts it at login.

use std::path::{Path, PathBuf};

#[cfg(all(unix, not(target_os = "macos")))]
fn unit_dir() -> Result<PathBuf, String> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .ok_or("No home folder.")?;
    Ok(base.join("systemd/user"))
}

fn run(cmd: &str, args: &[&str]) -> Result<(), String> {
    let s = std::process::Command::new(cmd).args(args).status().map_err(|e| format!("{cmd}: {e}"))?;
    if s.success() { Ok(()) } else { Err(format!("{cmd} {} failed", args.join(" "))) }
}

/// systemd quotes: arguments with spaces in double quotes.
#[cfg(all(unix, not(target_os = "macos")))]
fn quote(p: &Path) -> String {
    format!("\"{}\"", p.to_string_lossy().replace('\\', "\\\\").replace('"', "\\\""))
}

#[cfg(all(unix, not(target_os = "macos")))]
pub fn unit_files(exe: &Path, data: &Path, sync: &str, admin: &str) -> (String, String) {
    let socket = format!(
        "# Made by `egd-server autostart on`. Remove with `egd-server autostart off`.\n\
         [Unit]\nDescription=Evelopment Games Designer Server (starts on the first connection)\n\n\
         [Socket]\n# Order matters: first the sync port, then the admin page.\nListenStream={sync}\nListenStream={admin}\n\n\
         [Install]\nWantedBy=sockets.target\n"
    );
    let service = format!(
        "# Made by `egd-server autostart on`. Remove with `egd-server autostart off`.\n\
         [Unit]\nDescription=Evelopment Games Designer Server\nRequires=egd-server.socket\nAfter=network-online.target\n\n\
         [Service]\nExecStart={} --data {} --no-browser --headless\n\
         # Saving and the last git sync get time before the PC shuts down.\nTimeoutStopSec=150\nRestart=on-failure\n",
        quote(exe),
        quote(data)
    );
    (socket, service)
}

pub fn turn_on(data: &Path, sync: &str, admin: &str) -> Result<String, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let dir = unit_dir()?;
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let (socket, service) = unit_files(&exe, data, sync, admin);
        std::fs::write(dir.join("egd-server.socket"), socket).map_err(|e| e.to_string())?;
        std::fs::write(dir.join("egd-server.service"), service).map_err(|e| e.to_string())?;
        run("systemctl", &["--user", "daemon-reload"])?;
        run("systemctl", &["--user", "enable", "--now", "egd-server.socket"])?;
        Ok(format!(
            "On. systemd now holds {sync} and {admin} and starts the server on the first connection.\n\
             Stop this copy of the server first if it is still running, so the ports are free.\n\
             User services stop when you log out; to keep it reachable then: loginctl enable-linger $USER"
        ))
    }
    #[cfg(target_os = "macos")]
    {
        let _ = (sync, admin);
        let plist = plist_path()?;
        std::fs::create_dir_all(plist.parent().unwrap()).map_err(|e| e.to_string())?;
        let esc = |p: &Path| p.to_string_lossy().replace('&', "&amp;").replace('<', "&lt;");
        std::fs::write(
            &plist,
            format!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
                 <plist version=\"1.0\"><dict>\n<key>Label</key><string>{LABEL}</string>\n\
                 <key>ProgramArguments</key><array><string>{}</string><string>--data</string><string>{}</string><string>--no-browser</string></array>\n\
                 <key>RunAtLoad</key><true/>\n</dict></plist>\n",
                esc(&exe),
                esc(data)
            ),
        )
        .map_err(|e| e.to_string())?;
        Ok("On. The server starts when you log in.".into())
    }
    #[cfg(windows)]
    {
        let _ = (sync, admin);
        let value = format!("\"{}\" --data \"{}\" --no-browser", exe.display(), data.display());
        run("reg", &["add", RUN_KEY, "/v", "EGDServer", "/t", "REG_SZ", "/d", &value, "/f"])?;
        Ok("On. The server starts when you log in.".into())
    }
}

pub fn turn_off() -> Result<String, String> {
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let dir = unit_dir()?;
        let _ = run("systemctl", &["--user", "disable", "--now", "egd-server.socket", "egd-server.service"]);
        let _ = std::fs::remove_file(dir.join("egd-server.socket"));
        let _ = std::fs::remove_file(dir.join("egd-server.service"));
        let _ = run("systemctl", &["--user", "daemon-reload"]);
    }
    #[cfg(target_os = "macos")]
    {
        let _ = std::fs::remove_file(plist_path()?);
    }
    #[cfg(windows)]
    {
        let _ = run("reg", &["delete", RUN_KEY, "/v", "EGDServer", "/f"]);
    }
    Ok("Off. The server no longer starts by itself.".into())
}

pub fn is_on() -> bool {
    #[cfg(all(unix, not(target_os = "macos")))]
    return unit_dir().is_ok_and(|d| d.join("egd-server.socket").exists());
    #[cfg(target_os = "macos")]
    return plist_path().is_ok_and(|p| p.exists());
    #[cfg(windows)]
    return std::process::Command::new("reg").args(["query", RUN_KEY, "/v", "EGDServer"]).output().is_ok_and(|o| o.status.success());
}

#[cfg(target_os = "macos")]
const LABEL: &str = "games.evelopment.egd-server";
#[cfg(target_os = "macos")]
fn plist_path() -> Result<PathBuf, String> {
    let home = std::env::var_os("HOME").ok_or("No home folder.")?;
    Ok(PathBuf::from(home).join("Library/LaunchAgents").join(format!("{LABEL}.plist")))
}
#[cfg(windows)]
const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";

#[cfg(all(test, unix, not(target_os = "macos")))]
mod tests {
    #[test]
    fn unit_files_quote_paths_and_list_both_ports() {
        let (socket, service) =
            super::unit_files("/opt/my apps/egd-server".as_ref(), "/home/a/egd data".as_ref(), "0.0.0.0:8474", "127.0.0.1:8475");
        assert!(socket.contains("ListenStream=0.0.0.0:8474\nListenStream=127.0.0.1:8475\n"));
        assert!(service.contains("ExecStart=\"/opt/my apps/egd-server\" --data \"/home/a/egd data\" --no-browser --headless\n"));
    }
}
