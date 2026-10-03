//! The OpenChroma service. Runs under the Windows service manager when
//! installed, or directly (without a console window) when launched by hand.
#![windows_subsystem = "windows"]

fn main() {
    if let Err(e) = openchroma::service::run() {
        log::error!("{e}");
        std::process::exit(1);
    }
}
