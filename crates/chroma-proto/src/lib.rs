//! Types shared by the OpenChroma service and its RzChromaSDK DLL replacement.
//!
//! Chroma SDK clients (native DLL callers and REST callers alike) describe
//! lighting per device *category* on fixed virtual canvases. Everything here is
//! category-level; mapping canvases onto physical LEDs happens in the service.

use serde::{Deserialize, Serialize};

/// Named pipe the DLL uses to reach the service.
pub const PIPE_NAME: &str = r"\\.\pipe\openchroma-sdk";

/// Access the DLL requests when opening the pipe, and all the service grants
/// to ordinary users: FILE_GENERIC_READ | FILE_WRITE_DATA. Plain
/// GENERIC_WRITE would include FILE_CREATE_PIPE_INSTANCE, which would let any
/// process add instances of the pipe and pose as the service.
pub const PIPE_CLIENT_ACCESS: u32 = 0x0012_008B;

/// Chroma SDK colors are Win32 COLORREF values: 0x00BBGGRR.
pub type ColorRef = u32;

pub fn colorref_rgb(c: ColorRef) -> (u8, u8, u8) {
    (c as u8, (c >> 8) as u8, (c >> 16) as u8)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Category {
    Keyboard,
    Mouse,
    Mousepad,
    Headset,
    Keypad,
    ChromaLink,
}

impl Category {
    pub const ALL: [Category; 6] =
        [Category::Keyboard, Category::Mouse, Category::Mousepad, Category::Headset, Category::Keypad, Category::ChromaLink];

    /// Canvas size (rows, cols) for `SdkEffect::Custom` on this category.
    pub fn dims(self) -> (usize, usize) {
        match self {
            Category::Keyboard => (6, 22),
            Category::Mouse => (9, 7),
            Category::Mousepad => (1, 20),
            Category::Headset => (1, 5),
            Category::Keypad => (4, 5),
            Category::ChromaLink => (1, 5),
        }
    }

    #[allow(clippy::len_without_is_empty)] // a canvas is never empty
    pub fn len(self) -> usize {
        let (r, c) = self.dims();
        r * c
    }

    /// Path segment used by the REST API.
    pub fn rest_name(self) -> &'static str {
        match self {
            Category::Keyboard => "keyboard",
            Category::Mouse => "mouse",
            Category::Mousepad => "mousepad",
            Category::Headset => "headset",
            Category::Keypad => "keypad",
            Category::ChromaLink => "chromalink",
        }
    }

    pub fn from_rest_name(s: &str) -> Option<Self> {
        Category::ALL.into_iter().find(|c| c.rest_name() == s)
    }
}

/// A lighting state requested by an SDK client for one category.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SdkEffect {
    None,
    Static {
        color: ColorRef,
    },
    /// Row-major colors on the category canvas (`Category::len()` entries).
    Custom {
        colors: Vec<ColorRef>,
    },
    Breathing {
        color1: ColorRef,
        color2: Option<ColorRef>,
        random: bool,
    },
    Spectrum,
    Wave {
        reverse: bool,
    },
}

impl SdkEffect {
    /// Build a canvas-sized custom effect, padding or truncating `colors`.
    pub fn custom(category: Category, mut colors: Vec<ColorRef>) -> Self {
        colors.resize(category.len(), 0);
        SdkEffect::Custom { colors }
    }

    /// CHROMA_CUSTOM_KEY: a key color with bit 24 set overrides the base color.
    pub fn custom_key(base: &[ColorRef], keys: &[ColorRef]) -> Self {
        let colors = base
            .iter()
            .zip(keys.iter().chain(std::iter::repeat(&0)))
            .map(|(&c, &k)| if k & 0x0100_0000 != 0 { k & 0x00FF_FFFF } else { c & 0x00FF_FFFF })
            .collect();
        SdkEffect::custom(Category::Keyboard, colors)
    }

    /// Mousepad CHROMA_CUSTOM (v1) has 15 LEDs; stretch onto the 20-LED
    /// canvas rather than leaving the tail dark.
    pub fn mousepad_v1(leds: &[ColorRef]) -> Self {
        let n = Category::Mousepad.len();
        let colors = match leds.len() {
            0 => vec![0; n],
            len => (0..n).map(|i| leds[i * len / n]).collect(),
        };
        SdkEffect::Custom { colors }
    }

    /// Mouse CHROMA_CUSTOM (v1) addresses up to 30 LEDs by `Mouse::RZLED`
    /// index; place them on the 9x7 CUSTOM2 grid.
    pub fn mouse_v1(leds: &[ColorRef]) -> Self {
        let (_, cols) = Category::Mouse.dims();
        let mut grid = vec![0; Category::Mouse.len()];
        let mut put = |row: usize, col: usize, idx: usize| {
            if let Some(&c) = leds.get(idx) {
                grid[row * cols + col] = c;
            }
        };
        put(2, 3, 1); // scroll wheel
        put(7, 3, 2); // logo
        put(4, 3, 3); // backlight
        for i in 0..7 {
            put(i + 1, 0, 4 + i); // left side strip 1-7
            put(i + 1, 6, 11 + i); // right side strip 8-14
        }
        SdkEffect::Custom { colors: grid }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AppInfo {
    pub title: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub contact: String,
    #[serde(default)]
    pub category: u32,
}

/// Messages from the DLL to the service, one JSON object per line.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "msg", rename_all = "snake_case")]
pub enum ClientMsg {
    Hello {
        app: AppInfo,
        pid: u32,
        exe: String,
    },
    Effect {
        category: Category,
        effect: SdkEffect,
    },
    /// Sent when idle so a dead service is noticed and the DLL reconnects.
    Ping,
}

/// The service's single reply to `ClientMsg::Hello`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Welcome {
    /// Categories that currently have at least one physical device behind them.
    pub categories: Vec<Category>,
}

/// Chroma SDK device GUIDs accepted by `CreateEffect` / `QueryDevice`.
pub mod guids {
    use super::Category;

    pub type Guid = (u32, u16, u16, [u8; 8]);

    pub const KNOWN: &[(Guid, Category, &str)] = &[
        ((0x2ea1bb63, 0xca28, 0x428d, [0x9f, 0x06, 0x19, 0x6b, 0x88, 0x33, 0x0b, 0xbb]), Category::Keyboard, "BLACKWIDOW_CHROMA"),
        ((0xed1c1b82, 0xbfbe, 0x418f, [0xb4, 0x9d, 0xd0, 0x3f, 0x05, 0xb1, 0x49, 0xdf]), Category::Keyboard, "BLACKWIDOW_CHROMA_TE"),
        ((0x18c5ad9b, 0x4326, 0x4828, [0x92, 0xc4, 0x26, 0x69, 0xa6, 0x6d, 0x22, 0x83]), Category::Keyboard, "DEATHSTALKER_CHROMA"),
        ((0x872ab2a9, 0x7959, 0x4478, [0x9f, 0xed, 0x15, 0xf6, 0x18, 0x6e, 0x72, 0xe4]), Category::Keyboard, "OVERWATCH_KEYBOARD"),
        ((0x5af60076, 0xade9, 0x43d4, [0xb5, 0x74, 0x52, 0x59, 0x92, 0x93, 0xb5, 0x54]), Category::Keyboard, "BLACKWIDOW_X_CHROMA"),
        ((0x2d84dd51, 0x3290, 0x4aac, [0x9a, 0x89, 0xd8, 0xaf, 0xde, 0x38, 0xb5, 0x7c]), Category::Keyboard, "BLACKWIDOW_X_TE_CHROMA"),
        ((0x803378c1, 0xcc48, 0x4970, [0x85, 0x39, 0xd8, 0x28, 0xcc, 0x1d, 0x42, 0x0a]), Category::Keyboard, "ORNATA_CHROMA"),
        ((0xc83bdfe8, 0xe7fc, 0x40e0, [0x99, 0xdb, 0x87, 0x2e, 0x23, 0xf1, 0x98, 0x91]), Category::Keyboard, "BLADE_STEALTH"),
        ((0xf2bedfaf, 0xa0fe, 0x4651, [0x9d, 0x41, 0xb6, 0xce, 0x60, 0x3a, 0x3d, 0xdd]), Category::Keyboard, "BLADE"),
        ((0xa73ac338, 0xf0e5, 0x4bf7, [0x91, 0xae, 0xdd, 0x1f, 0x7e, 0x17, 0x37, 0xa5]), Category::Keyboard, "BLADE_PRO"),
        ((0xf85e7473, 0x8f03, 0x45b6, [0xa1, 0x6e, 0xce, 0x26, 0xcb, 0x8d, 0x24, 0x41]), Category::Keyboard, "HUNTSMAN"),
        ((0x16bb5abd, 0xc1cd, 0x4cb3, [0xbd, 0xf7, 0x62, 0x43, 0x87, 0x48, 0xbd, 0x98]), Category::Keyboard, "BLACKWIDOW_ELITE"),
        ((0xaec50d91, 0xb1f1, 0x452f, [0x8e, 0x16, 0x7b, 0x73, 0xf3, 0x76, 0xfd, 0xf3]), Category::Mouse, "DEATHADDER_CHROMA"),
        ((0x7ec00450, 0xe0ee, 0x4289, [0x89, 0xd5, 0x0d, 0x87, 0x9c, 0x19, 0x06, 0x1a]), Category::Mouse, "MAMBA_CHROMA_TE"),
        ((0xff8a5929, 0x4512, 0x4257, [0x8d, 0x59, 0xc6, 0x47, 0xbf, 0x99, 0x35, 0xd0]), Category::Mouse, "DIAMONDBACK_CHROMA"),
        ((0xd527cbdc, 0xeb0a, 0x483a, [0x9e, 0x89, 0x66, 0xd5, 0x04, 0x63, 0xec, 0x6c]), Category::Mouse, "MAMBA_CHROMA"),
        ((0xd714c50b, 0x7158, 0x4368, [0xb9, 0x9c, 0x60, 0x1a, 0xcb, 0x98, 0x5e, 0x98]), Category::Mouse, "NAGA_EPIC_CHROMA"),
        ((0xf1876328, 0x6ca4, 0x46ae, [0xbe, 0x04, 0xbe, 0x81, 0x2b, 0x41, 0x44, 0x33]), Category::Mouse, "NAGA_CHROMA"),
        ((0x52c15681, 0x4ece, 0x4dd9, [0x8a, 0x52, 0xa1, 0x41, 0x84, 0x59, 0xeb, 0x34]), Category::Mouse, "OROCHI_CHROMA"),
        ((0x195d70f5, 0xf285, 0x4cff, [0x99, 0xf2, 0xb8, 0xc0, 0xe9, 0x65, 0x8d, 0xb4]), Category::Mouse, "NAGA_HEX_CHROMA"),
        ((0x77834867, 0x3237, 0x4a9f, [0xad, 0x77, 0x4a, 0x46, 0xc4, 0x18, 0x30, 0x03]), Category::Mouse, "DEATHADDER_ELITE_CHROMA"),
        ((0xcd1e09a5, 0xd5e6, 0x4a6c, [0xa9, 0x3b, 0xe6, 0xd9, 0xbf, 0x1d, 0x20, 0x92]), Category::Headset, "KRAKEN71_CHROMA"),
        ((0xdf3164d7, 0x5408, 0x4a0e, [0x8a, 0x7f, 0xa7, 0x41, 0x2f, 0x26, 0xbe, 0xbf]), Category::Headset, "MANOWAR_CHROMA"),
        ((0x7fb8a36e, 0x9e74, 0x4bb3, [0x8c, 0x86, 0xca, 0xc7, 0xf7, 0x89, 0x1e, 0xbd]), Category::Headset, "KRAKEN71_REFRESH_CHROMA"),
        ((0xfb357780, 0x4617, 0x43a7, [0x96, 0x0f, 0xd1, 0x19, 0x0e, 0xd5, 0x48, 0x06]), Category::Headset, "KRAKEN_KITTY"),
        ((0x80f95a94, 0x73d2, 0x48ca, [0xae, 0x9a, 0x09, 0x86, 0x78, 0x9a, 0x9a, 0xf2]), Category::Mousepad, "FIREFLY_CHROMA"),
        ((0x00f0545c, 0xe180, 0x4ad1, [0x8e, 0x8a, 0x41, 0x90, 0x61, 0xce, 0x50, 0x5e]), Category::Keypad, "TARTARUS_CHROMA"),
        ((0x9d24b0ab, 0x0162, 0x466c, [0x96, 0x40, 0x7a, 0x92, 0x4a, 0xa4, 0xd9, 0xfd]), Category::Keypad, "ORBWEAVER_CHROMA"),
        ((0x35f6f18d, 0x1ae5, 0x436c, [0xa5, 0x75, 0xab, 0x44, 0xa1, 0x27, 0x90, 0x3a]), Category::Keyboard, "LENOVO_Y900"),
        ((0x47db1fa7, 0x6b9b, 0x4ee6, [0xb6, 0xf4, 0x40, 0x71, 0xa3, 0xb2, 0x05, 0x3b]), Category::ChromaLink, "LENOVO_Y27"),
        ((0x0201203b, 0x62f3, 0x4c50, [0x83, 0xdd, 0x59, 0x8b, 0xab, 0xd2, 0x08, 0xe0]), Category::ChromaLink, "CORE_CHROMA"),
        ((0xbb2e9c9b, 0xb0d2, 0x461a, [0xba, 0x52, 0x23, 0x0b, 0x5d, 0x6c, 0x36, 0x09]), Category::ChromaLink, "CHROMABOX"),
        ((0x45b308f2, 0xcd44, 0x4594, [0x83, 0x75, 0x4d, 0x59, 0x45, 0xad, 0x88, 0x0e]), Category::ChromaLink, "NOMMO_CHROMA"),
        ((0x3017280b, 0xd7f9, 0x4d7b, [0x93, 0x0e, 0x7b, 0x47, 0x18, 0x1b, 0x46, 0xb5]), Category::ChromaLink, "NOMMO_CHROMA_PRO"),
    ];

    pub fn category(g: Guid) -> Option<Category> {
        KNOWN.iter().find(|(k, _, _)| *k == g).map(|(_, c, _)| *c)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn custom_key_overrides_flagged_keys_only() {
        let base = vec![0x0000FF; 132];
        let mut keys = vec![0; 132];
        keys[5] = 0x0100FF00;
        keys[6] = 0x00FF0000; // no flag: ignored
        let SdkEffect::Custom { colors } = SdkEffect::custom_key(&base, &keys) else { panic!() };
        assert_eq!(colors[5], 0x00FF00);
        assert_eq!(colors[6], 0x0000FF);
    }

    #[test]
    fn effect_json_roundtrip() {
        let m = ClientMsg::Effect { category: Category::ChromaLink, effect: SdkEffect::Static { color: 0xFF } };
        let s = serde_json::to_string(&m).unwrap();
        assert_eq!(serde_json::from_str::<ClientMsg>(&s).unwrap(), m);
    }

    #[test]
    fn colorref_is_bgr() {
        assert_eq!(colorref_rgb(0x00336699), (0x99, 0x66, 0x33));
    }
}
