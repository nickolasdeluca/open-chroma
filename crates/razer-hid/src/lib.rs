//! Direct USB HID driver for Razer Chroma devices, no Synapse required.

pub mod devices;
pub mod protocol;

use std::ffi::CString;
use std::thread::sleep;
use std::time::Duration;

use hidapi::{HidApi, HidDevice};

pub use devices::{DeviceKind, DeviceSpec, MatrixType, Zone, DEVICES, RAZER_VID};
use protocol::Report;

pub type Error = hidapi::HidError;
pub type Result<T> = std::result::Result<T, Error>;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const BLACK: Rgb = Rgb { r: 0, g: 0, b: 0 };

    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Rgb { r, g, b }
    }
}

/// A supported device found on the bus but not yet opened.
#[derive(Clone, Debug)]
pub struct DeviceInfo {
    pub spec: &'static DeviceSpec,
    pub control_path: CString,
    /// Only for the ARGB controller: the interface that takes color data.
    pub argb_path: Option<CString>,
}

impl DeviceInfo {
    /// Stable identity used to notice the same device across rescans.
    pub fn key(&self) -> String {
        self.control_path.to_string_lossy().into_owned()
    }
}

/// List supported devices. Caller should `api.refresh_devices()` first to see
/// hot-plugged hardware.
pub fn enumerate(api: &HidApi) -> Vec<DeviceInfo> {
    let all: Vec<_> = api.device_list().filter(|d| d.vendor_id() == RAZER_VID).collect();
    let mut found = Vec::new();

    for spec in DEVICES {
        let ours = || all.iter().filter(move |d| d.product_id() == spec.pid);
        let control = ours().find(|d| {
            d.interface_number() == spec.control.interface
                && spec.control.usage.is_none_or(|(page, usage)| d.usage_page() == page && d.usage() == usage)
        });
        let Some(control) = control else { continue };

        let argb_path = if spec.matrix == MatrixType::ExtendedArgb {
            match ours().find(|d| d.interface_number() == 1) {
                Some(d) => Some(d.path().to_owned()),
                None => continue,
            }
        } else {
            None
        };

        found.push(DeviceInfo { spec, control_path: control.path().to_owned(), argb_path });
    }
    found
}

pub struct Device {
    pub info: DeviceInfo,
    control: HidDevice,
    argb: Option<HidDevice>,
}

impl Device {
    pub fn open(api: &HidApi, info: DeviceInfo) -> Result<Self> {
        let control = api.open_path(&info.control_path)?;
        let argb = match &info.argb_path {
            Some(p) => Some(api.open_path(p)?),
            None => None,
        };
        Ok(Device { info, control, argb })
    }

    pub fn spec(&self) -> &'static DeviceSpec {
        self.info.spec
    }

    fn tid(&self) -> u8 {
        self.info.spec.transaction_id
    }

    pub fn send(&self, report: &Report) -> Result<()> {
        self.control.send_feature_report(&report.to_wire())
    }

    /// Send a command and wait for its matching response. Other software
    /// (e.g. Synapse) may talk to the device at the same time, so responses
    /// to other commands are skipped and the request is retried.
    pub fn query(&self, report: &Report) -> Result<Report> {
        for _ in 0..3 {
            sleep(Duration::from_millis(2));
            self.send(report)?;
            for _ in 0..25 {
                sleep(Duration::from_millis(2));
                let mut buf = [0u8; protocol::REPORT_LEN + 1];
                self.control.get_feature_report(&mut buf)?;
                let resp = Report::from_wire(&buf);
                if resp.transaction_id == report.transaction_id
                    && resp.command_class == report.command_class
                    && resp.command_id == report.command_id
                    && resp.status != protocol::STATUS_NEW
                    && resp.status != protocol::STATUS_BUSY
                {
                    return Ok(resp);
                }
            }
        }
        Err(Error::HidApiError { message: "no response from device".into() })
    }

    pub fn firmware(&self) -> Result<String> {
        let r = self.query(&protocol::get_firmware(self.tid()))?;
        check_status(&r)?;
        Ok(format!("v{}.{}", r.args[0], r.args[1]))
    }

    pub fn serial(&self) -> Result<String> {
        let r = self.query(&protocol::get_serial(self.tid()))?;
        check_status(&r)?;
        let s: String = r.args[..22].iter().take_while(|&&b| b != 0).map(|&b| b as char).collect();
        Ok(s.trim().to_string())
    }

    /// Hardware brightness, 0-255. Applies to the whole device.
    pub fn set_brightness(&self, brightness: u8) -> Result<()> {
        let tid = self.tid();
        match self.spec().matrix {
            MatrixType::Standard => self.send(&protocol::standard_brightness(tid, protocol::LED_ID_BACKLIGHT, brightness)),
            MatrixType::Extended => self.send(&protocol::extended_brightness(tid, protocol::LED_ID_ZERO, brightness)),
            MatrixType::ExtendedArgb => {
                for led_id in 0x1A..=0x1F {
                    self.send(&protocol::extended_brightness(tid, led_id, brightness))?;
                }
                Ok(())
            }
        }
    }

    /// Only meaningful for the ARGB controller.
    pub fn set_argb_channel_sizes(&self, sizes: [u8; 6]) -> Result<()> {
        self.send(&protocol::argb_channel_sizes(self.tid(), sizes))
    }

    /// Display a full custom frame. `rows[r]` holds the colors for matrix row
    /// `r`; a row may be shorter than the matrix width (ARGB channels) and an
    /// empty row is skipped.
    pub fn set_frame(&self, rows: &[Vec<Rgb>]) -> Result<()> {
        let tid = self.tid();
        let spec = self.spec();
        for (row, colors) in rows.iter().enumerate().take(spec.rows as usize) {
            if colors.is_empty() {
                continue;
            }
            let rgb: Vec<u8> = colors.iter().take(spec.cols as usize).flat_map(|c| [c.r, c.g, c.b]).collect();
            match spec.matrix {
                MatrixType::Standard => self.send(&protocol::standard_frame_row(tid, row as u8, 0, &rgb))?,
                MatrixType::Extended => self.send(&protocol::extended_frame_row(tid, row as u8, 0, &rgb))?,
                MatrixType::ExtendedArgb => {
                    let argb = self.argb.as_ref().expect("ARGB device opened without color interface");
                    argb.send_feature_report(&protocol::argb_frame(row as u8, &rgb))?;
                }
            }
            sleep(Duration::from_millis(1));
        }
        match spec.matrix {
            MatrixType::Standard => self.send(&protocol::standard_mode_custom(tid)),
            MatrixType::Extended => self.send(&protocol::extended_mode_custom(tid)),
            MatrixType::ExtendedArgb => Ok(()),
        }
    }
}

fn check_status(r: &Report) -> Result<()> {
    match r.status {
        protocol::STATUS_OK => Ok(()),
        s => Err(Error::HidApiError { message: format!("device returned status 0x{s:02X}") }),
    }
}
