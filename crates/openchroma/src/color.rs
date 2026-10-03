pub use razer_hid::Rgb;
use serde::{de, Deserialize, Deserializer, Serialize, Serializer};

/// Colors in config files and the control API are "#RRGGBB" strings.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Color(pub Rgb);

impl Color {
    pub fn hex(self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.0.r, self.0.g, self.0.b)
    }

    pub fn parse(s: &str) -> Option<Color> {
        let s = s.strip_prefix('#').unwrap_or(s);
        if s.len() != 6 {
            return None;
        }
        let v = u32::from_str_radix(s, 16).ok()?;
        Some(Color(Rgb::new((v >> 16) as u8, (v >> 8) as u8, v as u8)))
    }
}

impl Serialize for Color {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.hex())
    }
}

impl<'de> Deserialize<'de> for Color {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Color::parse(&s).ok_or_else(|| de::Error::custom(format!("invalid color {s:?}, expected #RRGGBB")))
    }
}

pub fn from_colorref(c: chroma_proto::ColorRef) -> Rgb {
    let (r, g, b) = chroma_proto::colorref_rgb(c);
    Rgb::new(r, g, b)
}

pub fn lerp(a: Rgb, b: Rgb, t: f32) -> Rgb {
    let t = t.clamp(0.0, 1.0);
    let mix = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Rgb::new(mix(a.r, b.r), mix(a.g, b.g), mix(a.b, b.b))
}

pub fn scale(c: Rgb, k: f32) -> Rgb {
    let k = k.clamp(0.0, 1.0);
    Rgb::new((c.r as f32 * k).round() as u8, (c.g as f32 * k).round() as u8, (c.b as f32 * k).round() as u8)
}

/// Hue in turns (0..1), full saturation and value.
pub fn hue(h: f32) -> Rgb {
    let h = h.rem_euclid(1.0) * 6.0;
    let x = 1.0 - ((h % 2.0) - 1.0).abs();
    let (r, g, b) = match h as u32 {
        0 => (1.0, x, 0.0),
        1 => (x, 1.0, 0.0),
        2 => (0.0, 1.0, x),
        3 => (0.0, x, 1.0),
        4 => (x, 0.0, 1.0),
        _ => (1.0, 0.0, x),
    };
    Rgb::new((r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8)
}

/// Sample a looping gradient through `stops` at position `t` (0..1).
pub fn gradient(stops: &[Rgb], t: f32, wrap: bool) -> Rgb {
    match stops.len() {
        0 => Rgb::BLACK,
        1 => stops[0],
        n => {
            let segments = if wrap { n } else { n - 1 };
            let t = if wrap { t.rem_euclid(1.0) } else { t.clamp(0.0, 1.0) };
            let pos = t * segments as f32;
            let i = (pos as usize).min(segments - 1);
            lerp(stops[i], stops[(i + 1) % n], pos - i as f32)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_roundtrip() {
        let c = Color::parse("#12aBef").unwrap();
        assert_eq!(c.0, Rgb::new(0x12, 0xab, 0xef));
        assert_eq!(c.hex(), "#12abef");
        assert!(Color::parse("#123").is_none());
    }

    #[test]
    fn hue_primaries() {
        assert_eq!(hue(0.0), Rgb::new(255, 0, 0));
        assert_eq!(hue(1.0 / 3.0), Rgb::new(0, 255, 0));
        assert_eq!(hue(2.0 / 3.0), Rgb::new(0, 0, 255));
    }

    #[test]
    fn gradient_endpoints() {
        let stops = [Rgb::new(0, 0, 0), Rgb::new(200, 100, 0)];
        assert_eq!(gradient(&stops, 0.0, false), stops[0]);
        assert_eq!(gradient(&stops, 1.0, false), stops[1]);
        assert_eq!(gradient(&stops, 0.5, false), Rgb::new(100, 50, 0));
    }
}
