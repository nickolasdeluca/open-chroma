//! Software lighting effects for user profiles.
//!
//! Effects are pure functions of time and LED position, rendered by the
//! service every frame. Doing it in software (instead of the devices' onboard
//! effects) keeps every device in sync and works the same on all hardware.

use razer_hid::Rgb;
use serde::{Deserialize, Serialize};

use crate::color::{self, Color};

/// Where an LED sits, normalized to its device: `x` runs left to right (or
/// along a strip), `y` top to bottom, both 0..1.
#[derive(Clone, Copy, Debug)]
pub struct LedPos {
    pub x: f32,
    pub y: f32,
    /// Stable per-LED value for effects that need randomness.
    pub seed: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Effect {
    Off,
    Static {
        color: Color,
    },
    /// Fade in and out, moving to the next color each cycle.
    Breathing {
        colors: Vec<Color>,
        #[serde(default = "default_period")]
        period: f32,
    },
    /// Whole device cycles through the color wheel.
    Spectrum {
        #[serde(default = "default_slow_period")]
        period: f32,
    },
    /// Rainbow moving across each device.
    Wave {
        #[serde(default = "default_period")]
        period: f32,
        /// How many full rainbows fit across a device.
        #[serde(default = "one")]
        repeat: f32,
        #[serde(default)]
        reverse: bool,
    },
    /// Fixed gradient across each device.
    Gradient {
        colors: Vec<Color>,
    },
    /// Gradient that scrolls across each device.
    ColorWave {
        colors: Vec<Color>,
        #[serde(default = "default_period")]
        period: f32,
        #[serde(default)]
        reverse: bool,
    },
    /// Random LEDs fade in and out over a background.
    Starlight {
        colors: Vec<Color>,
        #[serde(default)]
        background: Color,
        #[serde(default = "default_period")]
        period: f32,
    },
}

fn default_period() -> f32 {
    4.0
}
fn default_slow_period() -> f32 {
    12.0
}
fn one() -> f32 {
    1.0
}

fn rgbs(colors: &[Color]) -> Vec<Rgb> {
    colors.iter().map(|c| c.0).collect()
}

fn hash(mut x: u32) -> u32 {
    x ^= x >> 16;
    x = x.wrapping_mul(0x7feb_352d);
    x ^= x >> 15;
    x = x.wrapping_mul(0x846c_a68b);
    x ^ (x >> 16)
}

impl Effect {
    /// Whether output changes over time; static effects are only re-sent as
    /// keepalives.
    pub fn is_animated(&self) -> bool {
        !matches!(self, Effect::Off | Effect::Static { .. } | Effect::Gradient { .. })
    }

    pub fn render(&self, t: f32, led: LedPos) -> Rgb {
        match self {
            Effect::Off => Rgb::BLACK,
            Effect::Static { color } => color.0,
            Effect::Breathing { colors, period } => {
                if colors.is_empty() {
                    return Rgb::BLACK;
                }
                let cycle = t / period.max(0.1);
                let c = colors[(cycle as usize) % colors.len()].0;
                // Smooth 0 -> 1 -> 0 over one period.
                let phase = cycle.fract();
                let level = 0.5 - 0.5 * (phase * std::f32::consts::TAU).cos();
                color::scale(c, level)
            }
            Effect::Spectrum { period } => color::hue(t / period.max(0.1)),
            Effect::Wave { period, repeat, reverse } => {
                let x = if *reverse { 1.0 - led.x } else { led.x };
                color::hue(x * repeat - t / period.max(0.1))
            }
            Effect::Gradient { colors } => color::gradient(&rgbs(colors), led.x, false),
            Effect::ColorWave { colors, period, reverse } => {
                let x = if *reverse { 1.0 - led.x } else { led.x };
                color::gradient(&rgbs(colors), x - t / period.max(0.1), true)
            }
            Effect::Starlight { colors, background, period } => {
                if colors.is_empty() {
                    return background.0;
                }
                // Each LED twinkles once per period at its own random offset,
                // in a random color, and only on some cycles.
                let p = period.max(0.1);
                let offset = (hash(led.seed) % 1000) as f32 / 1000.0;
                let cycle = t / p + offset;
                let n = cycle.floor() as u32;
                let roll = hash(led.seed ^ n.wrapping_mul(0x9e37_79b9));
                if !roll.is_multiple_of(3) {
                    return background.0;
                }
                let star = colors[(roll as usize / 3) % colors.len()].0;
                let level = 0.5 - 0.5 * (cycle.fract() * std::f32::consts::TAU).cos();
                color::lerp(background.0, star, level)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ORIGIN: LedPos = LedPos { x: 0.0, y: 0.0, seed: 0 };

    #[test]
    fn effects_parse_from_config_json() {
        let e: Effect = serde_json::from_str(r##"{"type":"breathing","colors":["#ff0000"]}"##).unwrap();
        assert_eq!(e, Effect::Breathing { colors: vec![Color::parse("#ff0000").unwrap()], period: 4.0 });
    }

    #[test]
    fn breathing_starts_dark_and_peaks_mid_period() {
        let e = Effect::Breathing { colors: vec![Color::parse("#ff0000").unwrap()], period: 2.0 };
        assert_eq!(e.render(0.0, ORIGIN), Rgb::BLACK);
        assert_eq!(e.render(1.0, ORIGIN), Rgb::new(255, 0, 0));
    }

    #[test]
    fn wave_moves_over_time() {
        let e = Effect::Wave { period: 4.0, repeat: 1.0, reverse: false };
        assert_ne!(e.render(0.0, ORIGIN), e.render(1.0, ORIGIN));
    }
}
