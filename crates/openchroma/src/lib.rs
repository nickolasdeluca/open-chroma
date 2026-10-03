//! OpenChroma: an open replacement for Razer Synapse lighting and the Chroma
//! SDK service.

pub mod apps;
pub mod backend;
pub mod client;
pub mod color;
pub mod config;
pub mod effects;
pub mod engine;
pub mod http;
pub mod install;
pub mod layout;
pub mod logging;
pub mod pipe;
pub mod sdk;
pub mod service;
pub mod synapse;

use std::thread;

/// Run the service until the process is killed.
pub fn run_service() -> Result<(), String> {
    logging::init();
    let listener = pipe::bind().map_err(|e| format!("cannot claim the SDK pipe ({e}); is OpenChroma already running?"))?;
    let config = config::Config::load_or_create();
    log::info!("OpenChroma {} starting; config at {}", env!("CARGO_PKG_VERSION"), config::Config::path().display());

    let shared = engine::Shared::new(config);
    http::start(shared.clone());
    {
        let shared = shared.clone();
        thread::Builder::new().name("sdk pipe".into()).spawn(move || listener.serve(shared)).map_err(|e| e.to_string())?;
    }
    engine::run(shared);
    Ok(())
}
