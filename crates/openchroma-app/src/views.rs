//! Turns the service's status and config into the UI's models.
//!
//! Status arrives about ten times a second. LED colors are written into
//! long-lived models in place, so the UI repaints the dots instead of
//! rebuilding elements (which would also reset hover and focus).

use std::rc::Rc;

use openchroma::config::{ArgbChannel, Config, GameSource};
use openchroma::effects::Effect;
use serde_json::Value;
use slint::{Color, Model, ModelRc, SharedString, VecModel};

use crate::state::{self, State, EFFECT_LABELS, MAPPING_OPTIONS, MAPPING_SOURCES, ZONE_OPTIONS};
use crate::{
    AppRow, AppWindow, ChannelPreview, CheckRow, ColorRow, EffectOption, EffectRow, MappingRow, PortEdit, PortRow, ProfileItem, ZoneItem,
};

const PRESETS: [&str; 16] = [
    "#ffffff", "#ff2000", "#ff7000", "#ffd000", "#a0ff00", "#44d62c", "#00ffa0", "#00c8ff", "#0060ff", "#0010ff", "#7a3cff", "#c000ff",
    "#ff00a0", "#ff6f91", "#40a0ff", "#000010",
];

fn rgb(hex: &str) -> Color {
    let v = u32::from_str_radix(hex.trim_start_matches('#'), 16).unwrap_or(0);
    Color::from_rgb_u8((v >> 16) as u8, (v >> 8) as u8, v as u8)
}

fn to_slint(c: openchroma::color::Color) -> Color {
    Color::from_rgb_u8(c.0.r, c.0.g, c.0.b)
}

pub fn from_slint(c: Color) -> openchroma::color::Color {
    openchroma::color::Color(openchroma::color::Rgb::new(c.red(), c.green(), c.blue()))
}

/// Sample a gradient through `colors` into slices for a `Swatch`.
fn gradient(colors: &[Color]) -> ModelRc<Color> {
    const SLICES: usize = 16;
    let slices: Vec<Color> = match colors.len() {
        0 => vec![Color::from_rgb_u8(0x1e, 0x22, 0x28)],
        1 => vec![colors[0]],
        n => (0..SLICES)
            .map(|i| {
                let pos = i as f32 / (SLICES - 1) as f32 * (n - 1) as f32;
                let k = (pos as usize).min(n - 2);
                let t = pos - k as f32;
                let (a, b) = (colors[k], colors[k + 1]);
                let mix = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
                Color::from_rgb_u8(mix(a.red(), b.red()), mix(a.green(), b.green()), mix(a.blue(), b.blue()))
            })
            .collect(),
    };
    ModelRc::new(VecModel::from(slices))
}

fn swatch(e: &Effect) -> ModelRc<Color> {
    let rainbow = ["#ff4d4d", "#ffd34d", "#5EE6A8", "#4dc3ff", "#9b6bff"].map(rgb);
    match e {
        Effect::Off => gradient(&[]),
        Effect::Spectrum { .. } => gradient(&[rgb("#4dc3ff"), rgb("#9b6bff")]),
        Effect::Wave { .. } => gradient(&rainbow),
        Effect::Breathing { colors, .. } => {
            let mut c = vec![rgb("#0b0d10")];
            c.extend(colors.iter().map(|&c| to_slint(c)));
            c.push(rgb("#0b0d10"));
            gradient(&c)
        }
        Effect::Starlight { colors, background, .. } => {
            let mut c = vec![to_slint(*background)];
            c.extend(colors.iter().map(|&c| to_slint(c)));
            c.push(to_slint(*background));
            gradient(&c)
        }
        other => gradient(&state::effect_colors(other).unwrap_or_default().into_iter().map(to_slint).collect::<Vec<_>>()),
    }
}

/// Update a model in place, touching only rows that changed.
fn sync<T: Clone + PartialEq + 'static>(model: &VecModel<T>, items: Vec<T>) {
    if model.row_count() != items.len() {
        model.set_vec(items);
        return;
    }
    for (i, item) in items.into_iter().enumerate() {
        if model.row_data(i).as_ref() != Some(&item) {
            model.set_row_data(i, item);
        }
    }
}

/// Rows of LED colors backed by persistent models.
struct Leds {
    outer: Rc<VecModel<ColorRow>>,
    inner: Vec<Rc<VecModel<Color>>>,
}

impl Leds {
    fn new() -> Leds {
        Leds { outer: Rc::new(VecModel::default()), inner: Vec::new() }
    }

    fn model(&self) -> ModelRc<ColorRow> {
        ModelRc::from(self.outer.clone())
    }

    fn sync(&mut self, rows: Vec<Vec<Color>>) {
        let same_shape = rows.len() == self.inner.len() && rows.iter().zip(&self.inner).all(|(r, m)| r.len() == m.row_count());
        if !same_shape {
            self.inner = rows.into_iter().map(|r| Rc::new(VecModel::from(r))).collect();
            self.outer.set_vec(self.inner.iter().map(|m| ColorRow { c: ModelRc::from(m.clone()) }).collect::<Vec<_>>());
            return;
        }
        for (m, row) in self.inner.iter().zip(rows) {
            sync(m, row);
        }
    }
}

/// One ARGB channel drawn as fans or as a strip.
struct Channel {
    fans: Leds,
    strip: Rc<VecModel<Color>>,
}

impl Channel {
    fn new() -> Channel {
        Channel { fans: Leds::new(), strip: Rc::new(VecModel::default()) }
    }

    fn sync(&mut self, cfg: &ArgbChannel, leds: &[Color]) {
        if cfg.fans.is_empty() {
            sync(&self.strip, leds.to_vec());
            self.fans.sync(Vec::new());
        } else {
            let mut rest = leds;
            let fans = cfg
                .fans
                .iter()
                .map(|&n| {
                    let (fan, tail) = rest.split_at((n as usize).min(rest.len()));
                    rest = tail;
                    fan.to_vec()
                })
                .collect();
            self.fans.sync(fans);
            sync(&self.strip, Vec::new());
        }
    }

    fn preview(&self, name: &str, is_fans: bool) -> ChannelPreview {
        ChannelPreview { name: name.into(), is_fans, fans: self.fans.model(), strip: ModelRc::from(self.strip.clone()) }
    }
}

pub struct Views {
    keyboard: Leds,
    case_strips: Leds,
    channels: Vec<Channel>,
    channels_left: Rc<VecModel<ChannelPreview>>,
    channels_right: Rc<VecModel<ChannelPreview>>,
    profiles: Rc<VecModel<ProfileItem>>,
    preview_strip: Rc<VecModel<Color>>,
    editor_colors: Rc<VecModel<Color>>,
    zones: Rc<VecModel<ZoneItem>>,
    port_previews: Vec<Leds>,
    port_rows: [Rc<VecModel<PortEdit>>; 2],
    mapping: Rc<VecModel<MappingRow>>,
    checks: Rc<VecModel<CheckRow>>,
    apps: Rc<VecModel<AppRow>>,
    game_devices: Rc<VecModel<SharedString>>,
    editor_shown: String,
}

fn strings(items: &[&str]) -> ModelRc<SharedString> {
    ModelRc::new(VecModel::from(items.iter().map(|s| SharedString::from(*s)).collect::<Vec<_>>()))
}

impl Views {
    /// Create the models and attach them to the window once.
    pub fn new(ui: &AppWindow) -> Views {
        let v = Views {
            keyboard: Leds::new(),
            case_strips: Leds::new(),
            channels: (0..6).map(|_| Channel::new()).collect(),
            channels_left: Rc::new(VecModel::default()),
            channels_right: Rc::new(VecModel::default()),
            profiles: Rc::new(VecModel::default()),
            preview_strip: Rc::new(VecModel::default()),
            editor_colors: Rc::new(VecModel::default()),
            zones: Rc::new(VecModel::default()),
            port_previews: (0..6).map(|_| Leds::new()).collect(),
            port_rows: [Rc::new(VecModel::default()), Rc::new(VecModel::default())],
            mapping: Rc::new(VecModel::default()),
            checks: Rc::new(VecModel::default()),
            apps: Rc::new(VecModel::default()),
            game_devices: Rc::new(VecModel::default()),
            editor_shown: String::new(),
        };
        ui.set_keyboard(v.keyboard.model());
        ui.set_case_strips(v.case_strips.model());
        ui.set_channels_left(ModelRc::from(v.channels_left.clone()));
        ui.set_channels_right(ModelRc::from(v.channels_right.clone()));
        ui.set_profiles(ModelRc::from(v.profiles.clone()));
        ui.set_preview_strip(ModelRc::from(v.preview_strip.clone()));
        ui.set_editor_colors(ModelRc::from(v.editor_colors.clone()));
        ui.set_zones(ModelRc::from(v.zones.clone()));
        ui.set_port_rows(ModelRc::new(VecModel::from(
            v.port_rows.iter().map(|m| PortRow { ports: ModelRc::from(m.clone()) }).collect::<Vec<_>>(),
        )));
        ui.set_mapping(ModelRc::from(v.mapping.clone()));
        ui.set_checks(ModelRc::from(v.checks.clone()));
        ui.set_apps(ModelRc::from(v.apps.clone()));
        ui.set_game_devices(ModelRc::from(v.game_devices.clone()));
        ui.set_zone_options(strings(&ZONE_OPTIONS));
        ui.set_mapping_options(strings(&MAPPING_OPTIONS));
        ui.set_link_options(strings(&[
            "Auto",
            "Chroma Link LED 0",
            "Chroma Link LED 1",
            "Chroma Link LED 2",
            "Chroma Link LED 3",
            "Chroma Link LED 4",
        ]));
        ui.set_color_presets(ModelRc::new(VecModel::from(PRESETS.iter().map(|h| rgb(h)).collect::<Vec<_>>())));

        // Effect cards never change; their swatches show the effect, not the
        // profile's colors.
        let sample = |i: usize| -> ModelRc<Color> {
            let c = |s: &[&str]| s.iter().map(|h| rgb(h)).collect::<Vec<_>>();
            match i {
                0 => gradient(&c(&["#00c8ff"])),
                1 => gradient(&c(&["#0b0d10", "#00c8ff", "#0b0d10"])),
                2 => gradient(&c(&["#4dc3ff", "#9b6bff"])),
                3 => gradient(&c(&["#ff4d4d", "#ffd34d", "#5EE6A8", "#4dc3ff", "#9b6bff"])),
                4 => gradient(&c(&["#0010ff", "#00ffa0"])),
                5 => gradient(&c(&["#0010ff", "#00c8ff", "#00ffa0", "#00c8ff", "#0010ff"])),
                6 => gradient(&c(&["#0b0d2a", "#ffffff", "#0b0d2a", "#40a0ff", "#0b0d2a"])),
                _ => gradient(&[]),
            }
        };
        let rows: Vec<EffectRow> = (0..2)
            .map(|r| EffectRow {
                items: ModelRc::new(VecModel::from(
                    (r * 4..r * 4 + 4)
                        .map(|i| EffectOption { index: i as i32, label: EFFECT_LABELS[i].into(), swatch: sample(i) })
                        .collect::<Vec<_>>(),
                )),
            })
            .collect();
        ui.set_effect_rows(ModelRc::new(VecModel::from(rows)));
        v
    }

    pub fn refresh(&mut self, ui: &AppWindow, state: &State, status: &Value) {
        let Some(config) = state.config.as_ref() else { return };
        let devices: Vec<&Value> = status["devices"].as_array().map(|d| d.iter().collect()).unwrap_or_default();
        let device = |id: &str| devices.iter().copied().find(|d| d["id"] == id);
        let preview = |d: &Value| -> Vec<Vec<Color>> {
            d["preview"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|row| row.as_array().into_iter().flatten().filter_map(Value::as_str).map(rgb).collect())
                .collect()
        };

        ui.set_devices_total(devices.len() as i32);
        ui.set_devices_connected(devices.iter().filter(|d| d["connected"] == true).count() as i32);

        // ---- settings
        ui.set_active_profile(config.active_profile.as_str().into());
        ui.set_brightness(config.brightness as f32 / 100.0);
        ui.set_games_allowed(config.sdk_enabled);

        // ---- setup preview
        let keyboard = device("keyboard").map(preview).unwrap_or_default();
        ui.set_has_keyboard(!keyboard.is_empty());
        self.keyboard.sync(keyboard.clone());

        let case = device("case").map(preview).unwrap_or_default();
        self.case_strips.sync(case.clone());

        let argb = device("argb").map(preview).unwrap_or_default();
        let used: Vec<usize> = (0..config.argb_channels.len().min(6)).filter(|&i| config.argb_channels[i].leds > 0).collect();
        for &i in &used {
            let leds = argb.get(i).cloned().unwrap_or_default();
            self.channels[i].sync(&config.argb_channels[i], &leds);
        }
        let previews: Vec<ChannelPreview> = used
            .iter()
            .map(|&i| self.channels[i].preview(&config.argb_channels[i].name, !config.argb_channels[i].fans.is_empty()))
            .collect();
        let (left, right) = previews.split_at(previews.len().min(1));
        sync(&self.channels_left, left.to_vec());
        sync(&self.channels_right, right.to_vec());
        ui.set_has_case(!case.is_empty() || !used.is_empty());
        let mut caption = Vec::new();
        if !case.is_empty() {
            caption.push("Lian Li O11 Dynamic".to_string());
        }
        caption.extend(used.iter().map(|&i| config.argb_channels[i].name.clone()));
        ui.set_case_caption(caption.join(" · ").into());

        let mouse = device("mouse").map(preview).unwrap_or_default();
        let m = mouse.first().cloned().unwrap_or_default();
        ui.set_has_mouse(!m.is_empty());
        if m.len() >= 2 {
            ui.set_mouse_logo(m[0]);
            ui.set_mouse_wheel(m[1]);
            ui.set_mouse_glow(m.get(m.len() / 2 + 1).copied().unwrap_or(m[0]));
        }
        let pad = device("mousepad").map(preview).unwrap_or_default();
        ui.set_has_pad(!pad.is_empty());
        if let Some(&c) = pad.first().and_then(|r| r.first()) {
            ui.set_pad(c);
        }

        // ---- profiles
        sync(
            &self.profiles,
            config
                .profiles
                .iter()
                .map(|p| ProfileItem {
                    name: p.name.as_str().into(),
                    effect: EFFECT_LABELS[state::effect_index(&p.effect)].into(),
                    swatch: swatch(&p.effect),
                    active: p.name == config.active_profile,
                })
                .collect(),
        );

        self.refresh_editor(ui, state, config, &keyboard, &case, &devices);
        self.refresh_ports(ui, state, status, &argb);
        self.refresh_games(ui, config, status, &devices);
    }

    fn refresh_editor(
        &mut self,
        ui: &AppWindow,
        state: &State,
        config: &Config,
        keyboard: &[Vec<Color>],
        case: &[Vec<Color>],
        devices: &[&Value],
    ) {
        let Some(profile) = state.profile() else { return };
        // The name field is the user's while they type; only reset it when a
        // different profile is opened.
        if self.editor_shown != state.editing {
            self.editor_shown = state.editing.clone();
            ui.set_editor_name(profile.name.as_str().into());
        }
        let e = &profile.effect;
        ui.set_editor_effect(state::effect_index(e) as i32);
        let colors = state::effect_colors(e).unwrap_or_default();
        sync(&self.editor_colors, colors.iter().map(|&c| to_slint(c)).collect());
        let period = state::effect_period(e);
        ui.set_show_colors(!colors.is_empty());
        ui.set_show_speed(period.is_some());
        ui.set_editor_speed(period.map(state::period_to_speed).unwrap_or(0.5));
        ui.set_editor_speed_label(period.map(|p| format!("{p:.1} s per cycle")).unwrap_or_default().into());
        ui.set_show_direction(state::effect_reverse(e).is_some());
        ui.set_editor_reverse(state::effect_reverse(e).unwrap_or(false));
        match e {
            Effect::Starlight { background, .. } => {
                ui.set_show_background(true);
                ui.set_editor_background(to_slint(*background));
            }
            _ => ui.set_show_background(false),
        }

        let mut strip: Vec<Color> = keyboard.get(2).cloned().unwrap_or_default();
        strip.extend(case.first().cloned().unwrap_or_default());
        sync(&self.preview_strip, strip);

        let has = |id: &str| devices.iter().any(|d| d["id"] == id);
        let mut zones = Vec::new();
        let mut zone = |key: &str, name: &str, device: &str| {
            let choice = state::zone_choice(profile.overrides.get(key));
            zones.push(ZoneItem {
                key: key.into(),
                name: name.into(),
                device: device.into(),
                choice: choice as i32,
                overridden: choice != 0,
            });
        };
        if has("keyboard") {
            zone("keyboard", "Keyboard", "BlackWidow Chroma V2");
        }
        if has("mouse") {
            zone("mouse", "Mouse", "Basilisk V3");
        }
        if has("mousepad") {
            zone("mousepad", "Mousepad", "Goliathus Chroma Extended");
        }
        if has("case") {
            zone("case", "Case strips", "Lian Li O11 Dynamic");
        }
        if has("argb") {
            for (i, ch) in config.argb_channels.iter().enumerate().filter(|(_, c)| c.leds > 0) {
                let detail = if ch.fans.is_empty() {
                    format!("ARGB channel {} · {} LEDs", i + 1, ch.leds)
                } else {
                    format!("ARGB channel {} · {} fans", i + 1, ch.fans.len())
                };
                zone(&format!("argb:{}", i + 1), &ch.name, &detail);
            }
        }
        sync(&self.zones, zones);
    }

    fn refresh_ports(&mut self, ui: &AppWindow, state: &State, status: &Value, argb: &[Vec<Color>]) {
        let ports = state.ports();
        let flashing = status["identify"].as_str().unwrap_or("");
        let mut edits = Vec::new();
        for (i, p) in ports.iter().enumerate().take(6) {
            let leds = argb.get(i).cloned().unwrap_or_default();
            // Preview the draft's shape with whatever colors are live.
            let shaped: Vec<Color> = (0..p.leds as usize).map(|k| leds.get(k).copied().unwrap_or(rgb("#262a31"))).collect();
            let rows = if p.fans.is_empty() {
                shaped.chunks(16).map(<[Color]>::to_vec).collect()
            } else {
                let mut rest = shaped.as_slice();
                p.fans
                    .iter()
                    .map(|&n| {
                        let (fan, tail) = rest.split_at((n as usize).min(rest.len()));
                        rest = tail;
                        fan.to_vec()
                    })
                    .collect()
            };
            self.port_previews[i].sync(rows);
            edits.push(PortEdit {
                index: i as i32,
                name: p.name.as_str().into(),
                is_fans: !p.fans.is_empty(),
                fans: p.fans.len().max(1) as i32,
                fan_leds: p.fans.first().copied().unwrap_or(8) as i32,
                leds: p.leds as i32,
                link: p.chroma_link_led.map_or(0, |l| l as i32 + 1),
                used: p.leds > 0,
                flashing: flashing == format!("argb:{}", i + 1),
                preview: self.port_previews[i].model(),
            });
        }
        let (a, b) = edits.split_at(edits.len().min(3));
        sync(&self.port_rows[0], a.to_vec());
        sync(&self.port_rows[1], b.to_vec());
        ui.set_ports_dirty(state.port_draft.is_some());
    }

    fn refresh_games(&mut self, ui: &AppWindow, config: &Config, status: &Value, devices: &[&Value]) {
        let category_name = |c: &str| match c {
            "keyboard" => "Keyboard",
            "mouse" => "Mouse",
            "mousepad" => "Mousepad",
            "chromalink" => "Chroma Link",
            "headset" => "Headset",
            "keypad" => "Keypad",
            _ => "Device",
        };

        let sessions = status["sessions"].as_array().cloned().unwrap_or_default();
        match sessions.iter().find(|s| s["active"] == true) {
            Some(s) => {
                let title = s["title"].as_str().unwrap_or("A game").to_string();
                let cats: Vec<String> =
                    s["categories"].as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_owned).collect();
                // Which of the user's devices show what the game draws.
                let shown: Vec<String> = devices
                    .iter()
                    .filter(|d| d["game"].as_str().is_some_and(|g| cats.iter().any(|c| c == g)))
                    .filter_map(|d| d["name"].as_str().map(friendly))
                    .collect();
                let kind = if s["client"]["kind"] == "rest" { "REST API" } else { "Native SDK" };
                let minutes = s["seconds"].as_u64().unwrap_or(0) / 60;
                ui.set_game_title(title.as_str().into());
                ui.set_game_detail(format!("{kind} · controlling the lights for {minutes} min").into());
                ui.set_game_banner(if shown.is_empty() {
                    format!("{title} is connected but hasn't drawn on your devices yet.").into()
                } else {
                    format!("{title} is controlling the {}. Your profile keeps running on everything else.", join_and(&shown)).into()
                });
                sync(&self.game_devices, cats.iter().map(|c| SharedString::from(category_name(c))).collect());
            }
            None => {
                ui.set_game_title("".into());
                sync(&self.game_devices, Vec::new());
            }
        }

        const ORDER: [&str; 5] = ["keyboard", "mouse", "mousepad", "case", "argb"];
        let mut ordered: Vec<&Value> = devices.to_vec();
        ordered.sort_by_key(|d| ORDER.iter().position(|id| d["id"] == *id).unwrap_or(ORDER.len()));
        sync(
            &self.mapping,
            ordered
                .iter()
                .filter_map(|d| {
                    let id = d["id"].as_str()?;
                    let natural = match id {
                        "keyboard" => GameSource::Keyboard,
                        "mouse" => GameSource::Mouse,
                        "mousepad" => GameSource::Mousepad,
                        _ => GameSource::ChromaLink,
                    };
                    let current = config.game_mapping.get(id).copied().unwrap_or(natural);
                    let name = match id {
                        "keyboard" => "Keyboard",
                        "mouse" => "Mouse",
                        "mousepad" => "Mousepad",
                        "case" => "Case strips",
                        _ => "ARGB fans",
                    };
                    Some(MappingRow {
                        id: id.into(),
                        name: name.into(),
                        detail: d["name"].as_str().map(friendly).unwrap_or_default().into(),
                        choice: MAPPING_SOURCES.iter().position(|s| *s == current).unwrap_or(0) as i32,
                    })
                })
                .collect(),
        );

        // Keep details to one line; the card can't grow for wrapped text.
        let dll = |name: &str, label: &str, folder: &str| {
            let state = status["sdk_dlls"][name].as_str();
            CheckRow {
                label: label.into(),
                ok: state == Some("openchroma"),
                detail: match state {
                    Some("openchroma") => format!("OpenChroma's {name} is in {folder}"),
                    Some("razer") => "Razer's DLL is installed; run `openchroma sdk install`".to_string(),
                    Some(_) => format!("{name} is missing; run `openchroma sdk install`"),
                    None => "Unknown; update the service".to_string(),
                }
                .into(),
            }
        };
        let port = status["sdk_port"].as_u64().unwrap_or(54235);
        let rest = CheckRow {
            label: "Web and Unity games".into(),
            ok: status["sdk_port_bound"] == true,
            detail: if status["sdk_port_bound"] == true {
                format!("REST API listening on port {port}")
            } else {
                format!("Port {port} is taken (Razer's SDK service?)")
            }
            .into(),
        };
        sync(
            &self.checks,
            vec![
                dll("RzChromaSDK64.dll", "Native games (64-bit)", "System32"),
                dll("RzChromaSDK.dll", "Native games (32-bit)", "SysWOW64"),
                rest,
            ],
        );

        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let live: Vec<&str> = sessions.iter().filter_map(|s| s["title"].as_str()).collect();
        sync(
            &self.apps,
            status["apps"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|a| {
                    let title = a["title"].as_str().unwrap_or("?");
                    let kind = if a["client"] == "rest" { "REST API" } else { "Native SDK" };
                    let when = if live.contains(&title) {
                        "connected now".to_string()
                    } else {
                        ago(now.saturating_sub(a["last_seen"].as_u64().unwrap_or(0)))
                    };
                    AppRow { title: title.into(), detail: format!("{kind} · {when}").into(), allowed: a["allowed"] != false }
                })
                .collect(),
        );
    }
}

/// "Razer BlackWidow Chroma V2" -> "BlackWidow Chroma V2".
fn friendly(name: &str) -> String {
    name.strip_prefix("Razer ").unwrap_or(name).to_string()
}

fn join_and(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [init @ .., last] => format!("{} and {last}", init.join(", ")),
    }
}

fn ago(secs: u64) -> String {
    match secs {
        0..=89 => "just now".into(),
        90..=3599 => format!("{} min ago", secs / 60),
        3600..=86399 => format!("{} h ago", secs / 3600),
        _ => format!("{} days ago", secs / 86400),
    }
}
