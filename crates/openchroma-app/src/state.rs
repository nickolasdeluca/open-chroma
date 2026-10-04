//! The app's working copy of the service config, and every edit the UI can
//! make to it. Edits change the local copy immediately (so the UI responds at
//! once) and are then sent to the service by the writer thread.

use std::time::{Duration, Instant};

use openchroma::color::Color;
use openchroma::config::{ArgbChannel, Config, GameSource, Profile};
use openchroma::effects::Effect;

/// After a local edit, ignore the service's copy of the config for this long,
/// so a poll that raced the write can't undo what the user just did.
const LOCAL_EDIT_GRACE: Duration = Duration::from_millis(1500);

/// Effects in the order the editor shows them.
pub const EFFECTS: [&str; 8] = ["static", "breathing", "spectrum", "wave", "gradient", "color_wave", "starlight", "off"];
pub const EFFECT_LABELS: [&str; 8] = ["Static", "Breathing", "Spectrum", "Rainbow wave", "Gradient", "Color wave", "Starlight", "Off"];

/// Options for a zone override, in the order of `ZONE_OPTIONS`.
pub const ZONE_OPTIONS: [&str; 7] = ["Profile effect", "Off", "Static", "Breathing", "Spectrum", "Rainbow wave", "Custom (config file)"];

/// Game canvases a device can show, in the order of `MAPPING_OPTIONS`.
pub const MAPPING_OPTIONS: [&str; 7] = ["Keyboard", "Mouse", "Mousepad", "Chroma Link", "Headset", "Keypad", "Nothing"];
pub const MAPPING_SOURCES: [GameSource; 7] = [
    GameSource::Keyboard,
    GameSource::Mouse,
    GameSource::Mousepad,
    GameSource::ChromaLink,
    GameSource::Headset,
    GameSource::Keypad,
    GameSource::None,
];

/// Slowest and fastest cycle the speed slider maps to, in seconds.
const SLOWEST: f32 = 20.0;
const FASTEST: f32 = 0.5;

fn hex(s: &str) -> Color {
    Color::parse(s).expect("valid built-in color")
}

pub struct State {
    pub config: Option<Config>,
    local_edit: Option<Instant>,
    /// Name of the profile open in the editor.
    pub editing: String,
    /// Unsaved ARGB channel edits; `None` = showing the saved channels.
    pub port_draft: Option<Vec<ArgbChannel>>,
}

impl State {
    pub fn new() -> State {
        State { config: None, local_edit: None, editing: String::new(), port_draft: None }
    }

    /// Take the service's config unless a local edit is still settling.
    pub fn receive(&mut self, config: Config) {
        if self.local_edit.is_some_and(|t| t.elapsed() < LOCAL_EDIT_GRACE) {
            return;
        }
        if self.editing.is_empty() || !config.profiles.iter().any(|p| p.name == self.editing) {
            self.editing = config.active_profile.clone();
        }
        self.config = Some(config);
    }

    /// Apply an edit; returns the config to send, if anything changed.
    fn edit(&mut self, f: impl FnOnce(&mut Config) -> bool) -> Option<Config> {
        let config = self.config.as_mut()?;
        if !f(config) {
            return None;
        }
        self.local_edit = Some(Instant::now());
        Some(config.clone())
    }

    fn edit_profile(&mut self, f: impl FnOnce(&mut Profile) -> bool) -> Option<Config> {
        let name = self.editing.clone();
        self.edit(|c| c.profiles.iter_mut().find(|p| p.name == name).is_some_and(f))
    }

    pub fn profile(&self) -> Option<&Profile> {
        self.config.as_ref()?.profiles.iter().find(|p| p.name == self.editing)
    }

    // ------------------------------------------------------------- settings

    pub fn activate(&mut self, name: &str) -> Option<Config> {
        self.edit(|c| {
            let changed = c.active_profile != name;
            c.active_profile = name.to_string();
            changed
        })
    }

    pub fn set_brightness(&mut self, v: f32) -> Option<Config> {
        let b = (v.clamp(0.0, 1.0) * 100.0).round() as u8;
        self.edit(|c| std::mem::replace(&mut c.brightness, b) != b)
    }

    pub fn set_games_allowed(&mut self, v: bool) -> Option<Config> {
        self.edit(|c| std::mem::replace(&mut c.sdk_enabled, v) != v)
    }

    pub fn set_mapping(&mut self, device: &str, choice: usize) -> Option<Config> {
        let source = *MAPPING_SOURCES.get(choice)?;
        self.edit(|c| c.game_mapping.insert(device.to_string(), source) != Some(source))
    }

    // ------------------------------------------------------------- profiles

    fn unique_name(config: &Config, base: &str) -> String {
        (1..)
            .map(|n| if n == 1 { base.to_string() } else { format!("{base} {n}") })
            .find(|name| !config.profiles.iter().any(|p| &p.name == name))
            .expect("some name is free")
    }

    pub fn new_profile(&mut self) -> Option<Config> {
        let name = Self::unique_name(self.config.as_ref()?, "New profile");
        self.editing = name.clone();
        self.edit(|c| {
            c.profiles.push(Profile::new(name, Effect::Static { color: hex("#ffffff") }));
            true
        })
    }

    pub fn duplicate(&mut self) -> Option<Config> {
        let mut copy = self.profile()?.clone();
        copy.name = Self::unique_name(self.config.as_ref()?, &format!("{} copy", copy.name));
        self.editing = copy.name.clone();
        self.edit(|c| {
            c.profiles.push(copy);
            true
        })
    }

    pub fn delete(&mut self) -> Option<Config> {
        let name = self.editing.clone();
        self.edit(|c| {
            if c.profiles.len() < 2 {
                return false;
            }
            c.profiles.retain(|p| p.name != name);
            if c.active_profile == name {
                c.active_profile = c.profiles[0].name.clone();
            }
            true
        })
    }

    /// Rename the profile being edited. Empty or duplicate names are ignored
    /// until the user types a valid one.
    pub fn rename(&mut self, new: &str) -> Option<Config> {
        let new = new.trim().to_string();
        let old = self.editing.clone();
        let config = self.config.as_ref()?;
        if new.is_empty() || new == old || config.profiles.iter().any(|p| p.name == new) {
            return None;
        }
        self.editing = new.clone();
        self.edit(|c| {
            if c.active_profile == old {
                c.active_profile = new.clone();
            }
            c.profiles.iter_mut().filter(|p| p.name == old).for_each(|p| p.name = new.clone());
            true
        })
    }

    // ------------------------------------------------------------- effect

    pub fn set_effect(&mut self, index: usize) -> Option<Config> {
        let kind = *EFFECTS.get(index)?;
        self.edit_profile(|p| {
            let colors = effect_colors(&p.effect).unwrap_or_else(|| vec![hex("#00c8ff"), hex("#7a3cff")]);
            let period = effect_period(&p.effect).unwrap_or(4.0);
            let reverse = effect_reverse(&p.effect).unwrap_or(false);
            let new = match kind {
                "static" => Effect::Static { color: colors[0] },
                "breathing" => Effect::Breathing { colors, period },
                "spectrum" => Effect::Spectrum { period: period.max(4.0) },
                "wave" => Effect::Wave { period, repeat: 1.0, reverse },
                "gradient" => Effect::Gradient { colors },
                "color_wave" => Effect::ColorWave { colors, period, reverse },
                "starlight" => Effect::Starlight { colors, background: hex("#000010"), period },
                _ => Effect::Off,
            };
            let changed = new != p.effect;
            p.effect = new;
            changed
        })
    }

    pub fn set_color(&mut self, index: usize, color: Color) -> Option<Config> {
        self.edit_profile(|p| match &mut p.effect {
            Effect::Static { color: c } if index == 0 => {
                *c = color;
                true
            }
            Effect::Breathing { colors, .. }
            | Effect::Gradient { colors }
            | Effect::ColorWave { colors, .. }
            | Effect::Starlight { colors, .. } => colors.get_mut(index).map(|c| *c = color).is_some(),
            _ => false,
        })
    }

    pub fn add_color(&mut self) -> Option<Config> {
        self.edit_profile(|p| match &mut p.effect {
            Effect::Breathing { colors, .. }
            | Effect::Gradient { colors }
            | Effect::ColorWave { colors, .. }
            | Effect::Starlight { colors, .. }
                if colors.len() < 8 =>
            {
                colors.push(*colors.last().unwrap_or(&hex("#ffffff")));
                true
            }
            _ => false,
        })
    }

    pub fn remove_color(&mut self, index: usize) -> Option<Config> {
        self.edit_profile(|p| match &mut p.effect {
            Effect::Breathing { colors, .. }
            | Effect::Gradient { colors }
            | Effect::ColorWave { colors, .. }
            | Effect::Starlight { colors, .. }
                if colors.len() > 1 && index < colors.len() =>
            {
                colors.remove(index);
                true
            }
            _ => false,
        })
    }

    pub fn set_background(&mut self, color: Color) -> Option<Config> {
        self.edit_profile(|p| match &mut p.effect {
            Effect::Starlight { background, .. } => {
                *background = color;
                true
            }
            _ => false,
        })
    }

    pub fn set_speed(&mut self, slider: f32) -> Option<Config> {
        let seconds = speed_to_period(slider);
        self.edit_profile(|p| match &mut p.effect {
            Effect::Breathing { period, .. }
            | Effect::Spectrum { period }
            | Effect::Wave { period, .. }
            | Effect::ColorWave { period, .. }
            | Effect::Starlight { period, .. } => {
                *period = seconds;
                true
            }
            _ => false,
        })
    }

    pub fn set_reverse(&mut self, v: bool) -> Option<Config> {
        self.edit_profile(|p| match &mut p.effect {
            Effect::Wave { reverse, .. } | Effect::ColorWave { reverse, .. } => std::mem::replace(reverse, v) != v,
            _ => false,
        })
    }

    pub fn set_zone(&mut self, key: &str, choice: usize) -> Option<Config> {
        let key = key.to_string();
        self.edit_profile(|p| {
            let colors = effect_colors(&p.effect).unwrap_or_else(|| vec![hex("#ffffff")]);
            let new = match choice {
                0 => None,
                1 => Some(Effect::Off),
                2 => Some(Effect::Static { color: colors[0] }),
                3 => Some(Effect::Breathing { colors, period: 4.0 }),
                4 => Some(Effect::Spectrum { period: 12.0 }),
                5 => Some(Effect::Wave { period: 4.0, repeat: 1.0, reverse: false }),
                _ => return false,
            };
            match new {
                Some(e) => p.overrides.insert(key, e.clone()) != Some(e),
                None => p.overrides.remove(&key).is_some(),
            }
        })
    }

    // ------------------------------------------------------------- ARGB ports

    pub fn ports(&self) -> Vec<ArgbChannel> {
        self.port_draft.clone().or_else(|| self.config.as_ref().map(|c| c.argb_channels.clone())).unwrap_or_default()
    }

    #[allow(clippy::too_many_arguments)]
    pub fn change_port(&mut self, index: usize, name: &str, is_fans: bool, fans: u8, fan_leds: u8, leds: u8, link: usize) {
        let mut ports = self.ports();
        let Some(port) = ports.get_mut(index) else { return };
        port.name = name.to_string();
        if is_fans {
            let fans = fans.clamp(1, 10);
            let per = fan_leds.clamp(1, 40);
            // The controller drives at most 80 LEDs per channel.
            let fit = (80 / per as usize).max(1).min(fans as usize);
            port.fans = vec![per; fit];
            port.leds = per * fit as u8;
        } else {
            port.fans.clear();
            port.leds = leds.clamp(1, 80);
        }
        port.chroma_link_led = link.checked_sub(1).map(|l| l.min(4) as u8);
        self.port_draft = Some(ports);
    }

    pub fn save_ports(&mut self) -> Option<Config> {
        let ports = self.port_draft.take()?;
        self.edit(|c| {
            c.argb_channels = ports;
            true
        })
    }

    pub fn discard_ports(&mut self) {
        self.port_draft = None;
    }
}

pub fn effect_colors(e: &Effect) -> Option<Vec<Color>> {
    match e {
        Effect::Static { color } => Some(vec![*color]),
        Effect::Breathing { colors, .. }
        | Effect::Gradient { colors }
        | Effect::ColorWave { colors, .. }
        | Effect::Starlight { colors, .. } => Some(colors.clone()).filter(|c| !c.is_empty()),
        _ => None,
    }
}

pub fn effect_period(e: &Effect) -> Option<f32> {
    match e {
        Effect::Breathing { period, .. }
        | Effect::Spectrum { period }
        | Effect::Wave { period, .. }
        | Effect::ColorWave { period, .. }
        | Effect::Starlight { period, .. } => Some(*period),
        _ => None,
    }
}

pub fn effect_reverse(e: &Effect) -> Option<bool> {
    match e {
        Effect::Wave { reverse, .. } | Effect::ColorWave { reverse, .. } => Some(*reverse),
        _ => None,
    }
}

pub fn effect_index(e: &Effect) -> usize {
    let kind = match e {
        Effect::Static { .. } => "static",
        Effect::Breathing { .. } => "breathing",
        Effect::Spectrum { .. } => "spectrum",
        Effect::Wave { .. } => "wave",
        Effect::Gradient { .. } => "gradient",
        Effect::ColorWave { .. } => "color_wave",
        Effect::Starlight { .. } => "starlight",
        Effect::Off => "off",
    };
    EFFECTS.iter().position(|k| *k == kind).unwrap_or(0)
}

pub fn zone_choice(e: Option<&Effect>) -> usize {
    match e {
        None => 0,
        Some(Effect::Off) => 1,
        Some(Effect::Static { .. }) => 2,
        Some(Effect::Breathing { .. }) => 3,
        Some(Effect::Spectrum { .. }) => 4,
        Some(Effect::Wave { .. }) => 5,
        Some(_) => 6,
    }
}

/// Slider position (0 = slow, 1 = fast) to seconds per cycle, logarithmic so
/// both ends get useful resolution.
pub fn speed_to_period(v: f32) -> f32 {
    let p = SLOWEST * (FASTEST / SLOWEST).powf(v.clamp(0.0, 1.0));
    (p * 10.0).round() / 10.0
}

pub fn period_to_speed(p: f32) -> f32 {
    ((p.clamp(FASTEST, SLOWEST) / SLOWEST).ln() / (FASTEST / SLOWEST).ln()).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> State {
        let mut s = State::new();
        s.receive(Config::default());
        s
    }

    #[test]
    fn speed_mapping_roundtrips() {
        for p in [0.5, 2.0, 4.0, 12.0, 20.0] {
            assert!((speed_to_period(period_to_speed(p)) - p).abs() < 0.1, "{p}");
        }
    }

    #[test]
    fn rename_ignores_invalid_names_and_follows_active() {
        let mut s = state();
        s.editing = "Ocean".into();
        assert!(s.rename("").is_none());
        assert!(s.rename("Ember").is_none());
        let active = s.config.as_ref().unwrap().active_profile.clone();
        s.activate("Ocean");
        let cfg = s.rename("Deep Ocean").unwrap();
        assert_eq!(cfg.active_profile, "Deep Ocean");
        assert!(cfg.profiles.iter().any(|p| p.name == "Deep Ocean"));
        assert_ne!(active, "Deep Ocean");
    }

    #[test]
    fn switching_effects_keeps_colors() {
        let mut s = state();
        s.editing = "Ocean".into();
        let cfg = s.set_effect(1).unwrap();
        let p = cfg.profiles.iter().find(|p| p.name == "Ocean").unwrap();
        assert!(matches!(&p.effect, Effect::Breathing { colors, .. } if colors.len() == 3));
    }

    #[test]
    fn fan_ports_stay_within_80_leds() {
        let mut s = state();
        s.change_port(3, "Fans", true, 10, 16, 0, 0);
        let p = &s.ports()[3];
        assert_eq!((p.fans.len(), p.leds), (5, 80));
        assert_eq!(p.chroma_link_led, None);
    }

    #[test]
    fn local_edits_win_over_a_racing_poll() {
        let mut s = state();
        s.set_brightness(0.4).unwrap();
        s.receive(Config::default());
        assert_eq!(s.config.as_ref().unwrap().brightness, 40);
    }
}
