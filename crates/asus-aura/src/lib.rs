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
}
