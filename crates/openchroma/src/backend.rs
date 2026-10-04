//! The vendor drivers behind one interface, so the engine can find, open and
//! drive any supported device the same way.

use asus_aura::ChannelKind;
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
    /// An ASUS Aura motherboard. The counts come from the board itself, so
    /// they are zero until it has been opened.
    Aura {
        leds: u8,
        headers: u8,
    },
}

impl Model {
    /// The device takes its row lengths when it is opened, so it has to be
    /// reopened when they change.
    pub fn sizes_set_on_open(self) -> bool {
        match self {
            Model::Razer(spec) => spec.kind == DeviceKind::ArgbController,
            Model::Aura { .. } => false,
        }
    }
}

/// A supported device found on the bus but not yet opened.
#[derive(Clone, Debug)]
pub enum DeviceInfo {
    Razer(razer_hid::DeviceInfo),
    Aura(asus_aura::DeviceInfo),
}

impl DeviceInfo {
    /// Stable identity used to notice the same device across rescans.
    pub fn key(&self) -> String {
        match self {
            DeviceInfo::Razer(i) => i.key(),
            DeviceInfo::Aura(i) => i.key(),
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            DeviceInfo::Razer(i) => i.spec.name,
            DeviceInfo::Aura(i) => i.spec.name,
        }
    }

    pub fn pid(&self) -> u16 {
        match self {
            DeviceInfo::Razer(i) => i.spec.pid,
            DeviceInfo::Aura(i) => i.spec.pid,
        }
    }

    pub fn model(&self) -> Model {
        match self {
            DeviceInfo::Razer(i) => Model::Razer(i.spec),
            DeviceInfo::Aura(_) => Model::Aura { leds: 0, headers: 0 },
        }
    }
}

/// List supported devices from every driver. Caller should
/// `api.refresh_devices()` first to see hot-plugged hardware.
pub fn enumerate(api: &HidApi) -> Vec<DeviceInfo> {
    let razer = razer_hid::enumerate(api).into_iter().map(DeviceInfo::Razer);
    razer.chain(asus_aura::enumerate(api).into_iter().map(DeviceInfo::Aura)).collect()
}

pub enum Device {
    Razer(razer_hid::Device),
    Aura(Aura),
}

pub struct Aura {
    dev: asus_aura::Device,
    /// In layout row order: the onboard LEDs first, then each header.
    channels: Vec<asus_aura::Channel>,
}

impl Aura {
    fn open(api: &HidApi, info: asus_aura::DeviceInfo) -> Result<Aura> {
        let dev = asus_aura::Device::open(api, info)?;
        let channels = dev.config_table()?.channels();
        // Only once: every switch to direct mode blanks the LEDs for a
        // moment, so repeating it makes the board flicker.
        for ch in &channels {
            dev.set_direct(ch)?;
        }
        Ok(Aura { dev, channels })
    }

    fn model(&self) -> Model {
        let (mut leds, mut headers) = (0, 0);
        for ch in &self.channels {
            match ch.kind {
                ChannelKind::Fixed { leds: n, .. } => leds = n,
                ChannelKind::Addressable { .. } => headers += 1,
            }
        }
        Model::Aura { leds, headers }
    }

    fn set_frame(&self, rows: &[Vec<Rgb>]) -> Result<()> {
        for (ch, row) in self.channels.iter().zip(rows) {
            if !row.is_empty() {
                let colors: Vec<[u8; 3]> = row.iter().map(|c| [c.r, c.g, c.b]).collect();
                self.dev.set_colors(ch, &colors)?;
            }
        }
        Ok(())
    }
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
            DeviceInfo::Aura(info) => Ok(Device::Aura(Aura::open(api, info)?)),
        }
    }

    /// What the device turned out to be once opened; can tell more than
    /// [`DeviceInfo::model`].
    pub fn model(&self) -> Model {
        match self {
            Device::Razer(d) => Model::Razer(d.spec()),
            Device::Aura(a) => a.model(),
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            Device::Razer(d) => d.spec().name,
            Device::Aura(a) => a.dev.spec().name,
        }
    }

    pub fn firmware(&self) -> Option<String> {
        match self {
            Device::Razer(d) => d.firmware().ok(),
            Device::Aura(a) => a.dev.firmware().ok(),
        }
    }

    pub fn serial(&self) -> Option<String> {
        match self {
            Device::Razer(d) => d.serial().ok().filter(|s| s.chars().all(|c| c.is_ascii_graphic()) && !s.is_empty()),
            Device::Aura(_) => None,
        }
    }

    /// Show one frame, one row per layout row. Empty rows are left as they
    /// are.
    pub fn set_frame(&self, rows: &[Vec<Rgb>]) -> Result<()> {
        match self {
            Device::Razer(d) => d.set_frame(rows),
            Device::Aura(a) => a.set_frame(rows),
        }
    }
}
