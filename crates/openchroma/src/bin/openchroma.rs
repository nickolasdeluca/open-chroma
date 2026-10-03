//! Command-line front end.

use std::path::Path;
use std::process::ExitCode;

use openchroma::config::UI_PORT;
use openchroma::install;
use serde_json::{json, Value};

const USAGE: &str = "\
OpenChroma - open Razer Chroma lighting service

usage: openchroma <command>

  service install          install/update the Windows service (admin)
  service uninstall        remove the Windows service (admin)
  service start|stop       start or stop the service
  run                      run in this console instead of as a service
  devices                  probe supported devices directly (read-only)
  status                   show what the running service sees
  profile <name>           switch the active lighting profile
  brightness <0-100>       set global brightness
  sdk on|off               allow or block games from taking over the lights
  sdk status               show which RzChromaSDK DLLs are installed
  sdk install [<game dir>] install the SDK DLL system-wide (admin) or into one game
  sdk uninstall [<dir>]    restore Razer's DLL (system-wide) or a game's original
  autostart on|off         start at sign-in instead of as a service
  ui                       open the web UI

The web UI is at http://127.0.0.1:54240/ while the service runs.";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let result = match args.as_slice() {
        ["run"] => openchroma::run_service(),
        ["service", "install"] => openchroma::service::install().map(|lines| lines.iter().for_each(|l| println!("{l}"))),
        ["service", "uninstall"] => openchroma::service::uninstall().map(|lines| lines.iter().for_each(|l| println!("{l}"))),
        ["service", "start"] => openchroma::service::start().map(|m| println!("{m}")),
        ["service", "stop"] => openchroma::service::stop().map(|m| println!("{m}")),
        ["devices"] => devices(),
        ["status"] => status(),
        ["profile", name] => settings(json!({"active_profile": name})),
        ["brightness", v] => match v.parse::<u8>() {
            Ok(v) if v <= 100 => settings(json!({"brightness": v})),
            _ => Err("brightness must be 0-100".into()),
        },
        ["sdk", "on"] => settings(json!({"sdk_enabled": true})),
        ["sdk", "off"] => settings(json!({"sdk_enabled": false})),
        ["sdk", "status"] => {
            install::system_status().iter().for_each(|l| println!("{l}"));
            Ok(())
        }
        ["sdk", "install"] => report(install::install_system()),
        ["sdk", "install", dir] => report(install::install_dir(Path::new(dir))),
        ["sdk", "uninstall"] => report(install::uninstall_system()),
        ["sdk", "uninstall", dir] => report(install::uninstall_dir(Path::new(dir))),
        ["autostart", "on"] => install::set_autostart(true).map(|m| println!("{m}")).map_err(|e| e.to_string()),
        ["autostart", "off"] => install::set_autostart(false).map(|m| println!("{m}")).map_err(|e| e.to_string()),
        ["ui"] => std::process::Command::new("cmd")
            .args(["/C", "start", "", &format!("http://127.0.0.1:{UI_PORT}/")])
            .status()
            .map(|_| ())
            .map_err(|e| e.to_string()),
        _ => {
            println!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn report(r: std::io::Result<Vec<String>>) -> Result<(), String> {
    let lines = r.map_err(|e| e.to_string())?;
    if lines.is_empty() {
        println!("nothing to do");
    }
    lines.iter().for_each(|l| println!("{l}"));
    Ok(())
}

fn devices() -> Result<(), String> {
    let api = hidapi::HidApi::new().map_err(|e| e.to_string())?;
    let found = razer_hid::enumerate(&api);
    if found.is_empty() {
        println!("no supported Razer devices found");
    }
    for info in found {
        let spec = info.spec;
        match razer_hid::Device::open(&api, info) {
            Ok(dev) => println!("{:04X}  {:<42} firmware {}", spec.pid, spec.name, dev.firmware().unwrap_or_else(|e| format!("? ({e})"))),
            Err(e) => println!("{:04X}  {:<42} cannot open: {e}", spec.pid, spec.name),
        }
    }
    Ok(())
}

/// Tiny HTTP/1.0 client for the local control API.
fn request(method: &str, path: &str, body: Option<&Value>) -> Result<Value, String> {
    openchroma::client::request("cli", method, path, body).map_err(|e| match e {
        openchroma::client::Error::Offline => not_running(),
        e => e.to_string(),
    })
}

fn not_running() -> String {
    match openchroma::service::state() {
        Ok(Some(state)) => format!("the OpenChroma service is {state:?}; start it with `openchroma service start`"),
        _ => "OpenChroma is not running; install the service with `openchroma service install` (as administrator)".into(),
    }
}

fn settings(v: Value) -> Result<(), String> {
    request("POST", "/api/settings", Some(&v)).map(|_| println!("ok"))
}

fn status() -> Result<(), String> {
    let s = request("GET", "/api/status", None)?;
    let service = match openchroma::service::state() {
        Ok(Some(state)) => format!("{state:?}"),
        Ok(None) => "not installed (running standalone)".into(),
        Err(e) => e,
    };
    println!("service:     {service}");
    println!("profile:     {} (brightness {}%)", s["active_profile"].as_str().unwrap_or("?"), s["brightness"]);
    println!(
        "chroma sdk:  games {}; REST API {}",
        if s["sdk_enabled"].as_bool() == Some(true) { "allowed" } else { "blocked" },
        if s["sdk_port_bound"].as_bool() == Some(true) {
            format!("listening on {}", s["sdk_port"])
        } else {
            format!("waiting for port {} (held by Razer's service?)", s["sdk_port"])
        }
    );
    println!("devices:");
    for d in s["devices"].as_array().into_iter().flatten() {
        let state = if d["connected"].as_bool() == Some(true) {
            format!("ok, firmware {}", d["firmware"].as_str().unwrap_or("?"))
        } else {
            format!("error: {}", d["error"].as_str().unwrap_or("not connected"))
        };
        println!("  {:<42} {state}", d["name"].as_str().unwrap_or("?"));
    }
    let sessions = s["sessions"].as_array().cloned().unwrap_or_default();
    if sessions.is_empty() {
        println!("apps:        none");
    }
    for a in sessions {
        println!(
            "app:         {} [{}]{}",
            a["title"].as_str().unwrap_or("?"),
            a["client"]["kind"].as_str().unwrap_or("?"),
            if a["active"].as_bool() == Some(true) { " <- controlling lights" } else { "" }
        );
    }
    Ok(())
}
