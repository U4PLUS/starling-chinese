//! Rewrites Mojang's download URLs to a mirror (BMCLAPI by default), so the game, its
//! libraries, the sounds and Mojang's Java runtimes download from a host that is reachable
//! and fast where Mojang's own CDN is slow or blocked, without touching any of the checking.
//!
//! Every file is still verified against the sha1 Mojang publishes (`sources::fetch_checked`,
//! `mojang::fetch_file`), so a mirror serving a wrong byte is refused, never silently kept.
//! Only the host a byte comes from changes; what is expected of it does not.
//!
//! The mapping was measured against `bmclapi2.bangbang93.com` (see the project's mirror test
//! notes): the client jar keeps Mojang's own path, but the libraries and the sounds move under
//! `/maven` and `/assets`. A host that lays them out differently only needs `set_base`.

use std::sync::Mutex;

/// The mirror downloads go through: `None` is Mojang's own hosts (no rewriting), `Some(host)`
/// is that host. Defaults to BMCLAPI, a public mirror of Mojang's game, library and sound files.
static HOST: Mutex<Option<String>> = Mutex::new(None);

/// Routes a Mojang content URL through the mirror. A URL on no known content host — the
/// metadata host, the Java runtime index, GitHub, anything else — is returned unchanged, as is
/// every URL once the mirror is off.
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
/// to use Mojang's own hosts. A trailing slash is dropped, so `…example.com/` and
/// `…example.com` behave the same.
pub fn set_base(base: Option<&str>) {
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
    HOST.lock().unwrap().clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// The mirror is one global, and cargo runs tests in parallel: serialize the ones that
    /// touch it, and have each establish the state it asserts before it asserts.
    static SERIAL: Mutex<()> = Mutex::new(());

    fn use_default() {
        set_base(Some("https://bmclapi2.bangbang93.com"));
    }

    #[test]
    fn the_client_jar_keeps_its_path_under_the_mirror() {
        let _g = SERIAL.lock().unwrap();
        use_default();
        assert_eq!(
            url("https://piston-data.mojang.com/v1/objects/e877…/client.jar"),
            "https://bmclapi2.bangbang93.com/v1/objects/e877…/client.jar"
        );
    }

    #[test]
    fn the_libraries_move_under_maven() {
        let _g = SERIAL.lock().unwrap();
        use_default();
        assert_eq!(
            url("https://libraries.minecraft.net/com/mojang/patchy/2.2.10/patchy-2.2.10.jar"),
            "https://bmclapi2.bangbang93.com/maven/com/mojang/patchy/2.2.10/patchy-2.2.10.jar"
        );
    }

    #[test]
    fn the_sounds_move_under_assets() {
        let _g = SERIAL.lock().unwrap();
        use_default();
        assert_eq!(
            url("https://resources.download.minecraft.net/b6/b62ca8ec…"),
            "https://bmclapi2.bangbang93.com/assets/b6/b62ca8ec…"
        );
    }

    #[test]
    fn a_host_that_is_not_content_is_left_alone() {
        let _g = SERIAL.lock().unwrap();
        use_default();
        // The metadata host stays official: it is small, and its JSON holds URLs the game reads.
        for u in [
            "https://piston-meta.mojang.com/v1/packages/deadbeef/26.3.json",
            "https://launchermeta.mojang.com/v1/products/java-runtime/x/all.json",
            "https://api.github.com/repos/x/y",
        ] {
            assert_eq!(url(u), u);
        }
    }

    #[test]
    fn a_custom_base_is_used_and_its_trailing_slash_dropped() {
        let _g = SERIAL.lock().unwrap();
        set_base(Some("https://mirror.example/"));
        assert_eq!(
            url("https://libraries.minecraft.net/a/b.jar"),
            "https://mirror.example/maven/a/b.jar"
        );
        assert_eq!(base().as_deref(), Some("https://mirror.example"));
    }

    #[test]
    fn turning_the_mirror_off_restores_mojang() {
        let _g = SERIAL.lock().unwrap();
        set_base(None);
        assert_eq!(base(), None);
        assert_eq!(
            url("https://libraries.minecraft.net/a/b.jar"),
            "https://libraries.minecraft.net/a/b.jar"
        );
    }

    #[test]
    fn an_empty_base_means_mojang_too() {
        let _g = SERIAL.lock().unwrap();
        set_base(Some("  "));
        assert_eq!(base(), None);
    }
}
