//! Apps that have used the Chroma SDK, and whether each may take over the
//! lights.
//!
//! Kept in `apps.json` next to the config rather than in it: the service
//! updates it whenever an app connects, while the UI rewrites the config as a
//! whole, and the two must not overwrite each other.

use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// Apps beyond this many, least recently seen first, are forgotten.
const MAX_APPS: usize = 50;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct KnownApp {
    /// The title the app reports, or its executable name. Used as the key.
    pub title: String,
    /// "native" or "rest".
    pub client: String,
    /// Unix seconds.
    pub last_seen: u64,
    pub allowed: bool,
}

#[derive(Default)]
pub struct Apps {
    list: Vec<KnownApp>,
}

fn path() -> PathBuf {
    crate::config::Config::dir().join("apps.json")
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

impl Apps {
    pub fn load() -> Apps {
        let list = fs::read_to_string(path()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
        Apps { list }
    }

    fn save(&self) {
        let result = fs::create_dir_all(crate::config::Config::dir())
            .and_then(|_| fs::write(path(), serde_json::to_string_pretty(&self.list).expect("apps serialize")));
        if let Err(e) = result {
            log::warn!("could not save {}: {e}", path().display());
        }
    }

    /// Record that an app connected; returns whether it may take over.
    pub fn seen(&mut self, title: &str, client: &str) -> bool {
        let allowed = match self.list.iter_mut().find(|a| a.title == title) {
            Some(app) => {
                app.last_seen = now();
                app.client = client.to_string();
                app.allowed
            }
            None => {
                self.list.push(KnownApp { title: title.to_string(), client: client.to_string(), last_seen: now(), allowed: true });
                true
            }
        };
        self.list.sort_by_key(|a| std::cmp::Reverse(a.last_seen));
        self.list.truncate(MAX_APPS);
        self.save();
        allowed
    }

    /// Returns false if the app is unknown.
    pub fn set_allowed(&mut self, title: &str, allowed: bool) -> bool {
        match self.list.iter_mut().find(|a| a.title == title) {
            Some(app) => {
                app.allowed = allowed;
                self.save();
                true
            }
            None => false,
        }
    }

    pub fn list(&self) -> &[KnownApp] {
        &self.list
    }
}
