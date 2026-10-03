//! Measure how fast each device accepts full custom frames.
use razer_hid::Rgb;
use std::time::Instant;

fn main() {
    let api = hidapi::HidApi::new().expect("hidapi init");
    for info in razer_hid::enumerate(&api) {
        let spec = info.spec;
        let dev = razer_hid::Device::open(&api, info).expect("open");
        let cols = if spec.kind == razer_hid::DeviceKind::ArgbController { 24 } else { spec.cols as usize };
        let n = 30;
        let start = Instant::now();
        for i in 0..n {
            let c = if i % 2 == 0 { Rgb::new(0, 0, 64) } else { Rgb::new(0, 64, 0) };
            let rows: Vec<Vec<Rgb>> = (0..spec.rows).map(|_| vec![c; cols]).collect();
            dev.set_frame(&rows).expect("frame");
        }
        let per = start.elapsed() / n;
        println!("{:<42} {:>6.1} ms/frame  (max ~{:.0} fps)", spec.name, per.as_secs_f64() * 1e3, 1.0 / per.as_secs_f64());
    }
}
