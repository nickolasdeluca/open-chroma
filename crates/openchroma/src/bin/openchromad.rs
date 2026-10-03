//! Background service without a console window.
#![windows_subsystem = "windows"]

fn main() {
    if let Err(e) = openchroma::run_service() {
        log::error!("{e}");
        std::process::exit(1);
    }
}
