//! Login-time autostart, ported from upstream's `update_autostart`: a Windows
//! registry Run entry, a freedesktop `.desktop` file, or a launchd plist.
//!
//! The entry starts our own executable with the caller's arguments (`run` for the
//! CLI, `--tray` for the GUI) and `RUSTPICK_DIR` baked in, since our data
//! directory is resolved from the environment/current directory.

use crate::settings::{data_dir, profile};
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// `base`, suffixed with the profile name so each profile gets its own entry.
fn entry_name(base: &str) -> String {
    match profile() {
        Some(p) => format!("{base}-{p}"),
        None => base.to_owned(),
    }
}

/// The command the autostart entry runs: (exe path, absolute data dir).
fn entry_paths() -> Result<(PathBuf, PathBuf)> {
    let exe = std::env::current_exe().context("resolving our own executable path")?;
    let dir = data_dir();
    let dir = std::path::absolute(&dir).unwrap_or(dir);
    Ok((exe, dir))
}

#[cfg(windows)]
mod imp {
    use super::*;
    use std::os::windows::process::CommandExt;

    const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    fn reg(args: &[&str]) -> Result<std::process::Output> {
        std::process::Command::new("reg")
            .args(args)
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .context("running reg.exe")
    }

    pub fn enabled() -> bool {
        let Ok((exe, dir)) = entry_paths() else { return false };
        let Ok(out) = reg(&["query", RUN_KEY, "/v", &entry_name("Rustpick")]) else { return false };
        let stdout = String::from_utf8_lossy(&out.stdout);
        out.status.success()
            && stdout.contains(exe.to_string_lossy().as_ref())
            && stdout.contains(dir.to_string_lossy().as_ref())
    }

    pub fn set(on: bool, args: &[&str]) -> Result<()> {
        if on {
            let (exe, dir) = entry_paths()?;
            // cmd strips the outer quotes, leaving: set "RUSTPICK_DIR=..." && "exe" args
            let data = format!(
                r#"cmd /c "set "RUSTPICK_DIR={}" && "{}" {}""#,
                dir.display(),
                exe.display(),
                args.join(" ")
            );
            reg(&["add", RUN_KEY, "/v", &entry_name("Rustpick"), "/t", "REG_SZ", "/d", &data, "/f"])?;
        } else {
            let _ = reg(&["delete", RUN_KEY, "/v", &entry_name("Rustpick"), "/f"]); // missing is fine
        }
        Ok(())
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use super::*;

    fn entry_path() -> PathBuf {
        // like upstream, honour XDG_CONFIG_HOME when it already has an autostart dir
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|p| p.join("autostart").is_dir())
            .map(|p| p.join("autostart"))
            .unwrap_or_else(|| home().join(".config/autostart"))
            .join(format!("{}.desktop", entry_name("rustpick")))
    }

    pub(super) fn entry_contents(exe: &Path, dir: &Path, args: &[&str]) -> String {
        format!(
            "[Desktop Entry]\nType=Application\nName=Rustpick\nDescription=Mine timed drops on Twitch\nExec=sh -c 'RUSTPICK_DIR=\"{}\" \"{}\" {}'\n",
            dir.display(),
            exe.display(),
            args.join(" ")
        )
    }

    pub fn enabled() -> bool {
        let path = entry_path();
        let Ok((exe, dir)) = entry_paths() else { return false };
        path.is_file()
            && std::fs::read_to_string(path).is_ok_and(|c| {
                c.contains(exe.to_string_lossy().as_ref()) && c.contains(dir.to_string_lossy().as_ref())
            })
    }

    pub fn set(on: bool, args: &[&str]) -> Result<()> {
        let path = entry_path();
        if on {
            let (exe, dir) = entry_paths()?;
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("creating {}", parent.display()))?;
            }
            std::fs::write(&path, entry_contents(&exe, &dir, args))
                .with_context(|| format!("writing {}", path.display()))?;
        } else {
            std::fs::remove_file(&path).ok(); // missing is fine
        }
        Ok(())
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use super::*;


    fn entry_path() -> PathBuf {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default()
            .join(format!("Library/LaunchAgents/{}.plist", entry_name("com.rustpick.rustpick")))
    }

    fn xml_escape(s: &str) -> String {
        s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
    }

    pub(super) fn entry_contents(exe: &Path, dir: &Path, args: &[&str]) -> String {
        let args: String = args.iter().map(|a| format!("  <string>{}</string>\n", xml_escape(a))).collect();
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
             <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
             <plist version=\"1.0\">\n\
             <dict>\n\
             \x20<key>EnvironmentVariables</key>\n\
             \x20<dict>\n\
             \x20\x20<key>RUSTPICK_DIR</key>\n\
             \x20\x20<string>{}</string>\n\
             \x20</dict>\n\
             \x20<key>Label</key>\n\
             \x20<string>{}</string>\n\
             \x20<key>ProgramArguments</key>\n\
             \x20<array>\n\
             \x20\x20<string>{}</string>\n\
             {args}\
             \x20</array>\n\
             \x20<key>RunAtLoad</key>\n\
             \x20<true/>\n\
             </dict>\n\
             </plist>\n",
            xml_escape(&dir.to_string_lossy()),
            xml_escape(&entry_name("com.rustpick.rustpick")),
            xml_escape(&exe.to_string_lossy()),
        )
    }

    pub fn enabled() -> bool {
        let path = entry_path();
        let Ok((exe, dir)) = entry_paths() else { return false };
        path.is_file()
            && std::fs::read_to_string(path).is_ok_and(|c| {
                c.contains(exe.to_string_lossy().as_ref()) && c.contains(dir.to_string_lossy().as_ref())
            })
    }

    pub fn set(on: bool, args: &[&str]) -> Result<()> {
        let path = entry_path();
        if on {
            let (exe, dir) = entry_paths()?;
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("creating {}", parent.display()))?;
            }
            std::fs::write(&path, entry_contents(&exe, &dir, args))
                .with_context(|| format!("writing {}", path.display()))?;
        } else {
            std::fs::remove_file(&path).ok(); // missing is fine
        }
        Ok(())
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
mod imp {
    use super::*;

    pub fn enabled() -> bool {
        false
    }

    pub fn set(_on: bool, _args: &[&str]) -> Result<()> {
        anyhow::bail!("autostart is not supported on this platform")
    }
}

/// Is the autostart entry present and pointing at this install?
pub fn enabled() -> bool {
    imp::enabled()
}

/// Create or remove the autostart entry; `args` follow the executable. The active
/// `--profile` is appended so the started instance shows the same profile.
pub fn set(on: bool, args: &[&str]) -> Result<()> {
    let mut args = args.to_vec();
    if let Some(p) = profile() {
        args.extend(["--profile", p]);
    }
    imp::set(on, &args)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_contents_carry_env_and_run() {
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        return;
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            let (exe, dir) = entry_paths().unwrap();
            let contents = imp::entry_contents(&exe, &dir, &["run", "--tray"]);
            assert!(contents.contains("RUSTPICK_DIR"));
            assert!(contents.contains("run"));
            if cfg!(target_os = "linux") {
                assert!(contents.starts_with("[Desktop Entry]"));
                assert!(contents.contains("Exec=sh -c 'RUSTPICK_DIR="));
                assert!(contents.contains("\" run --tray'"));
            }
            if cfg!(target_os = "macos") {
                assert!(contents.contains("<key>RunAtLoad</key>"));
                assert!(contents.contains("<string>run</string>"));
                assert!(contents.contains("<string>--tray</string>"));
            }
        }
    }
}
