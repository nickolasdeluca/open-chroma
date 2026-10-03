//! The vendor drivers behind one interface, so the engine can find, open and
//! drive any supported device the same way.

use hidapi::HidApi;
use razer_hid::{DeviceKind, Rgb};

use crate::config::Config;

/// Every driver talks HID through `hidapi`, so they share its error type.
pub type Error = hidapi::HidError;
pub type Result<T> = std::result::Result<T, Error>;

/// What a device is, which decides its LED layout.
#[derive(Clone, Copy, Debug)]
pub enum Model {
    Razer(&'static razer_hid::DeviceSpec),
}

impl Model {
    /// The device takes its row lengths when it is opened, so it has to be
    /// reopened when they change.
    pub fn sizes_set_on_open(self) -> bool {
        match self {
            Model::Razer(spec) => spec.kind == DeviceKind::ArgbController,
        }
    }
}

/// A supported device found on the bus but not yet opened.
#[derive(Clone, Debug)]
pub enum DeviceInfo {
    Razer(razer_hid::DeviceInfo),
}

impl DeviceInfo {
    /// Stable identity used to notice the same device across rescans.
    pub fn key(&self) -> String {
        match self {
            DeviceInfo::Razer(i) => i.key(),
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            DeviceInfo::Razer(i) => i.spec.name,
        }
    }

    pub fn pid(&self) -> u16 {
        match self {
            DeviceInfo::Razer(i) => i.spec.pid,
        }
    }

    pub fn model(&self) -> Model {
        match self {
            DeviceInfo::Razer(i) => Model::Razer(i.spec),
        }
    }
}

/// List supported devices from every driver. Caller should
/// `api.refresh_devices()` first to see hot-plugged hardware.
pub fn enumerate(api: &HidApi) -> Vec<DeviceInfo> {
    razer_hid::enumerate(api).into_iter().map(DeviceInfo::Razer).collect()
}

pub enum Device {
    Razer(razer_hid::Device),
}

impl Device {
    /// Open a device and get it ready to take frames.
    pub fn open(api: &HidApi, info: DeviceInfo, config: &Config) -> Result<Device> {
        match info {
            DeviceInfo::Razer(info) => {
                let dev = razer_hid::Device::open(api, info)?;
                let spec = dev.spec();
                if spec.kind == DeviceKind::ArgbController {
                    let mut sizes = [0u8; 6];
                    for (s, ch) in sizes.iter_mut().zip(&config.argb_channels) {
                        *s = ch.leds.min(spec.cols);
                    }
                    dev.set_argb_channel_sizes(sizes)?;
                }
                // Software brightness does the dimming; make sure a previous
                // app did not leave the hardware dimmed.
                dev.set_brightness(255)?;
                Ok(Device::Razer(dev))
            }
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            Device::Razer(d) => d.spec().name,
        }
    }

    pub fn firmware(&self) -> Option<String> {
        match self {
            Device::Razer(d) => d.firmware().ok(),
        }
    }

    pub fn serial(&self) -> Option<String> {
        match self {
            Device::Razer(d) => d.serial().ok().filter(|s| s.chars().all(|c| c.is_ascii_graphic()) && !s.is_empty()),
        }
    }

    /// Show one frame, one row per layout row. Empty rows are left as they
    /// are.
    pub fn set_frame(&self, rows: &[Vec<Rgb>]) -> Result<()> {
        match self {
            Device::Razer(d) => d.set_frame(rows),
        }
    }
}
