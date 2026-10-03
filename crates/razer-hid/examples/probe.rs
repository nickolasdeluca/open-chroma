//! Read-only check: list supported devices and ask each for firmware/serial.
fn main() {
    let api = hidapi::HidApi::new().expect("hidapi init");
    for info in razer_hid::enumerate(&api) {
        print!("{:04X} {:<42}", info.spec.pid, info.spec.name);
        match razer_hid::Device::open(&api, info) {
            Ok(dev) => println!(" fw={:?} serial={:?}", dev.firmware().map_err(|e| e.to_string()), dev.serial().map_err(|e| e.to_string())),
            Err(e) => println!(" open failed: {e}"),
        }
    }
}
