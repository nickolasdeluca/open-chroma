use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::color::Color;
use crate::effects::Effect;

/// Port of the Chroma SDK REST API. Clients hard-code 54235; the
/// `OPENCHROMA_SDK_PORT` override exists only for testing next to Synapse.
pub fn sdk_port() -> u16 {
    std::env::var("OPENCHROMA_SDK_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(54235)
}
pub const UI_PORT: u16 = 54240;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Global brightness, 0-100.
    pub brightness: u8,
    pub fps: u32,
    pub active_profile: String,
    /// Let games drive the lights through the Chroma SDK.
    pub sdk_enabled: bool,
    pub profiles: Vec<Profile>,
    /// The six channels of the Chroma Addressable RGB Controller.
    pub argb_channels: Vec<ArgbChannel>,
    /// The addressable headers of an ASUS Aura motherboard, in board order.
    /// The board cannot tell how many LEDs are plugged in, so headers
    /// missing here stay dark.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub motherboard_headers: Vec<ArgbChannel>,
    /// Which game canvas each device (by id) shows while a game has control.
    /// Devices not listed show their natural canvas.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub game_mapping: BTreeMap<String, GameSource>,
}

/// A Chroma SDK canvas a device can show during games, or none (the device
/// keeps the profile even while a game runs).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GameSource {
    Keyboard,
    Mouse,
    Mousepad,
    Headset,
    Keypad,
    ChromaLink,
    None,
}

impl GameSource {
    pub fn category(self) -> Option<chroma_proto::Category> {
        use chroma_proto::Category as C;
        Some(match self {
            GameSource::Keyboard => C::Keyboard,
            GameSource::Mouse => C::Mouse,
            GameSource::Mousepad => C::Mousepad,
            GameSource::Headset => C::Headset,
            GameSource::Keypad => C::Keypad,
            GameSource::ChromaLink => C::ChromaLink,
            GameSource::None => return None,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Profile {
    pub name: String,
    pub effect: Effect,
    /// Per-target overrides keyed by device id ("keyboard", "mouse",
    /// "mousepad", "case", "argb", "motherboard") or zone ("argb:4",
    /// "case:2", "motherboard:1").
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub overrides: BTreeMap<String, Effect>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ArgbChannel {
    pub name: String,
    /// LEDs connected to the channel, 0-80 (0-120 on a motherboard header).
    /// 0 disables it.
    pub leds: u8,
    /// Optional split into fans (LED counts), used for naming and so fan-
    /// shaped effects treat each fan as a ring.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fans: Vec<u8>,
    /// Which of the 5 Chroma Link LEDs drives this channel during games.
    /// Defaults to spreading channels over LEDs 1-4.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chroma_link_led: Option<u8>,
}

fn c(hex: &str) -> Color {
    Color::parse(hex).expect("valid built-in color")
}

impl Default for Config {
    fn default() -> Self {
        let profile = |name: &str, effect| Profile { name: name.into(), effect, overrides: BTreeMap::new() };
        Config {
            brightness: 100,
            fps: 30,
            active_profile: "Rainbow Wave".into(),
            sdk_enabled: true,
            profiles: vec![
                profile("Rainbow Wave", Effect::Wave { period: 4.0, repeat: 1.0, reverse: false }),
                profile("Spectrum", Effect::Spectrum { period: 12.0 }),
                profile("Ocean", Effect::ColorWave { colors: vec![c("#0010ff"), c("#00c8ff"), c("#00ffa0")], period: 6.0, reverse: false }),
                profile("Ember", Effect::Breathing { colors: vec![c("#ff2000"), c("#ff7000")], period: 5.0 }),
                profile("Starlight", Effect::Starlight { colors: vec![c("#ffffff"), c("#40a0ff")], background: c("#000010"), period: 3.0 }),
                profile("Razer Green", Effect::Static { color: c("#44d62c") }),
                profile("Off", Effect::Off),
            ],
            argb_channels: (1..=6)
                .map(|i| ArgbChannel { name: format!("Channel {i}"), leds: 0, fans: vec![], chroma_link_led: None })
                .collect(),
            motherboard_headers: Vec::new(),
            game_mapping: BTreeMap::new(),
        }
    }
}

impl Config {
    /// Machine-wide, so the service (running as SYSTEM) and the user's tools
    /// share one config.
    pub fn dir() -> PathBuf {
        std::env::var_os("ProgramData").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\ProgramData")).join("OpenChroma")
    }

    pub fn path() -> PathBuf {
        Self::dir().join("config.json")
    }

    /// Where versions before the service lived kept the config.
    fn legacy_path() -> Option<PathBuf> {
        Some(PathBuf::from(std::env::var_os("APPDATA")?).join(r"OpenChroma\config.json"))
    }

    /// Load the config, creating it on first run from (in order) an older
    /// per-user config or the defaults plus Synapse's ARGB channel layout,
    /// which cannot be read back from the controller.
    pub fn load_or_create() -> Config {
        let path = Self::path();
        if !path.exists() {
            if let Some(legacy) = Self::legacy_path().filter(|p| p.exists()) {
                let copied = fs::create_dir_all(Self::dir()).and_then(|_| fs::copy(&legacy, &path));
                match copied {
                    Ok(_) => log::info!("migrated config from {}", legacy.display()),
                    Err(e) => log::warn!("could not migrate {}: {e}", legacy.display()),
                }
            }
        }
        if let Ok(text) = fs::read_to_string(&path) {
            return match serde_json::from_str::<Config>(&text) {
                Ok(cfg) => cfg.normalized(),
                Err(e) => {
                    log::error!("{} is invalid ({e}); using defaults without overwriting it", path.display());
                    Config::default()
                }
            };
        }
        let mut cfg = Config::default();
        if let Some(channels) = crate::synapse::import_argb_channels() {
            log::info!("imported ARGB channel layout from Synapse");
            cfg.argb_channels = channels;
        }
        if let Err(e) = cfg.save() {
            log::warn!("could not write {}: {e}", path.display());
        }
        cfg
    }

    pub fn save(&self) -> std::io::Result<()> {
        fs::create_dir_all(Self::dir())?;
        let tmp = Self::path().with_extension("json.tmp");
        fs::write(&tmp, serde_json::to_string_pretty(self).expect("config serializes"))?;
        fs::rename(tmp, Self::path())
    }

    /// Clamp values into range so a hand-edited file cannot break rendering.
    pub fn normalized(mut self) -> Config {
        self.brightness = self.brightness.min(100);
        self.fps = self.fps.clamp(1, 60);
        self.argb_channels.resize_with(6, || ArgbChannel { name: "Channel".into(), leds: 0, fans: vec![], chroma_link_led: None });
        for ch in &mut self.argb_channels {
            ch.leds = ch.leds.min(80);
        }
        for h in &mut self.motherboard_headers {
            h.leds = h.leds.min(asus_aura::MAX_ARGB_LEDS as u8);
        }
        if self.profiles.is_empty() {
            self.profiles = Config::default().profiles;
        }
        self
    }

    pub fn profile(&self) -> &Profile {
        self.profiles.iter().find(|p| p.name == self.active_profile).unwrap_or(&self.profiles[0])
    }
}
