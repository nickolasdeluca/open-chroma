//! Read-only check: list supported devices and ask each for firmware/serial,
//! plus the current effect on extended-matrix devices (0x08 = custom frame).
fn main() {
    let api = hidapi::HidApi::new().expect("hidapi init");
    for info in razer_hid::enumerate(&api) {
        print!("{:04X} {:<42}", info.spec.pid, info.spec.name);
        match razer_hid::Device::open(&api, info) {
            Ok(dev) => {
                print!(" fw={:?} serial={:?}", dev.firmware().map_err(|e| e.to_string()), dev.serial().map_err(|e| e.to_string()));
                if dev.spec().matrix != razer_hid::MatrixType::Standard {
                    print!(" effect={:?}", dev.extended_effect().map(|e| format!("{e:#04X}")).map_err(|e| e.to_string()));
                }
                println!();
            }
            Err(e) => println!(" open failed: {e}"),
        }
    }
}
