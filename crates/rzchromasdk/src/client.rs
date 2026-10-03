//! Connection to the OpenChroma service.
//!
//! SDK calls arrive on the game's threads, often its render thread, so they
//! never touch the pipe directly. They record the latest effect per category
//! and wake a background sender; if the service is slow, intermediate frames
//! are coalesced instead of stalling the game. If the service goes away the
//! sender keeps retrying and replays the current state once it is back.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::os::windows::fs::OpenOptionsExt;
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use chroma_proto::{AppInfo, Category, ClientMsg, SdkEffect, Welcome, PIPE_CLIENT_ACCESS, PIPE_NAME};

const RETRY_INTERVAL: Duration = Duration::from_secs(2);
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(2);

#[derive(Default)]
struct State {
    hello: Option<ClientMsg>,
    /// Effects not yet written to the pipe.
    pending: HashMap<Category, SdkEffect>,
    /// Everything the app has shown, replayed after a reconnect.
    current: HashMap<Category, SdkEffect>,
    welcome: Option<Welcome>,
    /// Set once the first connection attempt has finished, success or not.
    attempted: bool,
    shutdown: bool,
    finished: bool,
}

pub struct Client {
    shared: Arc<(Mutex<State>, Condvar)>,
}

impl Client {
    /// Start a session. Waits briefly for the first connection so that a
    /// `QueryDevice` right after `Init` sees real device availability.
    pub fn start(app: AppInfo) -> Client {
        let hello = ClientMsg::Hello {
            app,
            pid: std::process::id(),
            exe: std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_default(),
        };
        let shared = Arc::new((Mutex::new(State { hello: Some(hello), ..Default::default() }), Condvar::new()));
        {
            let shared = shared.clone();
            let _ = thread::Builder::new().name("openchroma-sdk".into()).spawn(move || {
                run(&shared);
                shared.0.lock().unwrap().finished = true;
                shared.1.notify_all();
            });
        }

        let (lock, cvar) = &*shared;
        let guard = lock.lock().unwrap();
        let _ = cvar.wait_timeout_while(guard, Duration::from_millis(500), |s| !s.attempted);

        Client { shared }
    }

    pub fn show(&self, category: Category, effect: SdkEffect) {
        let (lock, cvar) = &*self.shared;
        let mut s = lock.lock().unwrap();
        s.current.insert(category, effect.clone());
        s.pending.insert(category, effect);
        cvar.notify_all();
    }

    /// Categories backed by hardware, or `None` if the service is unreachable.
    pub fn categories(&self) -> Option<Vec<Category>> {
        self.shared.0.lock().unwrap().welcome.as_ref().map(|w| w.categories.clone())
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        // Give the sender a moment to flush the final state (typically a
        // clear) before the pipe closes, which ends the session. Never wait
        // long: this runs on the game's thread.
        let (lock, cvar) = &*self.shared;
        let mut s = lock.lock().unwrap();
        s.shutdown = true;
        cvar.notify_all();
        let _ = cvar.wait_timeout_while(s, Duration::from_millis(250), |s| !s.finished);
    }
}

struct Conn {
    pipe: File,
}

impl Conn {
    fn open(hello: &ClientMsg) -> Option<(Conn, Welcome)> {
        let pipe = OpenOptions::new().access_mode(PIPE_CLIENT_ACCESS).open(PIPE_NAME).ok()?;
        let mut conn = Conn { pipe };
        conn.send(hello).ok()?;
        let mut line = String::new();
        BufReader::new(&conn.pipe).read_line(&mut line).ok()?;
        let welcome = serde_json::from_str(&line).ok()?;
        Some((conn, welcome))
    }

    fn send(&mut self, msg: &ClientMsg) -> std::io::Result<()> {
        let mut line = serde_json::to_vec(msg).expect("ClientMsg serializes");
        line.push(b'\n');
        self.pipe.write_all(&line)
    }
}

fn run(shared: &(Mutex<State>, Condvar)) {
    let (lock, cvar) = shared;
    let mut conn: Option<Conn> = None;
    let mut next_attempt = Instant::now();

    loop {
        if conn.is_none() && Instant::now() >= next_attempt {
            let Some(hello) = lock.lock().unwrap().hello.clone() else { return };
            let opened = Conn::open(&hello);
            next_attempt = Instant::now() + RETRY_INTERVAL;
            let mut s = lock.lock().unwrap();
            s.attempted = true;
            s.welcome = opened.as_ref().map(|(_, w)| w.clone());
            if let Some((c, _)) = opened {
                // Replay everything shown so far; the service may have restarted.
                s.pending = s.current.clone();
                conn = Some(c);
            }
            cvar.notify_all();
        }

        // Wait for new effects, the keepalive, or the next reconnect slot.
        let (batch, shutdown) = {
            let wait = if conn.is_some() { KEEPALIVE_INTERVAL } else { next_attempt.saturating_duration_since(Instant::now()) };
            let s = lock.lock().unwrap();
            let (mut s, _) = cvar.wait_timeout_while(s, wait, |s| s.pending.is_empty() && !s.shutdown).unwrap();
            (s.pending.drain().collect::<Vec<_>>(), s.shutdown)
        };

        if let Some(c) = conn.as_mut() {
            // An idle ping is how a dead service gets noticed when the app
            // is not sending anything (e.g. a static effect).
            let sent = if batch.is_empty() {
                shutdown || c.send(&ClientMsg::Ping).is_ok()
            } else {
                batch.into_iter().all(|(category, effect)| c.send(&ClientMsg::Effect { category, effect }).is_ok())
            };
            if !sent {
                // Dropped effects are still in `current` and get replayed.
                conn = None;
                lock.lock().unwrap().welcome = None;
            }
        }

        if shutdown {
            return;
        }
    }
}
