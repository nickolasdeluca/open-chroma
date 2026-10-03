//! Where each physical LED lives: its position for profile effects, and which
//! Chroma SDK canvas cell drives it while a game is in control.

use chroma_proto::Category;
use razer_hid::{DeviceKind, DeviceSpec};

use crate::config::Config;
use crate::effects::LedPos;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Source {
    /// One cell of a category canvas.
    Cell(Category, usize),
    /// Average of a whole canvas, for single-LED devices.
    Average(Category),
}

#[derive(Clone, Debug)]
pub struct Led {
    pub pos: LedPos,
    /// `None` when the device is mapped to no game canvas.
    pub source: Option<Source>,
}

/// LEDs grouped by device matrix row, matching `razer_hid::Device::set_frame`.
#[derive(Clone, Debug)]
pub struct DeviceLayout {
    /// Short id used in profile overrides.
    pub id: &'static str,
    pub rows: Vec<Vec<Led>>,
    /// Override key per row, e.g. "argb:4"; `None` for keyboards and mice.
    pub row_zones: Vec<Option<String>>,
    /// The game canvas this device shows, if any.
    pub game: Option<Category>,
}

pub fn device_id(kind: DeviceKind) -> &'static str {
    match kind {
        DeviceKind::Keyboard => "keyboard",
        DeviceKind::Mouse => "mouse",
        DeviceKind::Mousepad => "mousepad",
        DeviceKind::LedStrip => "case",
        DeviceKind::ArgbController => "argb",
    }
}

fn seed(kind: DeviceKind, row: usize, col: usize) -> u32 {
    (kind as u32) << 24 | (row as u32) << 12 | col as u32
}

fn frac(i: usize, n: usize) -> f32 {
    if n <= 1 {
        0.5
    } else {
        i as f32 / (n - 1) as f32
    }
}

/// Basilisk V3 LEDs placed on the SDK's 9x7 mouse grid, (row, col). Index 0 is
/// the logo, 1 the scroll wheel, 2-10 the underglow strip.
const BASILISK_V3_CELLS: [(usize, usize); 11] = [(7, 3), (2, 3), (1, 0), (3, 0), (5, 0), (7, 0), (8, 3), (7, 6), (5, 6), (3, 6), (1, 6)];

/// The canvas games naturally draw for this kind of device.
pub fn natural_category(kind: DeviceKind) -> Category {
    match kind {
        DeviceKind::Keyboard => Category::Keyboard,
        DeviceKind::Mouse => Category::Mouse,
        DeviceKind::Mousepad => Category::Mousepad,
        DeviceKind::LedStrip | DeviceKind::ArgbController => Category::ChromaLink,
    }
}

/// Place an LED on a canvas it was not designed for, by its position.
fn resample(category: Category, pos: LedPos, row: usize, rows: usize, single_led: bool) -> Source {
    if category == Category::ChromaLink {
        return Source::Cell(category, if rows > 1 { link_led(row) } else { 0 });
    }
    if single_led {
        return Source::Average(category);
    }
    let (gr, gc) = category.dims();
    let r = (pos.y * (gr - 1) as f32).round() as usize;
    let c = (pos.x * (gc - 1) as f32).round() as usize;
    Source::Cell(category, r * gc + c)
}

/// Chroma Link LED 0 is conventionally the "main" color and 1-4 are zones;
/// spread multi-zone devices over 1-4.
fn link_led(zone: usize) -> usize {
    1 + zone % 4
}

pub fn build(spec: &DeviceSpec, config: &Config) -> DeviceLayout {
    let kind = spec.kind;
    let id = device_id(kind);
    let (mut rows, row_zones): (Vec<Vec<Led>>, Vec<Option<String>>) = match kind {
        DeviceKind::Keyboard => {
            let (r, c) = (spec.rows as usize, spec.cols as usize);
            let (_, kc) = Category::Keyboard.dims();
            let rows = (0..r)
                .map(|row| {
                    (0..c)
                        .map(|col| Led {
                            pos: LedPos { x: frac(col, c), y: frac(row, r), seed: seed(kind, row, col) },
                            source: Some(Source::Cell(Category::Keyboard, row * kc + col)),
                        })
                        .collect()
                })
                .collect();
            (rows, vec![None; r])
        }
        DeviceKind::Mouse => {
            let (gr, gc) = Category::Mouse.dims();
            let leds = BASILISK_V3_CELLS
                .iter()
                .take(spec.cols as usize)
                .enumerate()
                .map(|(i, &(row, col))| Led {
                    pos: LedPos { x: frac(col, gc), y: frac(row, gr), seed: seed(kind, 0, i) },
                    source: Some(Source::Cell(Category::Mouse, row * gc + col)),
                })
                .collect();
            (vec![leds], vec![None])
        }
        DeviceKind::Mousepad => {
            let led = Led { pos: LedPos { x: 0.5, y: 0.5, seed: seed(kind, 0, 0) }, source: Some(Source::Average(Category::Mousepad)) };
            (vec![vec![led]], vec![None])
        }
        DeviceKind::LedStrip => {
            let (r, c) = (spec.rows as usize, spec.cols as usize);
            let rows = (0..r)
                .map(|row| {
                    (0..c)
                        .map(|col| Led {
                            pos: LedPos { x: frac(col, c), y: frac(row, r), seed: seed(kind, row, col) },
                            source: Some(Source::Cell(Category::ChromaLink, link_led(row))),
                        })
                        .collect()
                })
                .collect();
            (rows, (0..r).map(|row| Some(format!("{id}:{}", row + 1))).collect())
        }
        DeviceKind::ArgbController => {
            let rows = config
                .argb_channels
                .iter()
                .take(spec.rows as usize)
                .enumerate()
                .map(|(ch, cfg)| {
                    let n = cfg.leds.min(spec.cols) as usize;
                    let cell = cfg.chroma_link_led.map(|l| l.min(4) as usize).unwrap_or_else(|| link_led(ch));
                    (0..n)
                        .map(|i| Led {
                            pos: LedPos { x: frac(i, n), y: frac(ch, 6), seed: seed(kind, ch, i) },
                            source: Some(Source::Cell(Category::ChromaLink, cell)),
                        })
                        .collect()
                })
                .collect();
            (rows, (0..spec.rows as usize).map(|ch| Some(format!("{id}:{}", ch + 1))).collect())
        }
    };

    let natural = natural_category(kind);
    let game = config.game_mapping.get(id).map_or(Some(natural), |g| g.category());
    if game != Some(natural) {
        let single_led = rows.iter().map(Vec::len).sum::<usize>() == 1;
        let n = rows.len();
        for (r, row) in rows.iter_mut().enumerate() {
            for led in row {
                led.source = game.map(|c| resample(c, led.pos, r, n, single_led));
            }
        }
    }
    DeviceLayout { id, rows, row_zones, game }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_device_has_matrix_shaped_layout() {
        let mut cfg = Config::default();
        cfg.argb_channels[3].leds = 24;
        for spec in razer_hid::DEVICES {
            let layout = build(spec, &cfg);
            assert!(layout.rows.len() <= spec.rows as usize, "{}", spec.name);
            assert_eq!(layout.rows.len(), layout.row_zones.len(), "{}", spec.name);
            for row in &layout.rows {
                assert!(row.len() <= spec.cols as usize, "{}", spec.name);
                for led in row {
                    if let Some(Source::Cell(cat, i)) = led.source {
                        assert!(i < cat.len(), "{} cell {i} outside {cat:?}", spec.name);
                    }
                }
            }
        }
    }

    #[test]
    fn remapped_devices_stay_on_their_canvas() {
        use crate::config::GameSource;
        let mut cfg = Config::default();
        cfg.game_mapping.insert("mouse".into(), GameSource::Keyboard);
        cfg.game_mapping.insert("keyboard".into(), GameSource::None);
        cfg.game_mapping.insert("mousepad".into(), GameSource::ChromaLink);
        let layout = |pid| build(razer_hid::devices::spec_for(pid).unwrap(), &cfg);

        let mouse = layout(0x0099);
        assert_eq!(mouse.game, Some(Category::Keyboard));
        assert!(mouse.rows[0].iter().all(|l| matches!(l.source, Some(Source::Cell(Category::Keyboard, i)) if i < 132)));

        let keyboard = layout(0x0221);
        assert_eq!(keyboard.game, None);
        assert!(keyboard.rows.iter().flatten().all(|l| l.source.is_none()));

        assert_eq!(layout(0x0C02).rows[0][0].source, Some(Source::Cell(Category::ChromaLink, 0)));
    }

    #[test]
    fn argb_uses_configured_lengths() {
        let mut cfg = Config::default();
        cfg.argb_channels[3].leds = 24;
        cfg.argb_channels[5].leds = 24;
        let spec = razer_hid::devices::spec_for(0x0F1F).unwrap();
        let lens: Vec<_> = build(spec, &cfg).rows.iter().map(Vec::len).collect();
        assert_eq!(lens, [0, 0, 0, 24, 0, 24]);
    }
}
