//! Read-only check: list supported controllers and ask each for its firmware
//! and config table. Changes no lights and writes nothing to flash.
fn main() {
    let api = hidapi::HidApi::new().expect("hidapi init");
    for info in asus_aura::enumerate(&api) {
        println!("{:04X} {} (interface {})", info.spec.pid, info.spec.name, info.interface);
        let dev = match asus_aura::Device::open(&api, info) {
            Ok(dev) => dev,
            Err(e) => {
                println!("  open failed: {e}");
                continue;
            }
        };
        println!("  firmware: {:?}", dev.firmware().map_err(|e| e.to_string()));
        match dev.config_table() {
            Ok(t) => {
                println!(
                    "  mainboard LEDs: {}, 12V RGB headers: {}, ARGB headers: {}",
                    t.mainboard_leds(),
                    t.rgb_headers(),
                    t.argb_headers()
                );
                for row in t.raw.chunks(6) {
                    println!("  {}", row.iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(" "));
                }
            }
            Err(e) => println!("  config table failed: {e}"),
        }
    }
}
