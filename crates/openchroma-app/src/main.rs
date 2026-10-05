//! OpenChroma desktop app: a native window (Slint, software-rendered, so it
//! needs neither WebView2 nor a GPU driver) that manages lighting through the
//! service's local control API.
#![windows_subsystem = "windows"]

mod state;
mod views;

use std::cell::RefCell;
use std::sync::atomic::{AtomicPtr, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Duration;

use openchroma::client::{self, Error};
use openchroma::config::Config;
use serde_json::{json, Value};
use slint::{ComponentHandle, Weak};

use state::State;
use views::Views;

slint::include_modules!();

const POLL_INTERVAL: Duration = Duration::from_millis(120);
/// The config changes rarely; fetch it less often than status.
const CONFIG_EVERY: u32 = 5;
/// Update state changes even more rarely (about every 3 s).
const UPDATE_EVERY: u32 = 25;

/// Requests to the service, sent from a worker so the UI never blocks.
enum Command {
    Config(Box<Config>),
    AllowApp(String, bool),
    Identify(Option<String>),
    Update,
}

/// Data from the poller to the UI thread.
enum Update {
    Online { status: Value, config: Option<Config>, update: Option<Value> },
    Offline(String),
}

struct App {
    ui: Weak<AppWindow>,
    state: State,
    views: Views,
    status: Value,
    writer: Sender<Command>,
    /// The service version an update the user started is replacing.
    updating_from: Option<String>,
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

/// Run `f` against the app state on the UI thread.
fn with_app<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    APP.with(|a| a.borrow_mut().as_mut().map(f))
}

impl App {
    fn refresh(&mut self) {
        if let Some(ui) = self.ui.upgrade() {
            self.views.refresh(&ui, &self.state, &self.status);
        }
    }

    /// Send an edited config (if the edit changed anything) and redraw.
    fn commit(&mut self, config: Option<Config>) {
        if let Some(c) = config {
            let _ = self.writer.send(Command::Config(Box::new(c)));
        }
        self.refresh();
    }
}

fn toast(ui: &Weak<AppWindow>, message: String) {
    let ui = ui.clone();
    let _ = slint::invoke_from_event_loop(move || {
        if let Some(w) = ui.upgrade() {
            w.set_toast(message.into());
            let ui = w.as_weak();
            slint::Timer::single_shot(Duration::from_secs(3), move || {
                if let Some(w) = ui.upgrade() {
                    w.set_toast("".into());
                }
            });
        }
    });
}

/// Sends commands to the service. Bursts (a dragged slider) are coalesced so
/// only the newest config is written.
fn writer(rx: Receiver<Command>, ui: Weak<AppWindow>) {
    while let Ok(first) = rx.recv() {
        thread::sleep(Duration::from_millis(60));
        let mut latest_config = None;
        let mut others = Vec::new();
        for cmd in std::iter::once(first).chain(rx.try_iter()) {
            match cmd {
                Command::Config(c) => latest_config = Some(c),
                Command::Update => {
                    if let Err(e) = client::request("app", "POST", "/api/update", None) {
                        toast(&ui, format!("Couldn't update: {e}"));
                    }
                }
                other => others.push(other),
            }
        }
        let mut results = Vec::new();
        if let Some(c) = latest_config {
            results.push(client::request("app", "PUT", "/api/config", Some(&serde_json::to_value(&*c).expect("config serializes"))));
        }
        for cmd in others {
            results.push(match cmd {
                Command::AllowApp(title, allowed) => {
                    client::request("app", "POST", "/api/apps", Some(&json!({"title": title, "allowed": allowed})))
                }
                Command::Identify(target) => client::request("app", "POST", "/api/identify", Some(&json!({"target": target}))),
                Command::Config(_) | Command::Update => unreachable!(),
            });
        }
        if let Some(Err(e)) = results.into_iter().find(Result::is_err) {
            toast(&ui, format!("Couldn't save: {e}"));
        }
    }
}

fn offline_detail() -> String {
    match openchroma::service::state() {
        Ok(Some(state)) => format!("The service is {state:?}."),
        Ok(None) => "The service isn't installed. Run `openchroma service install` from an administrator terminal.".into(),
        Err(e) => e,
    }
}

/// Polls the service and hands results to the UI thread.
fn poller(ui: Weak<AppWindow>) {
    let mut n: u32 = 0;
    loop {
        let update = match client::request("app", "GET", "/api/status", None) {
            Ok(status) => {
                let config = if n.is_multiple_of(CONFIG_EVERY) {
                    client::request("app", "GET", "/api/config", None).ok().and_then(|v| serde_json::from_value(v).ok())
                } else {
                    None
                };
                let update = if n.is_multiple_of(UPDATE_EVERY) { client::request("app", "GET", "/api/update", None).ok() } else { None };
                n = n.wrapping_add(1);
                Update::Online { status, config, update }
            }
            Err(Error::Offline) => {
                n = 0;
                Update::Offline(offline_detail())
            }
            Err(Error::Failed(e)) => Update::Offline(e),
        };
        let ui = ui.clone();
        let sent = slint::invoke_from_event_loop(move || {
            let Some(w) = ui.upgrade() else { return };
            with_app(|app| match update {
                Update::Online { status, config, update } => {
                    w.set_online(true);
                    w.set_starting(false);
                    if let Some(c) = config {
                        app.state.receive(c);
                    }
                    if let Some(u) = update {
                        if u["state"] == "idle" && !u["error"].is_null() {
                            app.updating_from = None;
                        }
                        views::show_update(&w, &u);
                    }
                    let version = status["version"].as_str().unwrap_or_default();
                    w.set_version(version.into());
                    if app.updating_from.as_deref().is_some_and(|from| from != version) {
                        restart(&w, version);
                    }
                    app.status = status;
                    app.refresh();
                }
                Update::Offline(detail) => {
                    w.set_online(false);
                    w.set_offline_detail(detail.into());
                }
            });
        });
        if sent.is_err() {
            return; // event loop is gone
        }
        thread::sleep(if n == 0 { Duration::from_secs(1) } else { POLL_INTERVAL });
    }
}

/// The single-instance mutex, closed early when the app restarts itself.
static INSTANCE: AtomicPtr<std::ffi::c_void> = AtomicPtr::new(std::ptr::null_mut());

/// The service came back on a new version after an update the user started.
/// The installer replaced this program's file (the running copy was renamed
/// aside), so start the new one in its place.
fn restart(ui: &AppWindow, version: &str) {
    let dir = openchroma::service::install_dir();
    let installed = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d == dir)).unwrap_or(false);
    if !installed {
        // A development build: nothing of ours was replaced.
        with_app(|app| app.updating_from = None);
        ui.set_toast(format!("Updated to {version}").into());
        return;
    }
    let mutex = INSTANCE.swap(std::ptr::null_mut(), Ordering::SeqCst);
    if !mutex.is_null() {
        unsafe { windows_sys::Win32::Foundation::CloseHandle(mutex) };
    }
    match std::process::Command::new(dir.join("openchroma-app.exe")).spawn() {
        Ok(_) => std::process::exit(0),
        Err(e) => {
            with_app(|app| app.updating_from = None);
            ui.set_toast(format!("Updated to {version}; reopen OpenChroma ({e})").into());
        }
    }
}

/// Only one window: a second launch brings the first to the front.
fn already_running() -> bool {
    use windows_sys::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
    use windows_sys::Win32::System::Threading::CreateMutexW;
    use windows_sys::Win32::UI::WindowsAndMessaging::{FindWindowW, SetForegroundWindow, ShowWindow, SW_RESTORE};

    let name: Vec<u16> = "Local\\OpenChromaApp".encode_utf16().chain(Some(0)).collect();
    // The mutex lives as long as the process, unless `restart` hands over.
    let mutex = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
    if mutex.is_null() || unsafe { GetLastError() } != ERROR_ALREADY_EXISTS {
        INSTANCE.store(mutex, Ordering::SeqCst);
        return false;
    }
    let title: Vec<u16> = "OpenChroma".encode_utf16().chain(Some(0)).collect();
    let window = unsafe { FindWindowW(std::ptr::null(), title.as_ptr()) };
    if !window.is_null() {
        unsafe {
            ShowWindow(window, SW_RESTORE);
            SetForegroundWindow(window);
        }
    }
    true
}

fn main() -> Result<(), slint::PlatformError> {
    if already_running() {
        return Ok(());
    }
    let ui = AppWindow::new()?;
    // Development aids for screenshots: open on a page (0-3) and/or at a
    // size ("1280x820").
    if let Some(page) = std::env::var("OPENCHROMA_APP_PAGE").ok().and_then(|p| p.parse().ok()) {
        ui.set_page(page);
    }
    if let Some((w, h)) = std::env::var("OPENCHROMA_APP_SIZE").ok().and_then(|s| {
        let (w, h) = s.split_once('x')?;
        Some((w.parse().ok()?, h.parse().ok()?))
    }) {
        ui.window().set_size(slint::LogicalSize::new(w, h));
    }
    let (tx, rx) = mpsc::channel();
    let views = Views::new(&ui);
    APP.with(|a| {
        *a.borrow_mut() = Some(App { ui: ui.as_weak(), state: State::new(), views, status: Value::Null, writer: tx, updating_from: None })
    });

    // Every UI action edits the local state and commits it.
    macro_rules! on {
        ($setter:ident, |$app:ident $(, $arg:ident : $ty:ty)*| $body:expr) => {
            ui.$setter(move |$($arg: $ty),*| {
                with_app(|$app| $body);
            });
        };
    }
    on!(on_activate_profile, |app, name: slint::SharedString| {
        let c = app.state.activate(&name);
        app.commit(c)
    });
    on!(on_edit_profile, |app, name: slint::SharedString| {
        app.state.editing = name.to_string();
        app.refresh()
    });
    on!(on_new_profile, |app| {
        let c = app.state.new_profile();
        app.commit(c)
    });
    on!(on_duplicate_profile, |app| {
        let c = app.state.duplicate();
        app.commit(c)
    });
    on!(on_delete_profile, |app| {
        let c = app.state.delete();
        if c.is_none() {
            if let Some(ui) = app.ui.upgrade() {
                toast(&ui.as_weak(), "Keep at least one profile".into());
            }
        }
        app.commit(c)
    });
    on!(on_rename_profile, |app, name: slint::SharedString| {
        let c = app.state.rename(&name);
        app.commit(c)
    });
    on!(on_set_brightness, |app, v: f32| {
        let c = app.state.set_brightness(v);
        app.commit(c)
    });
    on!(on_set_games_allowed, |app, v: bool| {
        let c = app.state.set_games_allowed(v);
        app.commit(c)
    });
    on!(on_set_effect, |app, i: i32| {
        let c = app.state.set_effect(i as usize);
        app.commit(c)
    });
    on!(on_set_color, |app, i: i32, color: slint::Color| {
        let c = app.state.set_color(i as usize, views::from_slint(color));
        app.commit(c)
    });
    on!(on_add_color, |app| {
        let c = app.state.add_color();
        app.commit(c)
    });
    on!(on_remove_color, |app, i: i32| {
        let c = app.state.remove_color(i as usize);
        app.commit(c)
    });
    on!(on_set_background, |app, color: slint::Color| {
        let c = app.state.set_background(views::from_slint(color));
        app.commit(c)
    });
    on!(on_set_hex, |app, i: i32, text: slint::SharedString| {
        let Some(color) = openchroma::color::Color::parse(text.trim()) else { return };
        let c = if i < 0 { app.state.set_background(color) } else { app.state.set_color(i as usize, color) };
        app.commit(c)
    });
    on!(on_set_speed, |app, v: f32| {
        let c = app.state.set_speed(v);
        app.commit(c)
    });
    on!(on_set_reverse, |app, v: bool| {
        let c = app.state.set_reverse(v);
        app.commit(c)
    });
    on!(on_set_zone, |app, key: slint::SharedString, choice: i32| {
        let c = app.state.set_zone(&key, choice as usize);
        app.commit(c)
    });
    on!(on_paint_led, |app, device: slint::SharedString, row: i32, fan: i32, led: i32| {
        let (Some(ui), Some(config)) = (app.ui.upgrade(), app.state.config.as_ref()) else { return };
        let index = |v: i32| usize::try_from(v).unwrap_or(usize::MAX);
        let tool = ui.get_paint_tool();
        let cells = match tool {
            2 => app.views.device_cells(&device),
            _ => app.views.cells(config, &device, index(row), index(fan), index(led)),
        };
        let color = views::from_slint(ui.get_paint_color());
        let c = match tool {
            0 | 2 => app.state.paint(&device, &cells, Some(color)),
            1 => app.state.paint(&device, &cells, None),
            _ => app.state.erase_device(&device),
        };
        app.commit(c)
    });
    on!(on_paint_hex, |app, text: slint::SharedString| {
        let (Some(ui), Some(color)) = (app.ui.upgrade(), openchroma::color::Color::parse(text.trim())) else { return };
        ui.set_paint_color(views::to_slint(color));
    });
    on!(on_clear_paint, |app| {
        let c = app.state.erase_all();
        app.commit(c)
    });
    on!(on_set_mapping, |app, id: slint::SharedString, choice: i32| {
        let c = app.state.set_mapping(&id, choice as usize);
        app.commit(c)
    });
    on!(on_port_changed, |app, i: i32, name: slint::SharedString, is_fans: bool, fans: i32, fan_leds: i32, leds: i32, link: i32| {
        let clamp = |v: i32| v.clamp(0, 255) as u8;
        app.state.change_port(i as usize, &name, is_fans, clamp(fans), clamp(fan_leds), clamp(leds), link.max(0) as usize);
        app.refresh()
    });
    on!(on_save_ports, |app| {
        let c = app.state.save_ports();
        app.commit(c)
    });
    on!(on_discard_ports, |app| {
        app.state.discard_ports();
        app.refresh()
    });
    on!(on_identify, |app, port: i32| {
        let target = (port >= 0).then(|| format!("argb:{}", port + 1));
        let _ = app.writer.send(Command::Identify(target));
    });
    on!(on_set_app_allowed, |app, title: slint::SharedString, allowed: bool| {
        let _ = app.writer.send(Command::AllowApp(title.to_string(), allowed));
    });
    on!(on_update_now, |app| {
        app.updating_from = app.status["version"].as_str().map(str::to_owned);
        let _ = app.writer.send(Command::Update);
    });

    let weak = ui.as_weak();
    ui.on_start_service(move || {
        if let Some(w) = weak.upgrade() {
            w.set_starting(true);
        }
        let weak = weak.clone();
        thread::spawn(move || {
            if let Err(e) = openchroma::service::start() {
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(w) = weak.upgrade() {
                        w.set_starting(false);
                        w.set_offline_detail(e.into());
                    }
                });
            }
        });
    });

    {
        let weak = ui.as_weak();
        thread::spawn(move || writer(rx, weak));
    }
    {
        let weak = ui.as_weak();
        thread::spawn(move || poller(weak));
    }

    // Slint has no restore event, so watch for the minimized -> shown
    // transition and force a full repaint (see `repaint` in app.slint).
    let restore = slint::Timer::default();
    {
        let weak = ui.as_weak();
        let mut was_minimized = false;
        restore.start(slint::TimerMode::Repeated, Duration::from_millis(100), move || {
            let Some(w) = weak.upgrade() else { return };
            let minimized = w.window().is_minimized();
            if was_minimized && !minimized {
                w.set_repaint(!w.get_repaint());
            }
            was_minimized = minimized;
        });
    }
    ui.run()
}
