//! Running as, and managing, the "OpenChroma" Windows service.
//!
//! The service runs `openchromad.exe` from `%ProgramFiles%\OpenChroma` as
//! LocalSystem, starts at boot, and is restarted by Windows if it fails.
//! Installing copies the binaries there rather than running them from a build
//! folder: a SYSTEM service must not execute files a normal user can replace.

use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use windows_service::service::{
    ServiceAccess, ServiceAction, ServiceActionType, ServiceControl, ServiceControlAccept, ServiceErrorControl, ServiceExitCode,
    ServiceFailureActions, ServiceFailureResetPeriod, ServiceInfo, ServiceStartType, ServiceState, ServiceStatus, ServiceType,
};
use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};
use windows_service::{define_windows_service, service_dispatcher};

use crate::install;

pub const NAME: &str = "OpenChroma";
const DISPLAY_NAME: &str = "OpenChroma Lighting Service";
const DESCRIPTION: &str = "Drives Razer Chroma devices and serves the Chroma SDK to games.";

/// Files the service install copies from the CLI's folder.
const FILES: [&str; 4] = ["openchromad.exe", "openchroma.exe", install::DLL64, install::DLL32];

/// Win32 error when the process was not started by the service manager.
const ERROR_FAILED_SERVICE_CONTROLLER_CONNECT: i32 = 1063;
const ERROR_SERVICE_DOES_NOT_EXIST: i32 = 1060;
const ERROR_ACCESS_DENIED: i32 = 5;

// ---------------------------------------------------------------------------
// Running under the service manager
// ---------------------------------------------------------------------------

define_windows_service!(ffi_service_main, service_main);

/// Entry point for `openchromad.exe`: run as a service when started by the
/// service manager, otherwise run directly (e.g. double-clicked).
pub fn run() -> Result<(), String> {
    match service_dispatcher::start(NAME, ffi_service_main) {
        Ok(()) => Ok(()),
        Err(windows_service::Error::Winapi(e)) if e.raw_os_error() == Some(ERROR_FAILED_SERVICE_CONTROLLER_CONNECT) => crate::run_service(),
        Err(e) => Err(e.to_string()),
    }
}

enum Event {
    Stop,
    Failed(String),
}

fn service_main(_args: Vec<OsString>) {
    let (tx, rx) = mpsc::channel();
    let stop_tx = tx.clone();
    let handler = move |control| match control {
        ServiceControl::Stop | ServiceControl::Shutdown => {
            let _ = stop_tx.send(Event::Stop);
            ServiceControlHandlerResult::NoError
        }
        ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
        _ => ServiceControlHandlerResult::NotImplemented,
    };
    let Ok(status) = service_control_handler::register(NAME, handler) else { return };
    let report = |state, exit_code| {
        let _ = status.set_service_status(ServiceStatus {
            service_type: ServiceType::OWN_PROCESS,
            current_state: state,
            controls_accepted: if state == ServiceState::Running {
                ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN
            } else {
                ServiceControlAccept::empty()
            },
            exit_code,
            checkpoint: 0,
            wait_hint: Duration::from_secs(5),
            process_id: None,
        });
    };

    // `run_service` only returns if startup fails; otherwise it runs until
    // the process exits.
    thread::spawn(move || {
        if let Err(e) = crate::run_service() {
            let _ = tx.send(Event::Failed(e));
        }
    });
    report(ServiceState::Running, ServiceExitCode::Win32(0));

    let exit = match rx.recv() {
        Ok(Event::Failed(e)) => {
            log::error!("service failed to start: {e}");
            // A non-zero exit triggers the configured restart.
            ServiceExitCode::ServiceSpecific(1)
        }
        _ => {
            log::info!("service stopping");
            ServiceExitCode::Win32(0)
        }
    };
    report(ServiceState::Stopped, exit);
    // Devices keep their last frame; nothing else needs tearing down.
    std::process::exit(0);
}

// ---------------------------------------------------------------------------
// Managing the service
// ---------------------------------------------------------------------------

pub fn install_dir() -> PathBuf {
    std::env::var_os("ProgramFiles").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\Program Files")).join("OpenChroma")
}

fn winapi_code(e: &windows_service::Error) -> Option<i32> {
    match e {
        windows_service::Error::Winapi(io) => io.raw_os_error(),
        _ => None,
    }
}

fn describe(e: windows_service::Error) -> String {
    match winapi_code(&e) {
        Some(ERROR_ACCESS_DENIED) => "access denied; run this from an elevated (administrator) terminal".into(),
        Some(ERROR_SERVICE_DOES_NOT_EXIST) => {
            "the OpenChroma service is not installed (run `openchroma service install` as administrator)".into()
        }
        _ => e.to_string(),
    }
}

fn wait_for(service: &windows_service::service::Service, state: ServiceState, timeout: Duration) -> Result<(), String> {
    let start = Instant::now();
    loop {
        let current = service.query_status().map_err(describe)?.current_state;
        if current == state {
            return Ok(());
        }
        if start.elapsed() > timeout {
            return Err(format!("service is {current:?}, expected {state:?} after {}s", timeout.as_secs()));
        }
        thread::sleep(Duration::from_millis(200));
    }
}

/// Stop OpenChroma processes outside the service (a console `openchroma run`
/// or the old sign-in autostart); they hold the devices and the SDK pipe.
fn stop_user_instances() {
    let me = std::process::id().to_string();
    let _ = Command::new("taskkill").args(["/F", "/IM", "openchromad.exe"]).output();
    let _ = Command::new("taskkill").args(["/F", "/FI", "IMAGENAME eq openchroma.exe", "/FI", &format!("PID ne {me}")]).output();
}

fn copy_files(from: &Path, to: &Path) -> io::Result<()> {
    fs::create_dir_all(to)?;
    for name in FILES {
        let (src, dst) = (from.join(name), to.join(name));
        if !src.exists() {
            return Err(io::Error::new(io::ErrorKind::NotFound, format!("{} not found", src.display())));
        }
        if src.canonicalize()? == dst.canonicalize().unwrap_or_default() {
            continue;
        }
        fs::copy(&src, &dst).map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", dst.display())))?;
    }
    Ok(())
}

/// Install or update the service. Must run elevated.
pub fn install() -> Result<Vec<String>, String> {
    let mut done = Vec::new();
    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE)
        .map_err(describe)?;
    let access = ServiceAccess::QUERY_STATUS | ServiceAccess::START | ServiceAccess::STOP | ServiceAccess::CHANGE_CONFIG;

    // Stop the old version so its files can be replaced.
    let existing = match manager.open_service(NAME, access) {
        Ok(s) => Some(s),
        Err(e) if winapi_code(&e) == Some(ERROR_SERVICE_DOES_NOT_EXIST) => None,
        Err(e) => return Err(describe(e)),
    };
    if let Some(s) = &existing {
        if s.query_status().map_err(describe)?.current_state != ServiceState::Stopped {
            s.stop().map_err(describe)?;
            wait_for(s, ServiceState::Stopped, Duration::from_secs(15))?;
            done.push("stopped the running service".into());
        }
    }
    stop_user_instances();

    let source = std::env::current_exe().map_err(|e| e.to_string())?.parent().map(Path::to_path_buf).unwrap_or_default();
    let target = install_dir();
    copy_files(&source, &target).map_err(|e| e.to_string())?;
    done.push(format!("copied files to {}", target.display()));

    // Create the shared config now, as the user, so a first install can
    // still migrate the old per-user config and import Synapse's layout.
    crate::config::Config::load_or_create();
    done.push(format!("config: {}", crate::config::Config::path().display()));

    let info = ServiceInfo {
        name: NAME.into(),
        display_name: DISPLAY_NAME.into(),
        service_type: ServiceType::OWN_PROCESS,
        start_type: ServiceStartType::AutoStart,
        error_control: ServiceErrorControl::Normal,
        executable_path: target.join("openchromad.exe"),
        launch_arguments: vec![],
        dependencies: vec![],
        account_name: None, // LocalSystem
        account_password: None,
    };
    let service = match existing {
        Some(s) => {
            s.change_config(&info).map_err(describe)?;
            done.push("updated the service".into());
            s
        }
        None => {
            let s = manager.create_service(&info, access).map_err(describe)?;
            done.push(format!("created the \"{NAME}\" service"));
            s
        }
    };
    service.set_description(DESCRIPTION).map_err(describe)?;
    let restart = |secs| ServiceAction { action_type: ServiceActionType::Restart, delay: Duration::from_secs(secs) };
    service
        .update_failure_actions(ServiceFailureActions {
            reset_period: ServiceFailureResetPeriod::After(Duration::from_secs(24 * 3600)),
            reboot_msg: None,
            command: None,
            actions: Some(vec![restart(2), restart(5), restart(30)]),
        })
        .map_err(describe)?;
    service.set_failure_actions_on_non_crash_failures(true).map_err(describe)?;

    // Default service permissions plus start/stop for signed-in users, so
    // the CLI, build script and a future desktop app can control it without
    // elevation.
    let sddl = "D:(A;;CCLCSWRPWPDTLOCRRC;;;SY)(A;;CCDCLCSWRPWPDTLOCRSDRCWDWO;;;BA)(A;;CCLCSWRPWPLOCRRC;;;IU)(A;;CCLCSWLOCRRC;;;SU)";
    let sd = Command::new("sc").args(["sdset", NAME, sddl]).output().map_err(|e| e.to_string())?;
    if !sd.status.success() {
        done.push(format!("warning: could not let users start/stop the service: {}", String::from_utf8_lossy(&sd.stdout).trim()));
    }

    // The service replaces sign-in autostart.
    if install::set_autostart(false).is_ok() {
        done.push("removed the old sign-in autostart entry".into());
    }

    // Games need the SDK DLL; put ours in place unless Razer's is there.
    for line in install::install_system_if_free(&target).map_err(|e| e.to_string())? {
        done.push(line);
    }

    service.start(&[] as &[&str]).map_err(describe)?;
    wait_for(&service, ServiceState::Running, Duration::from_secs(15))?;
    done.push("service is running".into());
    Ok(done)
}

pub fn uninstall() -> Result<Vec<String>, String> {
    let mut done = Vec::new();
    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT).map_err(describe)?;
    let service =
        manager.open_service(NAME, ServiceAccess::QUERY_STATUS | ServiceAccess::STOP | ServiceAccess::DELETE).map_err(describe)?;
    if service.query_status().map_err(describe)?.current_state != ServiceState::Stopped {
        service.stop().map_err(describe)?;
        wait_for(&service, ServiceState::Stopped, Duration::from_secs(15))?;
        done.push("stopped the service".into());
    }
    service.delete().map_err(describe)?;
    done.push(format!("removed the \"{NAME}\" service"));

    let dir = install_dir();
    for name in FILES {
        // The CLI doing the uninstall may itself live here; skip what is in use.
        if fs::remove_file(dir.join(name)).is_err() && dir.join(name).exists() {
            done.push(format!("left {} (in use)", dir.join(name).display()));
        }
    }
    let _ = fs::remove_dir(&dir);
    done.push(format!(
        "kept {} and any SDK DLLs in System32 (`openchroma sdk uninstall` removes those)",
        crate::config::Config::dir().display()
    ));
    Ok(done)
}

fn control(start: bool) -> Result<String, String> {
    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT).map_err(describe)?;
    let access = ServiceAccess::QUERY_STATUS | if start { ServiceAccess::START } else { ServiceAccess::STOP };
    let service = manager.open_service(NAME, access).map_err(describe)?;
    let state = service.query_status().map_err(describe)?.current_state;
    if start {
        if state != ServiceState::Running {
            service.start(&[] as &[&str]).map_err(describe)?;
            wait_for(&service, ServiceState::Running, Duration::from_secs(15))?;
        }
        Ok("service is running".into())
    } else {
        if state != ServiceState::Stopped {
            service.stop().map_err(describe)?;
            wait_for(&service, ServiceState::Stopped, Duration::from_secs(15))?;
        }
        Ok("service is stopped".into())
    }
}

pub fn start() -> Result<String, String> {
    control(true)
}

pub fn stop() -> Result<String, String> {
    control(false)
}

/// `None` if the service is not installed.
pub fn state() -> Result<Option<ServiceState>, String> {
    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT).map_err(describe)?;
    match manager.open_service(NAME, ServiceAccess::QUERY_STATUS) {
        Ok(s) => Ok(Some(s.query_status().map_err(describe)?.current_state)),
        Err(e) if winapi_code(&e) == Some(ERROR_SERVICE_DOES_NOT_EXIST) => Ok(None),
        Err(e) => Err(describe(e)),
    }
}
