//! Static descriptions of supported devices.
//!
//! Matrix sizes, transaction ids and control-interface selection come from
//! OpenRGB's Razer controller (GPL-2.0-or-later), which in turn builds on
//! OpenRazer. Only devices verified on real hardware are listed; adding one is
//! a matter of appending an entry here.

pub const RAZER_VID: u16 = 0x1532;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DeviceKind {
    Keyboard,
    Mouse,
    Mousepad,
    /// Fixed-length LED strips, e.g. the Lian Li O11 Dynamic case.
    LedStrip,
    /// Six-channel addressable RGB controller with user-defined strip lengths.
    ArgbController,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MatrixType {
    /// Class 0x03 commands (older devices).
    Standard,
    /// Class 0x0F commands.
    Extended,
    /// Class 0x0F commands for control, 320-byte color reports on interface 1.
    ExtendedArgb,
}

/// Which HID collection carries the feature reports. On Windows every
/// top-level collection is a separate device path, so the interface number
/// alone is not enough.
#[derive(Clone, Copy, Debug)]
pub struct ControlInterface {
    pub interface: i32,
    /// `None` matches any collection on the interface.
    pub usage: Option<(u16, u16)>,
}

#[derive(Clone, Copy, Debug)]
pub struct Zone {
    pub name: &'static str,
    /// Row in the device matrix.
    pub row: u8,
    pub start_col: u8,
    pub len: u8,
}

#[derive(Debug)]
pub struct DeviceSpec {
    pub pid: u16,
    pub name: &'static str,
    pub kind: DeviceKind,
    pub matrix: MatrixType,
    pub transaction_id: u8,
    pub rows: u8,
    pub cols: u8,
    pub control: ControlInterface,
    pub zones: &'static [Zone],
}

impl DeviceSpec {
    pub fn led_count(&self) -> usize {
        self.rows as usize * self.cols as usize
    }
}

const fn zone(name: &'static str, row: u8, start_col: u8, len: u8) -> Zone {
    Zone { name, row, start_col, len }
}

pub static DEVICES: &[DeviceSpec] = &[
    DeviceSpec {
        pid: 0x0221,
        name: "Razer BlackWidow Chroma V2",
        kind: DeviceKind::Keyboard,
        matrix: MatrixType::Standard,
        transaction_id: 0x3F,
        rows: 6,
        cols: 22,
        control: ControlInterface { interface: 2, usage: Some((0x01, 0x02)) },
        zones: &[
            zone("Row 0", 0, 0, 22),
            zone("Row 1", 1, 0, 22),
            zone("Row 2", 2, 0, 22),
            zone("Row 3", 3, 0, 22),
            zone("Row 4", 4, 0, 22),
            zone("Row 5", 5, 0, 22),
        ],
    },
    DeviceSpec {
        pid: 0x0099,
        name: "Razer Basilisk V3",
        kind: DeviceKind::Mouse,
        matrix: MatrixType::Extended,
        transaction_id: 0x1F,
        rows: 1,
        cols: 11,
        control: ControlInterface { interface: 3, usage: Some((0x0C, 0x01)) },
        zones: &[zone("Logo", 0, 0, 1), zone("Scroll Wheel", 0, 1, 1), zone("Underglow", 0, 2, 9)],
    },
    DeviceSpec {
        pid: 0x0C02,
        name: "Razer Goliathus Chroma Extended",
        kind: DeviceKind::Mousepad,
        matrix: MatrixType::Extended,
        transaction_id: 0x3F,
        rows: 1,
        cols: 1,
        control: ControlInterface { interface: 0, usage: Some((0x01, 0x02)) },
        zones: &[zone("Edge", 0, 0, 1)],
    },
    DeviceSpec {
        pid: 0x0F13,
        name: "Lian Li O11 Dynamic Razer Edition",
        kind: DeviceKind::LedStrip,
        matrix: MatrixType::Extended,
        transaction_id: 0x1F,
        rows: 4,
        cols: 16,
        control: ControlInterface { interface: 2, usage: Some((0x01, 0x02)) },
        zones: &[zone("Strip 1", 0, 0, 16), zone("Strip 2", 1, 0, 16), zone("Strip 3", 2, 0, 16), zone("Strip 4", 3, 0, 16)],
    },
    DeviceSpec {
        pid: 0x0F1F,
        name: "Razer Chroma Addressable RGB Controller",
        kind: DeviceKind::ArgbController,
        matrix: MatrixType::ExtendedArgb,
        transaction_id: 0x3F,
        // Up to 80 LEDs per channel; the real lengths come from user config.
        rows: 6,
        cols: 80,
        control: ControlInterface { interface: 0, usage: None },
        zones: &[
            zone("Channel 1", 0, 0, 80),
            zone("Channel 2", 1, 0, 80),
            zone("Channel 3", 2, 0, 80),
            zone("Channel 4", 3, 0, 80),
            zone("Channel 5", 4, 0, 80),
            zone("Channel 6", 5, 0, 80),
        ],
    },
];

pub fn spec_for(pid: u16) -> Option<&'static DeviceSpec> {
    DEVICES.iter().find(|d| d.pid == pid)
}
