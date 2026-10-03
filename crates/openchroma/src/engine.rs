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
use razer_hid::{Device, DeviceInfo, DeviceKind, Rgb};
use serde::Serialize;

use crate::color::{self, Color};
use crate::config::Config;
use crate::effects::Effect;
use crate::layout::{self, DeviceLayout, Led, Source};
use crate::sdk::{Session, Sessions};

const RESCAN_INTERVAL: Duration = Duration::from_secs(2);
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(5);
const STATUS_INTERVAL: Duration = Duration::from_millis(100);

pub fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

pub struct Shared {
    pub config: RwLock<Config>,
    /// Bumped whenever `config` changes, so cached layouts get rebuilt.
    pub config_rev: AtomicU64,
    pub sessions: Mutex<Sessions>,
    pub devices: Mutex<Vec<DeviceStatus>>,
    pub started: Instant,
}

impl Shared {
    pub fn new(config: Config) -> Arc<Shared> {
        Arc::new(Shared {
            config: RwLock::new(config),
            config_rev: AtomicU64::new(0),
            sessions: Mutex::new(Sessions::default()),
            devices: Mutex::new(Vec::new()),
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
        let name = device.spec().name;
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

fn write_loop(device: &Device, mailbox: &(Mutex<Mailbox>, Condvar)) -> razer_hid::Result<()> {
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
    layout: DeviceLayout,
    writer: Option<Writer>,
    retry_at: Instant,
    status: DeviceStatus,
}

impl Slot {
    fn new(info: DeviceInfo, config: &Config) -> Slot {
        let spec = info.spec;
        let layout = layout::build(spec, config);
        let status = DeviceStatus {
            id: layout.id,
            name: spec.name,
            pid: format!("{:04X}", spec.pid),
            connected: false,
            firmware: None,
            serial: None,
            error: None,
            preview: Vec::new(),
            zones: layout.row_zones.clone(),
            game: layout.game,
        };
        Slot { info, layout, writer: None, retry_at: Instant::now(), status }
    }

    fn open(&mut self, api: &HidApi, config: &Config) {
        let spec = self.info.spec;
        let result = Device::open(api, self.info.clone()).and_then(|dev| {
            if spec.kind == DeviceKind::ArgbController {
                let mut sizes = [0u8; 6];
                for (s, ch) in sizes.iter_mut().zip(&config.argb_channels) {
                    *s = ch.leds.min(spec.cols);
                }
                dev.set_argb_channel_sizes(sizes)?;
            }
            // Software brightness does the dimming; make sure a previous
            // app did not leave the hardware dimmed.
            dev.set_brightness(255)?;
            Ok(dev)
        });
        match result {
            Ok(dev) => {
                self.status.firmware = dev.firmware().ok();
                self.status.serial = dev.serial().ok().filter(|s| s.chars().all(|c| c.is_ascii_graphic()) && !s.is_empty());
                self.status.error = None;
                self.status.connected = true;
                log::info!("opened {} (firmware {:?})", spec.name, self.status.firmware);
                self.writer = Some(Writer::spawn(dev));
            }
            Err(e) => {
                log::warn!("could not open {}: {e}", spec.name);
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

    loop {
        let tick = Instant::now();
        let config = shared.config();

        let rev = shared.config_rev.load(Ordering::SeqCst);
        if rev != layout_rev {
            layout_rev = rev;
            for slot in slots.values_mut() {
                let layout = layout::build(slot.info.spec, &config);
                // ARGB channel lengths are pushed to the controller on open,
                // so reopen it only when they change.
                let lengths = |l: &DeviceLayout| l.rows.iter().map(Vec::len).collect::<Vec<_>>();
                if slot.info.spec.kind == DeviceKind::ArgbController && lengths(&layout) != lengths(&slot.layout) {
                    slot.writer = None;
                }
                slot.status.zones = layout.row_zones.clone();
                slot.status.game = layout.game;
                slot.layout = layout;
            }
        }

        if last_scan.is_none_or(|t| t.elapsed() >= RESCAN_INTERVAL) {
            last_scan = Some(Instant::now());
            if let Err(e) = api.refresh_devices() {
                log::warn!("HID rescan failed: {e}");
            }
            let found: BTreeMap<String, DeviceInfo> = razer_hid::enumerate(&api).into_iter().map(|i| (i.key(), i)).collect();
            slots.retain(|key, slot| {
                let keep = found.contains_key(key);
                if !keep {
                    log::info!("{} disconnected", slot.info.spec.name);
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

    for slot in slots.values_mut() {
        let Some(writer) = &slot.writer else { continue };
        let layout = &slot.layout;
        let device_effect = profile.overrides.get(layout.id).unwrap_or(&profile.effect);
        let frame: Frame = layout
            .rows
            .iter()
            .zip(&layout.row_zones)
            .map(|(row, zone)| {
                let effect = zone.as_ref().and_then(|z| profile.overrides.get(z)).unwrap_or(device_effect);
                row.iter()
                    .map(|led| {
                        let c = active.and_then(|s| sdk_color(s, led)).unwrap_or_else(|| effect.render(t, led.pos));
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
