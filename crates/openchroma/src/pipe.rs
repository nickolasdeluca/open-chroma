//! Named-pipe endpoint for the RzChromaSDK DLL replacement.
//!
//! Each connected game gets its own pipe instance and thread. The session
//! lives exactly as long as the connection, so a game that crashes or exits
//! without calling `UnInit` releases the lights immediately.

use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::os::windows::io::{FromRawHandle, RawHandle};
use std::sync::{Arc, OnceLock};
use std::thread;

use chroma_proto::{ClientMsg, Welcome, PIPE_CLIENT_ACCESS, PIPE_NAME};
use windows_sys::Win32::Foundation::{GetLastError, ERROR_PIPE_CONNECTED, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Security::Authorization::{ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1};
use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
use windows_sys::Win32::Storage::FileSystem::{FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_DUPLEX};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};

use crate::engine::{self, lock, Shared};
use crate::sdk::Client;

/// The service runs as SYSTEM but games run as the user, so the default pipe
/// ACL (read-only for everyone else) would lock them out. SYSTEM,
/// administrators and the pipe's owner get full control; any signed-in user
/// may connect and talk, but not create pipe instances.
fn security_descriptor() -> std::io::Result<usize> {
    static SD: OnceLock<usize> = OnceLock::new();
    if let Some(&sd) = SD.get() {
        return Ok(sd);
    }
    let sddl = format!("D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GA;;;OW)(A;;0x{PIPE_CLIENT_ACCESS:X};;;AU)");
    let wide: Vec<u16> = sddl.encode_utf16().chain(Some(0)).collect();
    let mut sd = std::ptr::null_mut();
    // The descriptor is kept for the life of the process; never freed.
    let ok = unsafe { ConvertStringSecurityDescriptorToSecurityDescriptorW(wide.as_ptr(), SDDL_REVISION_1, &mut sd, std::ptr::null_mut()) };
    if ok == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(*SD.get_or_init(|| sd as usize))
}

fn create_instance(first: bool) -> std::io::Result<HANDLE> {
    let attrs = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: security_descriptor()? as *mut _,
        bInheritHandle: 0,
    };
    let name: Vec<u16> = PIPE_NAME.encode_utf16().chain(Some(0)).collect();
    let open_mode = PIPE_ACCESS_DUPLEX | if first { FILE_FLAG_FIRST_PIPE_INSTANCE } else { 0 };
    let handle = unsafe {
        CreateNamedPipeW(
            name.as_ptr(),
            open_mode,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            PIPE_UNLIMITED_INSTANCES,
            64 * 1024,
            64 * 1024,
            0,
            &attrs,
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(handle)
    }
}

/// Claim the pipe name. Fails if another OpenChroma instance is running,
/// which doubles as the single-instance check.
pub fn bind() -> std::io::Result<Listener> {
    Ok(Listener { next: create_instance(true)? })
}

pub struct Listener {
    next: HANDLE,
}

// The handle is only ever used by the thread that owns the listener.
unsafe impl Send for Listener {}

impl Listener {
    pub fn serve(mut self, shared: Arc<Shared>) {
        loop {
            let handle = self.next;
            let connected =
                unsafe { ConnectNamedPipe(handle, std::ptr::null_mut()) } != 0 || unsafe { GetLastError() } == ERROR_PIPE_CONNECTED;
            // Have the next instance ready before handling this one.
            self.next = loop {
                match create_instance(false) {
                    Ok(h) => break h,
                    Err(e) => {
                        log::error!("cannot create SDK pipe instance: {e}");
                        thread::sleep(std::time::Duration::from_secs(1));
                    }
                }
            };
            let pipe = unsafe { File::from_raw_handle(handle as RawHandle) };
            if !connected {
                continue;
            }
            let shared = shared.clone();
            let _ = thread::Builder::new().name("sdk client".into()).spawn(move || serve_client(pipe, &shared));
        }
    }
}

fn serve_client(pipe: File, shared: &Shared) {
    let Ok(mut writer) = pipe.try_clone() else { return };
    let mut lines = BufReader::new(pipe).lines();

    let session = match lines.next().and_then(Result::ok).and_then(|l| serde_json::from_str::<ClientMsg>(&l).ok()) {
        Some(ClientMsg::Hello { app, pid, exe }) => lock(&shared.sessions).open(app, Client::Native { pid, exe }),
        _ => return,
    };
    let welcome = Welcome { categories: engine::available_categories(shared) };
    let mut reply = serde_json::to_vec(&welcome).expect("welcome serializes");
    reply.push(b'\n');

    if writer.write_all(&reply).is_ok() {
        for line in lines {
            let Ok(line) = line else { break };
            match serde_json::from_str::<ClientMsg>(&line) {
                Ok(ClientMsg::Effect { category, effect }) => {
                    lock(&shared.sessions).show(session, category, effect);
                }
                Ok(ClientMsg::Hello { .. } | ClientMsg::Ping) => {}
                Err(e) => log::warn!("bad message from SDK client: {e}"),
            }
        }
    }
    lock(&shared.sessions).close(session);
}
