// Copyright (c) 2026 Ludovic Marie and the Horizon contributors.
// SPDX-License-Identifier: MIT
//! The skin servers the player has added, kept beside the accounts so every sign-in surface
//! offers the same list. A server is a Yggdrasil base URL and the name it reported (or the
//! player gave it); an account signed in through one records the same URL, so renewal and the
//! player's skin stay on that host.

use super::*;
use std::path::PathBuf;

/// One added skin server: where it signs in, and what to call it.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Server {
    /// The Yggdrasil base URL, no trailing slash.
    pub url: String,
    /// A display name: the server's own, or the host when it does not say.
    pub name: String,
}

/// The list of added skin servers, persisted as `servers.json` beside `accounts.json`.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct Servers {
    #[serde(default)]
    pub list: Vec<Server>,
}

impl Servers {
    fn path(dir: &Path) -> PathBuf {
        dir.join("servers.json")
    }
    pub fn load(dir: &Path) -> Self {
        std::fs::read_to_string(Self::path(dir))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }
    pub fn save(&self, dir: &Path) -> Result<(), String> {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        let text = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        let part = dir.join("servers.json.part");
        std::fs::write(&part, text + "\n").map_err(|e| e.to_string())?;
        std::fs::rename(part, Self::path(dir)).map_err(|e| e.to_string())
    }
    /// Adds a server, replacing one already at the same URL. Normalizes the URL (no trailing
    /// slash) and names it after its host when no name is given.
    pub fn add(&mut self, url: &str, name: &str) -> String {
        let url = url.trim().trim_end_matches('/').to_string();
        let name = if name.trim().is_empty() {
            host_of(&url)
        } else {
            name.trim().to_string()
        };
        self.list.retain(|s| s.url != url);
        self.list.push(Server { url: url.clone(), name });
        url
    }
    pub fn remove(&mut self, url: &str) {
        self.list.retain(|s| s.url != url);
    }
}

/// The host of a URL, for a default display name.
pub fn host_of(url: &str) -> String {
    url.trim_start_matches("https://")
        .trim_start_matches("http://")
        .split('/')
        .next()
        .unwrap_or(url)
        .to_string()
}
