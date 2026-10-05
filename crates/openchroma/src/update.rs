//! Updating OpenChroma from its GitHub releases.
//!
//! The service looks up the latest release only when a client asks (the app
//! polls while it is open), and installs only when a user asks: updating on
//! its own would restart the lights and the app under the user.
//!
//! Installing downloads the release package into the install folder under
//! Program Files, which only administrators and the service can write,
//! checks it against the release's `SHA256SUMS.txt`, and runs that package's
//! `openchroma service install`, the same step the installer script runs. It
//! stops this service, replaces the files and starts the new version.
//! Windows' own `curl.exe` and `tar.exe` do the downloading and unpacking, so
//! no TLS stack is linked in.

use std::fs;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use windows_sys::Win32::System::Threading::{CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW};

use crate::engine::lock;

/// CI builds take the repository they were built from, so a fork updates
/// from its own releases.
const REPO: &str = match option_env!("GITHUB_REPOSITORY") {
    Some(repo) => repo,
    None => "nickolasdeluca/open-chroma",
};
const CURRENT: &str = env!("CARGO_PKG_VERSION");
const CHECK_EVERY: Duration = Duration::from_secs(6 * 3600);
const RETRY_AFTER: Duration = Duration::from_secs(15 * 60);
/// How long the installer may run before we stop waiting for it. A
/// successful install stops this service long before that.
const INSTALL_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Clone, Copy, PartialEq)]
enum Phase {
    Idle,
    Checking,
    Downloading,
    Installing,
}

#[derive(Clone)]
struct Release {
    version: String,
    page: String,
    zip_name: String,
    zip_url: String,
    sums_url: String,
}

struct Updater {
    phase: Phase,
    latest: Option<Release>,
    checked: Option<Instant>,
    checked_at: Option<u64>,
    /// Why the last lookup failed.
    check_error: Option<String>,
    /// Why the last install failed.
    error: Option<String>,
}

static STATE: Mutex<Updater> =
    Mutex::new(Updater { phase: Phase::Idle, latest: None, checked: None, checked_at: None, check_error: None, error: None });

/// `0.3.1` or `v0.3.1` as comparable numbers; anything after `-` or `+` is
/// ignored.
fn parse_version(s: &str) -> Option<(u64, u64, u64)> {
    let core = s.trim().trim_start_matches('v').split(['-', '+']).next()?;
    let mut parts = core.split('.').map(|p| p.parse::<u64>().ok());
    let v = (parts.next()??, parts.next()??, parts.next()??);
    parts.next().is_none().then_some(v)
}

fn is_newer(latest: &str) -> bool {
    matches!((parse_version(latest), parse_version(CURRENT)), (Some(l), Some(c)) if l > c)
}

/// Update state for `GET /api/update`. Starts a lookup in the background
/// when the last one is old enough.
pub fn status() -> Value {
    let mut s = lock(&STATE);
    let due = match s.checked {
        None => true,
        Some(t) => t.elapsed() >= if s.check_error.is_some() { RETRY_AFTER } else { CHECK_EVERY },
    };
    if due && s.phase == Phase::Idle {
        s.phase = Phase::Checking;
        thread::spawn(check);
    }
    let latest = s.latest.as_ref();
    json!({
        "current": CURRENT,
        "latest": latest.map(|r| &r.version),
        "available": latest.is_some_and(|r| is_newer(&r.version)),
        "url": latest.map(|r| &r.page),
        "state": match s.phase {
            Phase::Idle => "idle",
            Phase::Checking => "checking",
            Phase::Downloading => "downloading",
            Phase::Installing => "installing",
        },
        "checked_at": s.checked_at,
        "check_error": s.check_error,
        "error": s.error,
    })
}

fn check() {
    let result = latest_release();
    let mut s = lock(&STATE);
    s.phase = Phase::Idle;
    s.checked = Some(Instant::now());
    s.checked_at = SystemTime::now().duration_since(UNIX_EPOCH).ok().map(|d| d.as_secs());
    match result {
        Ok(release) => {
            if is_newer(&release.version) && s.latest.as_ref().map(|r| &r.version) != Some(&release.version) {
                log::info!("OpenChroma {} is available (running {CURRENT})", release.version);
            }
            s.latest = Some(release);
            s.check_error = None;
        }
        Err(e) => {
            log::warn!("update check failed: {e}");
            s.check_error = Some(e);
        }
    }
}

/// Start installing the latest release, for `POST /api/update`.
pub fn start() -> Result<(), String> {
    let mut s = lock(&STATE);
    if s.phase != Phase::Idle {
        return Err("an update check or install is already running".into());
    }
    let Some(release) = s.latest.clone().filter(|r| is_newer(&r.version)) else {
        return Err(format!("no newer release than {CURRENT} is known"));
    };
    s.phase = Phase::Downloading;
    s.error = None;
    thread::spawn(move || {
        let result = install(&release);
        let mut s = lock(&STATE);
        s.phase = Phase::Idle;
        if let Err(e) = result {
            log::error!("updating to {} failed: {e}", release.version);
            s.error = Some(format!("updating to {} failed: {e}", release.version));
        }
    });
    Ok(())
}

fn system32(exe: &str) -> PathBuf {
    let root = std::env::var_os("SystemRoot").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
    root.join("System32").join(exe)
}

/// Run a Windows tool without a console window; its stdout on success.
fn run(exe: &str, args: &[&str]) -> Result<Vec<u8>, String> {
    let out = Command::new(system32(exe))
        .args(args)
        .stdin(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| format!("{exe}: {e}"))?;
    if out.status.success() {
        Ok(out.stdout)
    } else {
        Err(format!("{exe}: {}", String::from_utf8_lossy(&out.stderr).trim()))
    }
}

/// HTTPS only, redirects included: release assets redirect to GitHub's CDN.
fn download(url: &str, to: Option<&Path>) -> Result<Vec<u8>, String> {
    let agent = format!("openchroma/{CURRENT}");
    let mut args = vec!["--fail", "--silent", "--show-error", "--location", "--proto", "=https", "--proto-redir", "=https"];
    args.extend(["--max-time", "600", "--user-agent", &agent, "--header", "Accept: application/vnd.github+json"]);
    let to = to.map(|p| p.display().to_string());
    if let Some(to) = &to {
        args.extend(["--output", to]);
    }
    args.push(url);
    run("curl.exe", &args)
}

fn latest_release() -> Result<Release, String> {
    let body = download(&format!("https://api.github.com/repos/{REPO}/releases/latest"), None)?;
    let v: Value = serde_json::from_slice(&body).map_err(|e| format!("unexpected answer from GitHub: {e}"))?;
    let tag = v["tag_name"].as_str().ok_or("the latest release has no tag")?;
    let version = tag.trim_start_matches('v').to_string();
    if parse_version(&version).is_none() {
        return Err(format!("the latest release has an unexpected tag {tag:?}"));
    }
    let assets = v["assets"].as_array().map(Vec::as_slice).unwrap_or_default();
    let asset = |pick: &dyn Fn(&str) -> bool| {
        assets.iter().find_map(|a| {
            let name = a["name"].as_str().filter(|n| pick(n))?;
            Some((name.to_string(), a["browser_download_url"].as_str()?.to_string()))
        })
    };
    let (zip_name, zip_url) = asset(&|n| n.starts_with("OpenChroma-") && n.ends_with("-windows-x64.zip"))
        .ok_or_else(|| format!("{tag} has no Windows package"))?;
    let (_, sums_url) = asset(&|n| n == "SHA256SUMS.txt").ok_or_else(|| format!("{tag} has no checksums"))?;
    let page = v["html_url"].as_str().unwrap_or_default().to_string();
    Ok(Release { version, page, zip_name, zip_url, sums_url })
}

fn sha256(data: &[u8]) -> Result<[u8; 32], String> {
    use windows_sys::Win32::Security::Cryptography::{BCryptHash, BCRYPT_SHA256_ALG_HANDLE};
    let len = u32::try_from(data.len()).map_err(|_| "file too large to hash")?;
    let mut out = [0u8; 32];
    let status = unsafe { BCryptHash(BCRYPT_SHA256_ALG_HANDLE, std::ptr::null(), 0, data.as_ptr(), len, out.as_mut_ptr(), 32) };
    if status != 0 {
        return Err(format!("hashing failed (NTSTATUS {status:#x})"));
    }
    Ok(out)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The hash `SHA256SUMS.txt` lists for `name`.
fn expected_sum(sums: &str, name: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let (hash, file) = line.trim().split_once(char::is_whitespace)?;
        (file.trim().trim_start_matches('*') == name).then(|| hash.to_ascii_lowercase())
    })
}

/// Where downloads are unpacked; inside the install folder, so a normal user
/// can't swap the files before the installer runs them as SYSTEM.
fn update_dir() -> PathBuf {
    crate::service::install_dir().join("update")
}

fn install(release: &Release) -> Result<(), String> {
    let dir = update_dir();
    let _ = fs::remove_dir_all(&dir);
    let files = dir.join("files");
    fs::create_dir_all(&files).map_err(|e| format!("{}: {e}", files.display()))?;

    log::info!("downloading {}", release.zip_name);
    let zip = dir.join(&release.zip_name);
    download(&release.zip_url, Some(&zip))?;
    let sums = String::from_utf8_lossy(&download(&release.sums_url, None)?).into_owned();
    let expected = expected_sum(&sums, &release.zip_name).ok_or("the checksums don't list the package")?;
    let actual = hex(&sha256(&fs::read(&zip).map_err(|e| e.to_string())?)?);
    if actual != expected {
        return Err(format!("checksum mismatch for {}; not installing", release.zip_name));
    }

    let (zip_arg, files_arg) = (zip.display().to_string(), files.display().to_string());
    run("tar.exe", &["-xf", &zip_arg, "-C", &files_arg])?;
    let installer = files.join("openchroma.exe");
    if !installer.exists() {
        return Err("the package has no openchroma.exe".into());
    }
    // `service install` leaves this to the installer script, which isn't run
    // here.
    let _ = fs::copy(files.join("razer-services.ps1"), crate::service::install_dir().join("razer-services.ps1"));

    lock(&STATE).phase = Phase::Installing;
    log::info!("installing OpenChroma {}; the service will restart", release.version);
    // Its own process group, so it outlives this service when it stops it.
    let log_path = dir.join("install.log");
    let log_file = fs::File::create(&log_path).map_err(|e| e.to_string())?;
    let mut child = Command::new(&installer)
        .args(["service", "install"])
        .current_dir(&files)
        .stdin(Stdio::null())
        .stdout(log_file.try_clone().map_err(|e| e.to_string())?)
        .stderr(log_file)
        .creation_flags(CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP)
        .spawn()
        .map_err(|e| format!("starting the installer: {e}"))?;

    // Reaching the end of this means the installer gave up before stopping
    // us.
    let started = Instant::now();
    while started.elapsed() < INSTALL_TIMEOUT {
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
            let output = fs::read_to_string(&log_path).unwrap_or_default();
            return Err(format!("the installer exited with {status}: {}", output.trim()));
        }
        thread::sleep(Duration::from_millis(500));
    }
    Err("the installer did not finish in time".into())
}

/// Log what the last update's installer printed and delete its files. Runs
/// once the new service is up; the installer may still be finishing, so wait
/// a little first.
pub fn clean_up_later() {
    thread::spawn(|| {
        thread::sleep(Duration::from_secs(60));
        let dir = update_dir();
        if let Ok(output) = fs::read_to_string(dir.join("install.log")) {
            log::info!("last update's installer said:\n{}", output.trim());
        }
        let _ = fs::remove_dir_all(&dir);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert_eq!(parse_version("v0.3.1"), Some((0, 3, 1)));
        assert_eq!(parse_version("1.10.0-rc.1"), Some((1, 10, 0)));
        assert_eq!(parse_version("1.2"), None);
        assert_eq!(parse_version("1.2.3.4"), None);
        assert!(parse_version("0.10.0") > parse_version("0.9.9"));
    }

    #[test]
    fn checksums() {
        assert_eq!(hex(&sha256(b"abc").unwrap()), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        let sums = "AA11  install.ps1\r\nbb22  OpenChroma-0.3.1-windows-x64.zip\r\n";
        assert_eq!(expected_sum(sums, "OpenChroma-0.3.1-windows-x64.zip").as_deref(), Some("bb22"));
        assert_eq!(expected_sum(sums, "OpenChroma-0.3.0-windows-x64.zip"), None);
    }

    /// Talks to GitHub: `cargo test -p openchroma -- --ignored`.
    #[test]
    #[ignore]
    fn finds_the_latest_release() {
        let r = latest_release().unwrap();
        assert!(parse_version(&r.version).is_some());
        assert!(r.zip_url.ends_with(&r.zip_name));
        let sums = String::from_utf8(download(&r.sums_url, None).unwrap()).unwrap();
        assert!(expected_sum(&sums, &r.zip_name).is_some());
    }
}
