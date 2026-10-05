//! Device management and the render loop.
//!
//! The render thread rescans the bus every couple of seconds, (re)opens
//! devices, and computes one frame per device per tick. Each open device has
//! its own writer thread, so a slow or wedged device never delays the others,
//! and a write error just drops that device until the next rescan reopens it.
//! Writers also re-send the full frame every few seconds even when nothing
//! changed, which restores lighting after a device resets itself (sleep,
//! firmware hiccup, another app poking it).

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, RwLock};
use std::thread;
use std::time::{Duration, Instant};

use chroma_proto::{Category, SdkEffect};
use hidapi::HidApi;
use razer_hid::Rgb;
use serde::Serialize;

use crate::apps::Apps;
use crate::backend::{self, Device, DeviceInfo, Model};
use crate::color::{self, Color};
use crate::config::Config;
use crate::effects::Effect;
use crate::layout::{self, DeviceLayout, Led, Source};
use crate::sdk::{Client, Session, Sessions};

const RESCAN_INTERVAL: Duration = Duration::from_secs(2);
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(5);
const STATUS_INTERVAL: Duration = Duration::from_millis(100);
/// Identify stops by itself after this long.
const IDENTIFY_TIMEOUT: Duration = Duration::from_secs(15);
/// How long after a resume to wait before reopening devices, so USB has
/// settled.
const RESUME_SETTLE: Duration = Duration::from_secs(2);

/// Bumped on every resume from sleep or hibernation.
static RESUMES: AtomicU64 = AtomicU64::new(0);

/// Tell the render loop the system just woke up. Waking resets some
/// controllers to their own effect while the open handle stays valid, so
/// writes keep succeeding without showing anything (the Aura board drops out
/// of direct mode). Reopening repeats the setup each device needs.
pub fn system_resumed() {
    RESUMES.fetch_add(1, Ordering::SeqCst);
}

pub fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

pub struct Shared {
    pub config: RwLock<Config>,
    /// Bumped whenever `config` changes, so cached layouts get rebuilt.
    pub config_rev: AtomicU64,
    pub sessions: Mutex<Sessions>,
    pub devices: Mutex<Vec<DeviceStatus>>,
    pub apps: Mutex<Apps>,
    /// Device id or zone key ("argb:4") currently flashing to be found, and
    /// since when.
    pub identify: Mutex<Option<(String, Instant)>>,
    pub started: Instant,
}

impl Shared {
    pub fn new(config: Config) -> Arc<Shared> {
        Arc::new(Shared {
            config: RwLock::new(config),
            config_rev: AtomicU64::new(0),
            sessions: Mutex::new(Sessions::default()),
            devices: Mutex::new(Vec::new()),
            apps: Mutex::new(Apps::load()),
            identify: Mutex::new(None),
            started: Instant::now(),
        })
    }

    pub fn config(&self) -> Config {
        self.config.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn update_config(&self, f: impl FnOnce(&mut Config)) -> std::io::Result<()> {
        let snapshot = {
            let mut cfg = self.config.write().unwrap_or_else(|e| e.into_inner());
            f(&mut cfg);
            *cfg = cfg.clone().normalized();
            cfg.clone()
        };
        self.config_rev.fetch_add(1, Ordering::SeqCst);
        snapshot.save()
    }

    /// Start an SDK session, remembering the app and honoring the user's
    /// allow/block choice for it.
    pub fn open_session(&self, app: chroma_proto::AppInfo, client: Client) -> u64 {
        let kind = if matches!(client, Client::Rest) { "rest" } else { "native" };
        let allowed = lock(&self.apps).seen(&app.title, kind);
        lock(&self.sessions).open(app, client, allowed)
    }

    /// Returns false if the app has never connected.
    pub fn set_app_allowed(&self, title: &str, allowed: bool) -> bool {
        let known = lock(&self.apps).set_allowed(title, allowed);
        if known {
            lock(&self.sessions).set_allowed(title, allowed);
        }
        known
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct DeviceStatus {
    pub id: &'static str,
    pub name: &'static str,
    pub pid: String,
    pub connected: bool,
    pub firmware: Option<String>,
    pub serial: Option<String>,
    pub error: Option<String>,
    /// Current colors per row as "#rrggbb", for the UI preview.
    pub preview: Vec<Vec<String>>,
    pub zones: Vec<Option<String>>,
    /// The game canvas this device shows, if any.
    pub game: Option<Category>,
}

type Frame = Vec<Vec<Rgb>>;

#[derive(Default)]
struct Mailbox {
    frame: Option<Frame>,
    closed: bool,
}

struct Writer {
    mailbox: Arc<(Mutex<Mailbox>, Condvar)>,
    alive: Arc<AtomicBool>,
    error: Arc<Mutex<Option<String>>>,
}

impl Writer {
    fn spawn(device: Device) -> Writer {
        let mailbox = Arc::new((Mutex::new(Mailbox::default()), Condvar::new()));
        let alive = Arc::new(AtomicBool::new(true));
        let error = Arc::new(Mutex::new(None));
        let name = device.name();
        {
            let (mailbox, alive, error) = (mailbox.clone(), alive.clone(), error.clone());
            thread::Builder::new()
                .name(format!("writer {name}"))
                .spawn(move || {
                    if let Err(e) = write_loop(&device, &mailbox) {
                        log::warn!("{name}: {e}; will reconnect");
                        *lock(&error) = Some(e.to_string());
                    }
                    alive.store(false, Ordering::SeqCst);
                })
                .expect("spawn device writer");
        }
        Writer { mailbox, alive, error }
    }

    fn post(&self, frame: Frame) {
        let (m, cv) = &*self.mailbox;
        lock(m).frame = Some(frame);
        cv.notify_one();
    }

    fn is_alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }
}

impl Drop for Writer {
    fn drop(&mut self) {
        let (m, cv) = &*self.mailbox;
        lock(m).closed = true;
        cv.notify_one();
    }
}

fn write_loop(device: &Device, mailbox: &(Mutex<Mailbox>, Condvar)) -> backend::Result<()> {
    let (m, cv) = mailbox;
    let mut sent: Option<Frame> = None;
    let mut last_full = Instant::now();
    loop {
        let next = {
            let guard = lock(m);
            let (mut guard, _) =
                cv.wait_timeout_while(guard, KEEPALIVE_INTERVAL, |mb| mb.frame.is_none() && !mb.closed).unwrap_or_else(|e| e.into_inner());
            if guard.closed {
                return Ok(());
            }
            guard.frame.take()
        };
        let Some(frame) = next.or_else(|| sent.clone()) else { continue };

        let full = sent.is_none() || last_full.elapsed() >= KEEPALIVE_INTERVAL;
        let rows: Frame = frame
            .iter()
            .enumerate()
            .map(|(i, row)| {
                let changed = sent.as_ref().and_then(|s| s.get(i)) != Some(row);
                if full || changed {
                    row.clone()
                } else {
                    Vec::new()
                }
            })
            .collect();
        if rows.iter().all(Vec::is_empty) {
            continue;
        }
        device.set_frame(&rows)?;
        if full {
            last_full = Instant::now();
        }
        sent = Some(frame);
    }
}

struct Slot {
    info: DeviceInfo,
    model: Model,
    layout: DeviceLayout,
    writer: Option<Writer>,
    retry_at: Instant,
    status: DeviceStatus,
}

impl Slot {
    fn new(info: DeviceInfo, config: &Config) -> Slot {
        let model = info.model();
        let layout = layout::build(model, config);
        let status = DeviceStatus {
            id: layout.id,
            name: info.name(),
            pid: format!("{:04X}", info.pid()),
            connected: false,
            firmware: None,
            serial: None,
            error: None,
            preview: Vec::new(),
            zones: layout.row_zones.clone(),
            game: layout.game,
        };
        Slot { info, model, layout, writer: None, retry_at: Instant::now(), status }
    }

    fn set_layout(&mut self, layout: DeviceLayout) {
        self.status.zones = layout.row_zones.clone();
        self.status.game = layout.game;
        self.layout = layout;
    }

    fn open(&mut self, api: &HidApi, config: &Config) {
        let name = self.info.name();
        match Device::open(api, self.info.clone(), config) {
            Ok(dev) => {
                // Some devices only tell their LED count once opened.
                self.model = dev.model();
                self.set_layout(layout::build(self.model, config));
                self.status.firmware = dev.firmware();
                self.status.serial = dev.serial();
                self.status.error = None;
                self.status.connected = true;
                log::info!("opened {name} (firmware {:?})", self.status.firmware);
                self.writer = Some(Writer::spawn(dev));
            }
            Err(e) => {
                log::warn!("could not open {name}: {e}");
                self.status.connected = false;
                self.status.error = Some(e.to_string());
                self.retry_at = Instant::now() + RESCAN_INTERVAL;
            }
        }
    }
}

pub fn run(shared: Arc<Shared>) {
    let mut api = loop {
        match HidApi::new() {
            Ok(api) => break api,
            Err(e) => {
                log::error!("HID init failed: {e}");
                thread::sleep(RESCAN_INTERVAL);
            }
        }
    };
    let mut slots: BTreeMap<String, Slot> = BTreeMap::new();
    let mut last_scan: Option<Instant> = None;
    let mut last_status = Instant::now();
    let mut layout_rev = shared.config_rev.load(Ordering::SeqCst);
    let mut resumes = RESUMES.load(Ordering::SeqCst);

    loop {
        let tick = Instant::now();
        let config = shared.config();

        let r = RESUMES.load(Ordering::SeqCst);
        if r != resumes {
            resumes = r;
            log::info!("system resumed; reopening devices");
            for slot in slots.values_mut() {
                slot.writer = None;
                slot.retry_at = Instant::now() + RESUME_SETTLE;
            }
        }

        let rev = shared.config_rev.load(Ordering::SeqCst);
        if rev != layout_rev {
            layout_rev = rev;
            for slot in slots.values_mut() {
                let layout = layout::build(slot.model, &config);
                // Some devices take their row lengths on open, so reopen them
                // only when the lengths change.
                let lengths = |l: &DeviceLayout| l.rows.iter().map(Vec::len).collect::<Vec<_>>();
                if slot.model.sizes_set_on_open() && lengths(&layout) != lengths(&slot.layout) {
                    slot.writer = None;
                }
                slot.set_layout(layout);
            }
        }

        if last_scan.is_none_or(|t| t.elapsed() >= RESCAN_INTERVAL) {
            last_scan = Some(Instant::now());
            if let Err(e) = api.refresh_devices() {
                log::warn!("HID rescan failed: {e}");
            }
            let found: BTreeMap<String, DeviceInfo> = backend::enumerate(&api).into_iter().map(|i| (i.key(), i)).collect();
            slots.retain(|key, slot| {
                let keep = found.contains_key(key);
                if !keep {
                    log::info!("{} disconnected", slot.info.name());
                }
                keep
            });
            for (key, info) in found {
                slots.entry(key).or_insert_with(|| Slot::new(info, &config));
            }
        }

        for slot in slots.values_mut() {
            if let Some(w) = &slot.writer {
                if !w.is_alive() {
                    slot.status.error = lock(&w.error).take();
                    slot.status.connected = false;
                    slot.writer = None;
                    slot.retry_at = Instant::now() + Duration::from_millis(500);
                }
            }
            if slot.writer.is_none() && Instant::now() >= slot.retry_at {
                slot.open(&api, &config);
            }
        }

        render(&shared, &config, &mut slots);

        if last_status.elapsed() >= STATUS_INTERVAL {
            last_status = Instant::now();
            *lock(&shared.devices) = slots.values().map(|s| s.status.clone()).collect();
        }

        let frame_time = Duration::from_secs_f32(1.0 / config.fps as f32);
        if let Some(rest) = frame_time.checked_sub(tick.elapsed()) {
            thread::sleep(rest);
        }
    }
}

fn render(shared: &Shared, config: &Config, slots: &mut BTreeMap<String, Slot>) {
    let t = shared.started.elapsed().as_secs_f32();
    let profile = config.profile();
    let brightness = config.brightness as f32 / 100.0;

    let mut sessions = lock(&shared.sessions);
    sessions.expire();
    let active = if config.sdk_enabled { sessions.active() } else { None };

    let identify = {
        let mut id = lock(&shared.identify);
        if id.as_ref().is_some_and(|(_, since)| since.elapsed() > IDENTIFY_TIMEOUT) {
            *id = None;
        }
        id.as_ref().map(|(target, _)| target.clone())
    };
    // Flash at 2 Hz so the target stands out against any effect.
    let flash = if (t * 2.0).fract() < 0.5 { Rgb::new(255, 255, 255) } else { Rgb::BLACK };

    for slot in slots.values_mut() {
        let Some(writer) = &slot.writer else { continue };
        let layout = &slot.layout;
        let device_effect = profile.overrides.get(layout.id).unwrap_or(&profile.effect);
        let frame: Frame = layout
            .rows
            .iter()
            .zip(&layout.row_zones)
            .enumerate()
            .map(|(r, (row, zone))| {
                let effect = zone.as_ref().and_then(|z| profile.overrides.get(z)).unwrap_or(device_effect);
                let identified = identify.as_deref().is_some_and(|i| i == layout.id || zone.as_deref() == Some(i));
                row.iter()
                    .enumerate()
                    .map(|(col, led)| {
                        if identified {
                            return flash;
                        }
                        let c = active.and_then(|s| sdk_color(s, led)).unwrap_or_else(|| match profile.painted(layout.id, r, col) {
                            Some(painted) => painted.0,
                            None => effect.render(t, led.pos),
                        });
                        color::scale(c, brightness)
                    })
                    .collect()
            })
            .collect();
        slot.status.preview = frame.iter().map(|row| row.iter().map(|&c| Color(c).hex()).collect()).collect();
        writer.post(frame);
    }
}

/// Color an LED from the game's canvas, if the game has drawn on it.
fn sdk_color(session: &Session, led: &Led) -> Option<Rgb> {
    let source = led.source?;
    let category = match source {
        Source::Cell(c, _) | Source::Average(c) => c,
    };
    let (effect, since) = session.shown.get(&category)?;
    let t = since.elapsed().as_secs_f32();
    Some(match effect {
        SdkEffect::None => Rgb::BLACK,
        SdkEffect::Static { color } => color::from_colorref(*color),
        SdkEffect::Custom { colors } => match source {
            Source::Cell(_, i) => colors.get(i).map_or(Rgb::BLACK, |&c| color::from_colorref(c)),
            Source::Average(_) => average(colors),
        },
        SdkEffect::Breathing { color1, color2, random } => {
            let colors = if *random {
                (0..6).map(|i| Color(color::hue(i as f32 / 6.0))).collect()
            } else {
                std::iter::once(*color1).chain(*color2).map(|c| Color(color::from_colorref(c))).collect()
            };
            Effect::Breathing { colors, period: 4.0 }.render(t, led.pos)
        }
        SdkEffect::Spectrum => Effect::Spectrum { period: 12.0 }.render(t, led.pos),
        SdkEffect::Wave { reverse } => Effect::Wave { period: 4.0, repeat: 1.0, reverse: *reverse }.render(t, led.pos),
    })
}

fn average(colors: &[u32]) -> Rgb {
    if colors.is_empty() {
        return Rgb::BLACK;
    }
    let (mut r, mut g, mut b) = (0u32, 0u32, 0u32);
    for &c in colors {
        let x = color::from_colorref(c);
        r += x.r as u32;
        g += x.g as u32;
        b += x.b as u32;
    }
    let n = colors.len() as u32;
    Rgb::new((r / n) as u8, (g / n) as u8, (b / n) as u8)
}

/// Which SDK categories have hardware behind them right now.
pub fn available_categories(shared: &Shared) -> Vec<Category> {
    let devices = lock(&shared.devices);
    let mut cats = Vec::new();
    for c in devices.iter().filter(|d| d.connected).filter_map(|d| d.game) {
        if !cats.contains(&c) {
            cats.push(c);
        }
    }
    cats
}
