//! Direct USB HID driver for ASUS Aura motherboard lighting, no Armoury Crate
//! required.
//!
//! The protocol comes from OpenRGB's `AsusAuraUSBController`
//! (GPL-2.0-or-later). Only the USB controller found on boards from about
//! 2018 on is handled; older boards drive their LEDs over SMBus, which needs a
//! kernel driver.

use std::ffi::CString;
use std::time::{Duration, Instant};

use hidapi::{HidApi, HidDevice};

pub type Error = hidapi::HidError;
pub type Result<T> = std::result::Result<T, Error>;

pub const ASUS_VID: u16 = 0x0B05;

/// Every request and reply starts with this report id.
const REPORT_ID: u8 = 0xEC;
const REPORT_LEN: usize = 65;

const REQUEST_FIRMWARE: u8 = 0x82;
const REPLY_FIRMWARE: u8 = 0x02;
const REQUEST_CONFIG_TABLE: u8 = 0xB0;
const REPLY_CONFIG_TABLE: u8 = 0x30;
const SET_EFFECT: u8 = 0x35;
const SET_DIRECT_COLORS: u8 = 0x40;
// 0x3F commits the current effect to flash. It is deliberately never sent:
// OpenChroma must not write device flash, and the board falls back to its
// saved effect on the next boot.

const EFFECT_DIRECT: u8 = 0xFF;
/// Set on the last color packet of a channel to show the new frame.
const APPLY: u8 = 0x80;
const LEDS_PER_PACKET: usize = 20;
/// The direct channel that drives the onboard LEDs and 12 V RGB headers.
const FIXED_DIRECT_CHANNEL: u8 = 0x04;

/// OpenRGB's limit for one addressable header.
pub const MAX_ARGB_LEDS: usize = 120;

const REPLY_TIMEOUT: Duration = Duration::from_millis(500);

#[derive(Debug)]
pub struct DeviceSpec {
    pub pid: u16,
    pub name: &'static str,
}

/// Only controllers checked with `probe` on real hardware. OpenRGB also lists
/// PIDs 0x1939, 0x19AF, 0x1AA6 and 0x1BED.
pub static DEVICES: &[DeviceSpec] = &[
    // ROG Crosshair VIII Hero (X570).
    DeviceSpec { pid: 0x18F3, name: "ASUS Aura Motherboard" },
];

/// A supported controller found on the bus but not yet opened.
#[derive(Clone, Debug)]
pub struct DeviceInfo {
    pub spec: &'static DeviceSpec,
    pub path: CString,
    pub interface: i32,
}

impl DeviceInfo {
    /// Stable identity used to notice the same device across rescans.
    pub fn key(&self) -> String {
        self.path.to_string_lossy().into_owned()
    }
}

/// List supported controllers. Caller should `api.refresh_devices()` first to
/// see hot-plugged hardware.
pub fn enumerate(api: &HidApi) -> Vec<DeviceInfo> {
    api.device_list()
        .filter(|d| d.vendor_id() == ASUS_VID)
        .filter_map(|d| {
            let spec = DEVICES.iter().find(|s| s.pid == d.product_id())?;
            Some(DeviceInfo { spec, path: d.path().to_owned(), interface: d.interface_number() })
        })
        .collect()
}

/// What the controller reports about the LEDs it drives.
#[derive(Clone, Debug)]
pub struct ConfigTable {
    pub raw: [u8; 60],
}

impl ConfigTable {
    pub fn argb_headers(&self) -> u8 {
        self.raw[0x02]
    }

    /// Onboard LEDs, including the 12 V RGB headers.
    pub fn mainboard_leds(&self) -> u8 {
        self.raw[0x1B]
    }

    /// 12 V RGB headers. OpenRGB treats a count above the LED total as bogus.
    pub fn rgb_headers(&self) -> u8 {
        let n = self.raw[0x1D];
        if n > self.mainboard_leds() {
            0
        } else {
            n
        }
    }

    /// The lighting channels in the order the controller numbers their
    /// effects: the fixed LEDs first, if any, then each addressable header.
    pub fn channels(&self) -> Vec<Channel> {
        let mut channels = Vec::new();
        if self.mainboard_leds() > 0 {
            channels.push(Channel {
                kind: ChannelKind::Fixed { leds: self.mainboard_leds(), rgb_headers: self.rgb_headers() },
                effect: 0,
                direct: FIXED_DIRECT_CHANNEL,
            });
        }
        for header in 0..self.argb_headers() {
            let effect = channels.len() as u8;
            channels.push(Channel { kind: ChannelKind::Addressable { header }, effect, direct: header });
        }
        channels
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChannelKind {
    /// Onboard LEDs followed by the 12 V RGB headers, one color each.
    Fixed { leds: u8, rgb_headers: u8 },
    /// An ARGB header; its length depends on what is plugged in.
    Addressable { header: u8 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Channel {
    pub kind: ChannelKind,
    /// Index used when setting the channel's effect.
    effect: u8,
    /// Index used when sending the channel's colors.
    direct: u8,
}

fn effect_packet(channel: &Channel, effect: u8) -> [u8; REPORT_LEN] {
    let mut buf = [0u8; REPORT_LEN];
    // Byte 4 stays 0: a 1 would set the effect shown while the PC is off,
    // which only takes hold after a commit.
    buf[..6].copy_from_slice(&[REPORT_ID, SET_EFFECT, channel.effect, 0x00, 0x00, effect]);
    buf
}

fn color_packets(channel: &Channel, colors: &[[u8; 3]]) -> Vec<[u8; REPORT_LEN]> {
    let chunks = colors.chunks(LEDS_PER_PACKET);
    let last = chunks.len().saturating_sub(1);
    chunks
        .enumerate()
        .map(|(i, chunk)| {
            let mut buf = [0u8; REPORT_LEN];
            buf[0] = REPORT_ID;
            buf[1] = SET_DIRECT_COLORS;
            buf[2] = channel.direct | if i == last { APPLY } else { 0 };
            buf[3] = (i * LEDS_PER_PACKET) as u8;
            buf[4] = chunk.len() as u8;
            for (dst, src) in buf[5..].chunks_exact_mut(3).zip(chunk) {
                dst.copy_from_slice(src);
            }
            buf
        })
        .collect()
}

pub struct Device {
    pub info: DeviceInfo,
    hid: HidDevice,
}

impl Device {
    pub fn open(api: &HidApi, info: DeviceInfo) -> Result<Self> {
        let hid = api.open_path(&info.path)?;
        Ok(Device { info, hid })
    }

    pub fn spec(&self) -> &'static DeviceSpec {
        self.info.spec
    }

    /// Send a request and wait for its reply. Armoury Crate may talk to the
    /// controller at the same time, so replies to other requests are skipped.
    fn query(&self, request: u8, reply: u8) -> Result<[u8; REPORT_LEN]> {
        let mut buf = [0u8; REPORT_LEN];
        buf[0] = REPORT_ID;
        buf[1] = request;
        self.hid.write(&buf)?;

        let deadline = Instant::now() + REPLY_TIMEOUT;
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            let mut resp = [0u8; REPORT_LEN];
            let n = self.hid.read_timeout(&mut resp, left.as_millis().max(1) as i32)?;
            if n >= 2 && resp[0] == REPORT_ID && resp[1] == reply {
                return Ok(resp);
            }
        }
        Err(Error::HidApiError { message: format!("no reply to request {request:#04X}") })
    }

    pub fn firmware(&self) -> Result<String> {
        let r = self.query(REQUEST_FIRMWARE, REPLY_FIRMWARE)?;
        let s: String = r[2..18].iter().take_while(|&&b| b != 0).map(|&b| b as char).collect();
        Ok(s.trim().to_string())
    }

    pub fn config_table(&self) -> Result<ConfigTable> {
        let r = self.query(REQUEST_CONFIG_TABLE, REPLY_CONFIG_TABLE)?;
        let mut raw = [0u8; 60];
        raw.copy_from_slice(&r[4..64]);
        Ok(ConfigTable { raw })
    }

    /// Hand a channel over to the host. The switch lasts until the next boot
    /// or until other software sets an effect.
    pub fn set_direct(&self, channel: &Channel) -> Result<()> {
        self.hid.write(&effect_packet(channel, EFFECT_DIRECT))?;
        Ok(())
    }

    /// Show one frame on a channel that is in direct mode. Addressable
    /// headers take at most [`MAX_ARGB_LEDS`] colors; extra ones are dropped.
    pub fn set_colors(&self, channel: &Channel, colors: &[[u8; 3]]) -> Result<()> {
        let colors = &colors[..colors.len().min(MAX_ARGB_LEDS)];
        for packet in color_packets(channel, colors) {
            self.hid.write(&packet)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_table_fields() {
        let mut raw = [0u8; 60];
        raw[0x02] = 3;
        raw[0x1B] = 5;
        raw[0x1D] = 2;
        let t = ConfigTable { raw };
        assert_eq!((t.argb_headers(), t.mainboard_leds(), t.rgb_headers()), (3, 5, 2));

        raw[0x1D] = 9;
        assert_eq!(ConfigTable { raw }.rgb_headers(), 0);
    }

    fn table(leds: u8, rgb_headers: u8, argb_headers: u8) -> ConfigTable {
        let mut raw = [0u8; 60];
        raw[0x02] = argb_headers;
        raw[0x1B] = leds;
        raw[0x1D] = rgb_headers;
        ConfigTable { raw }
    }

    #[test]
    fn channels_number_effects_and_direct_indices() {
        let ch = table(8, 2, 2).channels();
        assert_eq!(ch.len(), 3);
        assert_eq!(ch[0], Channel { kind: ChannelKind::Fixed { leds: 8, rgb_headers: 2 }, effect: 0, direct: 0x04 });
        assert_eq!(ch[1], Channel { kind: ChannelKind::Addressable { header: 0 }, effect: 1, direct: 0 });
        assert_eq!(ch[2], Channel { kind: ChannelKind::Addressable { header: 1 }, effect: 2, direct: 1 });

        // Without fixed LEDs the headers start at effect 0.
        let ch = table(0, 0, 1).channels();
        assert_eq!(ch, vec![Channel { kind: ChannelKind::Addressable { header: 0 }, effect: 0, direct: 0 }]);
    }

    #[test]
    fn effect_packet_never_sets_shutdown_effect() {
        let ch = table(8, 2, 2).channels();
        let p = effect_packet(&ch[1], EFFECT_DIRECT);
        assert_eq!(&p[..6], &[0xEC, 0x35, 1, 0, 0, 0xFF]);
        assert!(p[6..].iter().all(|&b| b == 0));
    }

    #[test]
    fn color_packets_split_and_apply_on_last() {
        let ch = table(8, 2, 2).channels();
        let colors: Vec<[u8; 3]> = (0..45u8).map(|i| [i, i.wrapping_add(1), i.wrapping_add(2)]).collect();
        let p = color_packets(&ch[2], &colors);
        assert_eq!(p.len(), 3);
        let header = |b: &[u8; REPORT_LEN]| (b[0], b[1], b[2], b[3], b[4]);
        assert_eq!(header(&p[0]), (0xEC, 0x40, 0x01, 0, 20));
        assert_eq!(header(&p[1]), (0xEC, 0x40, 0x01, 20, 20));
        assert_eq!(header(&p[2]), (0xEC, 0x40, 0x81, 40, 5));
        assert_eq!(&p[2][5..20], &[40, 41, 42, 41, 42, 43, 42, 43, 44, 43, 44, 45, 44, 45, 46]);
        assert!(p[2][20..].iter().all(|&b| b == 0));

        let fixed = color_packets(&ch[0], &[[1, 2, 3]; 8]);
        assert_eq!(fixed.len(), 1);
        assert_eq!(header(&fixed[0]), (0xEC, 0x40, 0x84, 0, 8));

        assert!(color_packets(&ch[0], &[]).is_empty());
    }
}
