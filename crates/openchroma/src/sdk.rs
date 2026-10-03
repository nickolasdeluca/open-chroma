//! Chroma SDK client sessions, from both the native DLL and the REST API.
//!
//! The most recently started session of an allowed app owns the lights; a
//! blocked app keeps working but is ignored. Categories it has not
//! drawn on keep showing the user's profile, so a game that only lights the
//! keyboard leaves the case fans alone.

use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, Instant};

use chroma_proto::{AppInfo, Category, SdkEffect};
use serde::Serialize;

/// REST clients must send a heartbeat at least this often.
pub const REST_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Client {
    Native { pid: u32, exe: String },
    Rest,
}

#[derive(Debug)]
pub struct Session {
    pub id: u64,
    pub app: AppInfo,
    pub client: Client,
    pub started: Instant,
    pub last_seen: Instant,
    /// Current effect per category, with when it was set (for animations).
    pub shown: HashMap<Category, (SdkEffect, Instant)>,
    /// Effects created for later `SetEffect` (REST only; the DLL keeps its own).
    pub stored: HashMap<String, (Category, SdkEffect)>,
    pub heartbeats: u64,
    /// Whether the user lets this app take over the lights.
    pub allowed: bool,
}

#[derive(Default)]
pub struct Sessions {
    sessions: BTreeMap<u64, Session>,
    next_id: u64,
}

impl Sessions {
    pub fn open(&mut self, app: AppInfo, client: Client, allowed: bool) -> u64 {
        self.next_id += 1;
        let id = self.next_id;
        let now = Instant::now();
        log::info!("SDK session {id} started: {:?} ({client:?})", app.title);
        self.sessions.insert(
            id,
            Session {
                id,
                app,
                client,
                started: now,
                last_seen: now,
                shown: HashMap::new(),
                stored: HashMap::new(),
                heartbeats: 0,
                allowed,
            },
        );
        id
    }

    pub fn close(&mut self, id: u64) -> bool {
        match self.sessions.remove(&id) {
            Some(s) => {
                log::info!("SDK session {id} ended: {:?}", s.app.title);
                true
            }
            None => false,
        }
    }

    pub fn get_mut(&mut self, id: u64) -> Option<&mut Session> {
        let s = self.sessions.get_mut(&id)?;
        s.last_seen = Instant::now();
        Some(s)
    }

    pub fn show(&mut self, id: u64, category: Category, effect: SdkEffect) -> bool {
        match self.get_mut(id) {
            Some(s) => {
                s.shown.insert(category, (effect, Instant::now()));
                true
            }
            None => false,
        }
    }

    /// Drop REST sessions whose client stopped sending heartbeats.
    pub fn expire(&mut self) {
        let stale: Vec<u64> = self
            .sessions
            .values()
            .filter(|s| matches!(s.client, Client::Rest) && s.last_seen.elapsed() > REST_TIMEOUT)
            .map(|s| s.id)
            .collect();
        for id in stale {
            log::info!("SDK session {id} timed out");
            self.close(id);
        }
    }

    pub fn active(&self) -> Option<&Session> {
        self.sessions.values().rev().find(|s| s.allowed)
    }

    /// Apply an allow/block change to the app's live sessions.
    pub fn set_allowed(&mut self, title: &str, allowed: bool) {
        for s in self.sessions.values_mut().filter(|s| s.app.title == title) {
            s.allowed = allowed;
        }
    }

    pub fn all(&self) -> impl Iterator<Item = &Session> {
        self.sessions.values()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(title: &str) -> AppInfo {
        AppInfo { title: title.into(), ..Default::default() }
    }

    #[test]
    fn newest_session_wins_and_older_resumes() {
        let mut s = Sessions::default();
        let a = s.open(app("a"), Client::Rest, true);
        let b = s.open(app("b"), Client::Rest, true);
        assert_eq!(s.active().unwrap().id, b);
        s.close(b);
        assert_eq!(s.active().unwrap().id, a);
    }

    #[test]
    fn blocked_apps_never_take_over() {
        let mut s = Sessions::default();
        let a = s.open(app("a"), Client::Rest, true);
        s.open(app("b"), Client::Rest, false);
        assert_eq!(s.active().unwrap().id, a);
        s.set_allowed("a", false);
        assert!(s.active().is_none());
    }

    #[test]
    fn show_on_closed_session_is_rejected() {
        let mut s = Sessions::default();
        let a = s.open(app("a"), Client::Rest, true);
        s.close(a);
        assert!(!s.show(a, Category::Keyboard, SdkEffect::None));
    }
}
