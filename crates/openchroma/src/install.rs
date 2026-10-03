//! Installing the RzChromaSDK DLL replacement and autostart entry.
//!
//! Games load `RzChromaSDK64.dll` (64-bit) or `RzChromaSDK.dll` (32-bit) by
//! name, which Windows resolves from the game's own folder first and then
//! from System32/SysWOW64. OpenChroma's DLLs can go in either place. Razer's
//! originals are backed up before being replaced and can be restored.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

pub const DLL64: &str = "RzChromaSDK64.dll";
pub const DLL32: &str = "RzChromaSDK.dll";

/// Bytes present only in OpenChroma's DLL (its sender thread name).
const MARKER: &[u8] = b"openchroma-sdk";

fn windir() -> PathBuf {
    std::env::var_os("SystemRoot").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\Windows"))
}

fn backup_dir() -> PathBuf {
    std::env::var_os("ProgramData").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\ProgramData")).join(r"OpenChroma\backup")
}

/// System locations for each DLL, as seen from a 64-bit process.
fn system_targets() -> [(&'static str, PathBuf); 2] {
    [(DLL64, windir().join("System32").join(DLL64)), (DLL32, windir().join("SysWOW64").join(DLL32))]
}

/// OpenChroma's own DLLs ship next to the executable.
fn our_dll(name: &str) -> io::Result<PathBuf> {
    let dir = std::env::current_exe()?.parent().map(Path::to_path_buf).unwrap_or_default();
    let p = dir.join(name);
    if p.exists() {
        Ok(p)
    } else {
        Err(io::Error::new(io::ErrorKind::NotFound, format!("{} not found next to openchroma.exe", name)))
    }
}

pub fn is_ours(path: &Path) -> bool {
    fs::read(path).map(|b| b.windows(MARKER.len()).any(|w| w == MARKER)).unwrap_or(false)
}

fn explain(e: io::Error, path: &Path) -> io::Error {
    let hint = match e.kind() {
        io::ErrorKind::PermissionDenied => {
            " (run from an elevated terminal; if it still fails, close games and stop Razer's services, which may have the DLL open)"
        }
        _ => "",
    };
    io::Error::new(e.kind(), format!("{}: {e}{hint}", path.display()))
}

pub fn install_system() -> io::Result<Vec<String>> {
    let mut done = Vec::new();
    fs::create_dir_all(backup_dir()).map_err(|e| explain(e, &backup_dir()))?;
    for (name, target) in system_targets() {
        let ours = match our_dll(name) {
            Ok(p) => p,
            Err(e) => {
                done.push(format!("skipped {name}: {e}"));
                continue;
            }
        };
        if target.exists() && !is_ours(&target) {
            let backup = backup_dir().join(name);
            if !backup.exists() {
                fs::copy(&target, &backup).map_err(|e| explain(e, &backup))?;
                done.push(format!("backed up {} to {}", target.display(), backup.display()));
            }
        }
        fs::copy(&ours, &target).map_err(|e| explain(e, &target))?;
        done.push(format!("installed {}", target.display()));
    }
    Ok(done)
}

/// Install or update OpenChroma's DLLs from `dir` into the system folders,
/// but leave Razer's DLLs alone; replacing those stays an explicit
/// `sdk install`.
pub fn install_system_if_free(dir: &Path) -> io::Result<Vec<String>> {
    let mut done = Vec::new();
    for (name, target) in system_targets() {
        if target.exists() && !is_ours(&target) {
            done.push(format!("left Razer's {} in place (`openchroma sdk install` replaces it)", target.display()));
            continue;
        }
        let ours = dir.join(name);
        fs::copy(&ours, &target).map_err(|e| explain(e, &target))?;
        done.push(format!("installed {}", target.display()));
    }
    Ok(done)
}

pub fn uninstall_system() -> io::Result<Vec<String>> {
    let mut done = Vec::new();
    for (name, target) in system_targets() {
        let backup = backup_dir().join(name);
        if backup.exists() {
            fs::copy(&backup, &target).map_err(|e| explain(e, &target))?;
            done.push(format!("restored Razer's {}", target.display()));
        } else if is_ours(&target) {
            fs::remove_file(&target).map_err(|e| explain(e, &target))?;
            done.push(format!("removed {} (no Razer backup existed)", target.display()));
        }
    }
    Ok(done)
}

/// Put both DLLs in a game's folder; existing ones are kept as `.orig`.
pub fn install_dir(dir: &Path) -> io::Result<Vec<String>> {
    let mut done = Vec::new();
    for name in [DLL64, DLL32] {
        let ours = our_dll(name)?;
        let target = dir.join(name);
        if target.exists() && !is_ours(&target) {
            let orig = dir.join(format!("{name}.orig"));
            fs::rename(&target, &orig).map_err(|e| explain(e, &target))?;
            done.push(format!("kept original as {}", orig.display()));
        }
        fs::copy(&ours, &target).map_err(|e| explain(e, &target))?;
        done.push(format!("installed {}", target.display()));
    }
    Ok(done)
}

pub fn uninstall_dir(dir: &Path) -> io::Result<Vec<String>> {
    let mut done = Vec::new();
    for name in [DLL64, DLL32] {
        let target = dir.join(name);
        if is_ours(&target) {
            fs::remove_file(&target).map_err(|e| explain(e, &target))?;
            done.push(format!("removed {}", target.display()));
        }
        let orig = dir.join(format!("{name}.orig"));
        if orig.exists() {
            fs::rename(&orig, &target).map_err(|e| explain(e, &orig))?;
            done.push(format!("restored {}", target.display()));
        }
    }
    Ok(done)
}

/// Who provides each system DLL: "openchroma", "razer" or "missing".
pub fn system_dll_states() -> Vec<(&'static str, &'static str)> {
    system_targets()
        .into_iter()
        .map(|(name, target)| {
            let state = if !target.exists() {
                "missing"
            } else if is_ours(&target) {
                "openchroma"
            } else {
                "razer"
            };
            (name, state)
        })
        .collect()
}

pub fn system_status() -> Vec<String> {
    system_targets()
        .into_iter()
        .zip(system_dll_states())
        .map(|((_, target), (_, state))| {
            let label = match state {
                "openchroma" => "OpenChroma",
                "razer" => "Razer (original)",
                _ => "missing",
            };
            format!("{}: {label}", target.display())
        })
        .collect()
}

const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";

pub fn set_autostart(enable: bool) -> io::Result<String> {
    let status = if enable {
        let exe = std::env::current_exe()?.with_file_name("openchromad.exe");
        if !exe.exists() {
            return Err(io::Error::new(io::ErrorKind::NotFound, format!("{} not found", exe.display())));
        }
        Command::new("reg")
            .args(["add", RUN_KEY, "/v", "OpenChroma", "/t", "REG_SZ", "/d", &format!("\"{}\"", exe.display()), "/f"])
            .output()?
    } else {
        Command::new("reg").args(["delete", RUN_KEY, "/v", "OpenChroma", "/f"]).output()?
    };
    if status.status.success() {
        Ok(if enable { "OpenChroma will start when you sign in".into() } else { "autostart removed".into() })
    } else {
        Err(io::Error::other(String::from_utf8_lossy(&status.stderr).trim().to_string()))
    }
}
