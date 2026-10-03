//! Minimal logger: stderr plus a size-capped file next to the config.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

use log::{Level, LevelFilter, Log, Metadata, Record};

const MAX_LOG_BYTES: u64 = 4 << 20;

struct Logger {
    file: Option<Mutex<File>>,
}

pub fn path() -> PathBuf {
    crate::config::Config::dir().join("openchroma.log")
}

pub fn init() {
    let path = path();
    let _ = fs::create_dir_all(crate::config::Config::dir());
    if fs::metadata(&path).map(|m| m.len() > MAX_LOG_BYTES).unwrap_or(false) {
        let _ = fs::rename(&path, path.with_extension("log.old"));
    }
    let file = OpenOptions::new().create(true).append(true).open(&path).ok().map(Mutex::new);
    if log::set_boxed_logger(Box::new(Logger { file })).is_ok() {
        log::set_max_level(LevelFilter::Info);
    }
}

impl Log for Logger {
    fn enabled(&self, m: &Metadata) -> bool {
        m.level() <= Level::Info
    }

    fn log(&self, r: &Record) {
        if !self.enabled(r.metadata()) {
            return;
        }
        let mut t = unsafe { std::mem::zeroed() };
        unsafe { windows_sys::Win32::System::SystemInformation::GetLocalTime(&mut t) };
        let line = format!(
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02} {:<5} {}\n",
            t.wYear,
            t.wMonth,
            t.wDay,
            t.wHour,
            t.wMinute,
            t.wSecond,
            r.level(),
            r.args()
        );
        eprint!("{line}");
        if let Some(f) = &self.file {
            let _ = f.lock().unwrap_or_else(|e| e.into_inner()).write_all(line.as_bytes());
        }
    }

    fn flush(&self) {}
}
