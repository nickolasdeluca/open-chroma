//! Changes the lights: put every channel in direct mode and cycle red, green,
//! blue and white. Nothing is written to flash; Armoury Crate or a reboot
//! restores the saved effect.
//!
//! Usage: `direct [ARGB LEDs per header...]`, e.g. `direct 30 0`.
use std::thread::sleep;
use std::time::Duration;

use asus_aura::ChannelKind;

fn main() {
    let argb: Vec<usize> = std::env::args().skip(1).map(|a| a.parse().expect("LED count")).collect();
    let api = hidapi::HidApi::new().expect("hidapi init");
    for info in asus_aura::enumerate(&api) {
        let dev = asus_aura::Device::open(&api, info).expect("open");
        let channels = dev.config_table().expect("config table").channels();
        for ch in &channels {
            dev.set_direct(ch).expect("set direct");
        }
        for color in [[255, 0, 0], [0, 255, 0], [0, 0, 255], [255, 255, 255]] {
            println!("{color:?}");
            for ch in &channels {
                let n = match ch.kind {
                    ChannelKind::Fixed { leds, .. } => leds as usize,
                    ChannelKind::Addressable { header } => argb.get(header as usize).copied().unwrap_or(0),
                };
                dev.set_colors(ch, &vec![color; n]).expect("set colors");
            }
            sleep(Duration::from_secs(2));
        }
    }
}
