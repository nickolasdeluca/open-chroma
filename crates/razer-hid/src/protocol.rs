//! Razer's 90-byte HID feature-report protocol.
//!
//! Every lighting command is a feature report sent on the device's control
//! interface. The layout (shared with OpenRazer and OpenRGB) is:
//!
//! ```text
//! [0]  status            0x00 = new command
//! [1]  transaction id    device specific (0x1F, 0x3F, 0xFF, ...)
//! [2]  remaining packets big endian u16
//! [4]  protocol type     always 0
//! [5]  data size         number of meaningful argument bytes
//! [6]  command class
//! [7]  command id
//! [8]  arguments[80]
//! [88] crc               XOR of bytes 2..88
//! [89] reserved
//! ```
//!
//! On Windows hidapi needs a leading report-id byte (0), so buffers on the
//! wire are 91 bytes long.

pub const REPORT_LEN: usize = 90;
pub const ARGS_LEN: usize = 80;

pub const STATUS_NEW: u8 = 0x00;
pub const STATUS_BUSY: u8 = 0x01;
pub const STATUS_OK: u8 = 0x02;
pub const STATUS_FAIL: u8 = 0x03;
pub const STATUS_TIMEOUT: u8 = 0x04;
pub const STATUS_NOT_SUPPORTED: u8 = 0x05;

/// Do not persist to on-board flash. Every command OpenChroma sends uses this,
/// so nothing it does survives a power cycle or wears the device's flash.
pub const STORAGE_NO_SAVE: u8 = 0x00;
pub const LED_ID_ZERO: u8 = 0x00;
pub const LED_ID_BACKLIGHT: u8 = 0x05;

#[derive(Clone)]
pub struct Report {
    pub status: u8,
    pub transaction_id: u8,
    pub remaining_packets: u16,
    pub protocol_type: u8,
    pub data_size: u8,
    pub command_class: u8,
    pub command_id: u8,
    pub args: [u8; ARGS_LEN],
}

impl Report {
    pub fn new(transaction_id: u8, command_class: u8, command_id: u8, data_size: u8) -> Self {
        Report {
            status: STATUS_NEW,
            transaction_id,
            remaining_packets: 0,
            protocol_type: 0,
            data_size,
            command_class,
            command_id,
            args: [0; ARGS_LEN],
        }
    }

    /// Serialize with the leading HID report id expected by hidapi.
    pub fn to_wire(&self) -> [u8; REPORT_LEN + 1] {
        let mut buf = [0u8; REPORT_LEN + 1];
        let r = &mut buf[1..];
        r[0] = self.status;
        r[1] = self.transaction_id;
        r[2..4].copy_from_slice(&self.remaining_packets.to_be_bytes());
        r[4] = self.protocol_type;
        r[5] = self.data_size;
        r[6] = self.command_class;
        r[7] = self.command_id;
        r[8..88].copy_from_slice(&self.args);
        r[88] = crc(r);
        buf
    }

    pub fn from_wire(buf: &[u8; REPORT_LEN + 1]) -> Self {
        let r = &buf[1..];
        let mut args = [0u8; ARGS_LEN];
        args.copy_from_slice(&r[8..88]);
        Report {
            status: r[0],
            transaction_id: r[1],
            remaining_packets: u16::from_be_bytes([r[2], r[3]]),
            protocol_type: r[4],
            data_size: r[5],
            command_class: r[6],
            command_id: r[7],
            args,
        }
    }
}

fn crc(report: &[u8]) -> u8 {
    report[2..88].iter().fold(0, |acc, b| acc ^ b)
}

// ---------------------------------------------------------------------------
// Command builders
// ---------------------------------------------------------------------------

pub fn get_firmware(tid: u8) -> Report {
    Report::new(tid, 0x00, 0x81, 0x02)
}

pub fn get_serial(tid: u8) -> Report {
    Report::new(tid, 0x00, 0x82, 0x16)
}

/// Older "standard matrix" devices (e.g. BlackWidow Chroma V2): one row of
/// custom-frame colors.
pub fn standard_frame_row(tid: u8, row: u8, start: u8, rgb: &[u8]) -> Report {
    let cols = (rgb.len() / 3) as u8;
    let mut r = Report::new(tid, 0x03, 0x0B, (rgb.len() + 4) as u8);
    r.args[0] = 0xFF;
    r.args[1] = row;
    r.args[2] = start;
    r.args[3] = start + cols - 1;
    r.args[4..4 + rgb.len()].copy_from_slice(rgb);
    r
}

/// Switch a standard-matrix device to show its custom frame buffer.
pub fn standard_mode_custom(tid: u8) -> Report {
    let mut r = Report::new(tid, 0x03, 0x0A, 0x02);
    r.args[0] = 0x05;
    r.args[1] = STORAGE_NO_SAVE;
    r
}

pub fn standard_brightness(tid: u8, led_id: u8, brightness: u8) -> Report {
    let mut r = Report::new(tid, 0x03, 0x03, 0x03);
    r.args[0] = STORAGE_NO_SAVE;
    r.args[1] = led_id;
    r.args[2] = brightness;
    r
}

/// Newer "extended matrix" devices: one row of custom-frame colors.
pub fn extended_frame_row(tid: u8, row: u8, start: u8, rgb: &[u8]) -> Report {
    let cols = (rgb.len() / 3) as u8;
    let mut r = Report::new(tid, 0x0F, 0x03, (rgb.len() + 5) as u8);
    r.args[2] = row;
    r.args[3] = start;
    r.args[4] = start + cols - 1;
    r.args[5..5 + rgb.len()].copy_from_slice(rgb);
    r
}

pub fn extended_mode_custom(tid: u8) -> Report {
    let mut r = Report::new(tid, 0x0F, 0x02, 0x0C);
    r.args[0] = STORAGE_NO_SAVE;
    r.args[1] = LED_ID_ZERO;
    r.args[2] = 0x08;
    r
}

pub fn extended_brightness(tid: u8, led_id: u8, brightness: u8) -> Report {
    let mut r = Report::new(tid, 0x0F, 0x04, 0x03);
    r.args[0] = STORAGE_NO_SAVE;
    r.args[1] = led_id;
    r.args[2] = brightness;
    r
}

/// Tell the addressable RGB controller how many LEDs hang off each of its six
/// channels. A channel with 0 LEDs is disabled.
pub fn argb_channel_sizes(tid: u8, sizes: [u8; 6]) -> Report {
    let mut r = Report::new(tid, 0x0F, 0x08, 0x0D);
    r.args[0] = 0x06;
    for (i, &n) in sizes.iter().enumerate() {
        r.args[1 + i * 2] = if n == 0 { i as u8 + 1 } else { 0x19 };
        r.args[2 + i * 2] = n;
    }
    r
}

/// The ARGB controller takes its color data as a separate 320-byte report on
/// interface 1 rather than the usual 90-byte command.
pub const ARGB_REPORT_LEN: usize = 321;
pub const ARGB_MAX_LEDS: usize = 105;

pub fn argb_frame(channel: u8, rgb: &[u8]) -> [u8; ARGB_REPORT_LEN] {
    let mut buf = [0u8; ARGB_REPORT_LEN];
    let leds = (rgb.len() / 3).min(ARGB_MAX_LEDS);
    buf[0] = 0x00; // HID report id
    buf[1] = if channel < 5 { 0x04 } else { 0x84 };
    buf[2] = channel;
    buf[3] = channel;
    buf[4] = 0;
    buf[5] = leds.saturating_sub(1) as u8;
    buf[6..6 + leds * 3].copy_from_slice(&rgb[..leds * 3]);
    buf
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn firmware_request_matches_known_capture() {
        // Header bytes and CRC for "get firmware" as built by OpenRazer.
        let wire = get_firmware(0xFF).to_wire();
        assert_eq!(&wire[1..9], &[0x00, 0xFF, 0x00, 0x00, 0x00, 0x02, 0x00, 0x81]);
        assert_eq!(wire[89], 0x02 ^ 0x81);
    }

    #[test]
    fn roundtrip() {
        let r = extended_frame_row(0x1F, 2, 0, &[1, 2, 3, 4, 5, 6]);
        let back = Report::from_wire(&r.to_wire());
        assert_eq!(back.args[..11], r.args[..11]);
        assert_eq!(back.data_size, 11);
        assert_eq!(back.args[4], 1); // stop column
    }

    #[test]
    fn argb_sizes_layout() {
        let r = argb_channel_sizes(0x3F, [30, 0, 0, 0, 0, 12]);
        assert_eq!(&r.args[..13], &[6, 0x19, 30, 2, 0, 3, 0, 4, 0, 5, 0, 0x19, 12]);
    }
}
