//! Rewrites Mojang's download URLs to a mirror (BMCLAPI by default), so the game, its
//! libraries and the sounds download from a host that is reachable and fast where Mojang's
//! own CDN is slow or blocked, without touching any of the checking.
//!
//! Every file is still verified against the sha1 the version JSON publishes (`fetch_checked`),
//! so a mirror serving a wrong byte is refused, never silently kept. Only the host a byte comes
//! from changes; what is expected of it does not.
//!
//! The mapping was measured against `bmclapi2.bangbang93.com`: the client jar keeps Mojang's
//! own path, but the libraries move under `/maven` and the sounds under `/assets`. The version
//! JSON and the asset index are NOT rewritten: BMCLAPI re-serializes those JSON documents, so
//! their bytes (and the sha1 the lock pins) differ from Mojang's, and a mirror copy would fail
//! the check. They stay on Mojang's own host.
//!
//! On Android there is no settings screen to point the mirror, so it defaults to BMCLAPI and is
//! always on; `set_base` can still override it (or turn it off with `None`).

use std::sync::Mutex;

/// The mirror downloads go through. Starts on BMCLAPI, a public mirror of Mojang's game,
/// library and sound files; `set_base(None)` falls back to Mojang's own hosts.
static HOST: Mutex<Option<String>> = Mutex::new(Some(String::new()));

/// Whether the built-in default has been replaced by `set_base`.
static SET: Mutex<bool> = Mutex::new(false);

const BMCLAPI: &str = "https://bmclapi2.bangbang93.com";

/// Routes a Mojang content URL through the mirror. A URL on no known content host — the
/// metadata host, the asset index, GitHub, anything else — is returned unchanged.
///
/// Three shapes are known (each verified byte-for-byte against BMCLAPI):
/// - the client jar, on `piston-data`, keeps its path: `/v1/objects/<sha1>/<name>`
/// - the libraries, on `libraries.minecraft.net`, move under `/maven`
/// - the sounds, on `resources.download.minecraft.net`, move under `/assets`
pub fn url(url: &str) -> String {
    let Some(host) = host() else {
        return url.to_string();
    };
    let rest = if let Some(rest) = url.strip_prefix("https://piston-data.mojang.com/") {
        rest
    } else if let Some(rest) = url.strip_prefix("https://libraries.minecraft.net/") {
        return format!("{host}/maven/{rest}");
    } else if let Some(rest) = url.strip_prefix("https://resources.download.minecraft.net/") {
        return format!("{host}/assets/{rest}");
    } else {
        return url.to_string();
    };
    format!("{host}/{rest}")
}

/// Points the mirror at `base`: a host (`https://…`) to download Mojang's content from, or none
/// to use Mojang's own hosts. A trailing slash is dropped.
pub fn set_base(base: Option<&str>) {
    *SET.lock().unwrap() = true;
    let mut host = HOST.lock().unwrap();
    *host = match base.map(str::trim).filter(|base| !base.is_empty()) {
        Some(host) => Some(host.trim_end_matches('/').to_string()),
        None => None,
    };
}

/// The mirror in use, or none when the downloads go to Mojang directly.
pub fn base() -> Option<String> {
    host()
}

fn host() -> Option<String> {
    let set = *SET.lock().unwrap();
    let host = HOST.lock().unwrap().clone();
    if set {
        host
    } else {
        Some(BMCLAPI.to_string())
    }
}
