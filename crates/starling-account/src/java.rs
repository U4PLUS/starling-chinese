//! The Java runtime that prepares the game files on the device, where none is installed (a
//! phone): OpenJDK 25 built for Android (GPLv2 with the Classpath Exception; publisher/source
//! references are recorded in THIRD_PARTY_NOTICES.md).
//! It is fetched once, checked against the hash pinned here, and unpacked in a folder of its own;
//! the preparer runs Mojang's code in it (horizon-prepare's exporters), and it can go afterwards.
use crate::files::Failure;
use sha2::{Digest, Sha256};
use std::io::{BufReader, Read, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::path::{Component, Path, PathBuf};

/// A front that mirrors GitHub release downloads from inside networks that reach GitHub
/// slowly; it serves the exact bytes, which the SHA-256 pin below still checks.
const MIRROR: &str = "https://gh-proxy.com/";

/// A runtime archive (`.tar.xz`) and what it must be.
pub struct Runtime {
    pub url: &'static str,
    pub sha256: &'static str,
    pub size: u64,
}

/// OpenJDK 25.0.5 for Android on 64-bit ARM (its release `download_jre25`).
pub const JRE25_ANDROID_ARM64: Runtime = Runtime {
    url: "https://github.com/AngelAuraMC/angelauramc-openjdk-build/releases/download/download_jre25/jre25-android-arm64.tar.xz",
    sha256: "d3eb7afe2240c26728a1bb440502c5f18ac3883e932d202dd7f0c9bcbbce4c37",
    size: 38_031_580,
};

/// OpenJDK 25.0.4 for Android ARMv7, pinned to EclipseJRE's published release.
/// The AngelAuraMC ARM32 25.0.5 build returned incorrect floating-point results
/// in on-device tests, including Math.pow(2, -1), and could not generate reports.
pub const JRE25_ANDROID_ARM: Runtime = Runtime {
    url: "https://github.com/SouhardyaMaybe/EclipseJRE/releases/download/jres-v1.0.0/jre25-arm-20261009-release.tar.xz",
    sha256: "528457305c073128b0e0e2478e2b0c41af1c92a0621061cce7ecccd66bdd103d",
    size: 26_646_332,
};

/// OpenJDK 25 for Android x86_64, pinned to the publisher's release digest.
pub const JRE25_ANDROID_X86_64: Runtime = Runtime {
    url: "https://github.com/AngelAuraMC/angelauramc-openjdk-build/releases/download/download_jre25/jre25-android-x86_64.tar.xz",
    sha256: "7fca862ee1b2d5fe23cd9c9c3d9b7ad3c241947ad1a6cc9464ef2e674867105d",
    size: 39_061_384,
};

/// Published runtimes matching the process, never a runtime for another ABI.
pub fn android_runtime(arch: &str) -> Option<&'static Runtime> {
    match arch {
        "arm" => Some(&JRE25_ANDROID_ARM),
        "aarch64" => Some(&JRE25_ANDROID_ARM64),
        "x86_64" => Some(&JRE25_ANDROID_X86_64),
        _ => None,
    }
}

/// Written in the runtime's folder once it is unpacked: the archive's hash.
const INSTALLED: &str = ".starling-runtime";

/// Whether `runtime` is unpacked in `dir`.
pub fn installed(runtime: &Runtime, dir: &Path) -> bool {
    std::fs::read_to_string(dir.join(INSTALLED)).is_ok_and(|s| s.trim() == runtime.sha256)
}

/// `runtime` in `dir` (its `bin`, `lib`...), fetched and unpacked unless it is there already.
/// `progress` gets the bytes fetched and the archive's size; `cancelled` stops the download.
pub fn install(
    runtime: &Runtime,
    dir: &Path,
    progress: &dyn Fn(u64, u64),
    cancelled: &dyn Fn() -> bool,
) -> Result<(), String> {
    if installed(runtime, dir) {
        return Ok(());
    }
    let parent = dir.parent().ok_or("the runtime's folder has no parent")?;
    std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    let archive = dir.with_extension("tar.xz");
    fetch(runtime, &archive, progress, cancelled)?;
    let unpacked = unpack(&archive, dir);
    let _ = std::fs::remove_file(&archive);
    unpacked?;
    std::fs::write(dir.join(INSTALLED), runtime.sha256).map_err(|e| e.to_string())
}

/// The archive into `path`, through a `.part` file, checked as it streams; tried again from the
/// start when the network drops ([`crate::files::retrying`]).
fn fetch(
    runtime: &Runtime,
    path: &Path,
    progress: &dyn Fn(u64, u64),
    cancelled: &dyn Fn() -> bool,
) -> Result<(), String> {
    crate::files::retrying(|| {
        fetch_parallel(runtime, path, progress, cancelled).or_else(|_| {
            fetch_once(runtime, path, progress, cancelled)
        })
    })
}

/// The archive fetched in parallel pieces, which a front that throttles each connection
/// serves several times faster than one stream (measured ~2x on the mirror). Splits the
/// file at byte ranges the server has said it will honour, pulls the pieces concurrently
/// into `.part.N` siblings, and concatenates them into the `.part` the caller checks. A
/// server that refuses ranges (or a size it will not state) returns `None` and the caller
/// falls back to the single stream.
fn fetch_parallel(
    runtime: &Runtime,
    path: &Path,
    progress: &dyn Fn(u64, u64),
    cancelled: &dyn Fn() -> bool,
) -> Result<(), Failure> {
    const NAME: &str = "the Java runtime";
    const PIECES: usize = 8;
    const MIN_PARALLEL: u64 = 8 << 20;
    let Some((_, accepts_ranges)) = probe(runtime.url, NAME) else {
        return Err(Failure::Final(format!("{NAME}: could not be sized.")));
    };
    if !accepts_ranges || runtime.size < MIN_PARALLEL {
        return Err(Failure::Final(format!("{NAME}: not fetched in pieces.")));
    }
    let total = runtime.size;
    let bounds = chunk_bounds(total, PIECES);
    let disk = |e: String| Failure::Final(e);
    let fetched = Arc::new(AtomicU64::new(0));
    let stop = Arc::new(AtomicBool::new(false));
    thread::scope(|scope| {
        let mut handles = Vec::with_capacity(bounds.len());
        for (index, (start, end)) in bounds.iter().enumerate() {
            let fetched = Arc::clone(&fetched);
            let stop = Arc::clone(&stop);
            let piece = piece_path(path, index);
            handles.push(scope.spawn(move || {
                if stop.load(Ordering::Relaxed) {
                    return Err(Failure::Final("Stopped.".into()));
                }
                match fetch_range(runtime.url, *start, *end, &piece, &fetched, &stop) {
                    Ok(()) => Ok(()),
                    Err(e) => {
                        stop.store(true, Ordering::Relaxed);
                        let _ = std::fs::remove_file(&piece);
                        Err(e)
                    }
                }
            }));
        }
        // The pieces run concurrently; drive the progress bar off their shared counter while
        // they stream, so the callback is only ever touched from this thread.
        let mut first_error = None;
        for handle in handles {
            loop {
                match handle.is_finished() {
                    true => break,
                    false => {
                        if cancelled() {
                            stop.store(true, Ordering::Relaxed);
                        }
                        progress(fetched.load(Ordering::Relaxed), total);
                        thread::sleep(std::time::Duration::from_millis(200));
                    }
                }
            }
            if let Err(e) = handle.join().unwrap_or_else(|_| {
                Err(Failure::Final(format!("{NAME}: a piece failed to download.")))
            }) {
                first_error.get_or_insert(e);
            }
        }
        if let Some(e) = first_error {
            return Err(e);
        }
        // Every piece is on disk whole; concatenate them in order into the `.part`.
        let mut part = path.as_os_str().to_owned();
        part.push(".part");
        let part = PathBuf::from(part);
        let mut out = std::fs::File::create(&part)
            .map_err(|e| disk(format!("{}: {e}", part.display())))?;
        let mut written = 0u64;
        for index in 0..bounds.len() {
            let piece = piece_path(path, index);
            let bytes = std::fs::read(&piece)
                .map_err(|e| disk(format!("{}: {e}", piece.display())))?;
            written += bytes.len() as u64;
            out.write_all(&bytes)
                .map_err(|e| disk(format!("{}: {e}", part.display())))?;
            let _ = std::fs::remove_file(&piece);
        }
        if written != total {
            let _ = std::fs::remove_file(&part);
            return Err(Failure::Again(format!("The download of {NAME} was cut off.")));
        }
        // The caller unpacks `path` itself, so put the concatenated archive there — the same
        // rename the single stream does, without which `path` would never exist.
        std::fs::rename(&part, path).map_err(|e| disk(format!("{}: {e}", path.display())))?;
        progress(total, total);
        Ok(())
    })
}

/// `(start, end)` inclusive byte ranges for `pieces` chunks of a `total`-byte file.
fn chunk_bounds(total: u64, pieces: usize) -> Vec<(u64, u64)> {
    let base = total / pieces as u64;
    let extra = total % pieces as u64;
    let mut ranges = Vec::with_capacity(pieces);
    let mut start = 0u64;
    for index in 0..pieces {
        let length = base + if (index as u64) < extra { 1 } else { 0 };
        if length == 0 {
            break;
        }
        ranges.push((start, start + length - 1));
        start += length;
    }
    ranges
}

fn piece_path(path: &Path, index: usize) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(format!(".part{index}"));
    PathBuf::from(name)
}

/// One inclusive byte range into `piece`, appending (so a retried piece resumes), bumping
/// the shared fetched counter as it streams. `stop` is checked between reads so a sibling's
/// failure ends this piece promptly.
fn fetch_range(
    url: &str,
    start: u64,
    end: u64,
    piece: &Path,
    fetched: &AtomicU64,
    stop: &AtomicBool,
) -> Result<(), Failure> {
    const NAME: &str = "the Java runtime";
    let response = connect_range(url, start, end, NAME)?;
    let disk = |e: String| Failure::Final(e);
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(piece)
        .map_err(|e| disk(format!("{}: {e}", piece.display())))?;
    let mut reader = response.into_reader();
    let mut buffer = vec![0; 1 << 16];
    loop {
        if stop.load(Ordering::Relaxed) {
            return Err(Failure::Final("Stopped.".into()));
        }
        let n = reader
            .read(&mut buffer)
            .map_err(|_| Failure::Again(format!("The download of {NAME} was cut off.")))?;
        if n == 0 {
            break;
        }
        file.write_all(&buffer[..n])
            .map_err(|e| disk(format!("{}: {e}", piece.display())))?;
        fetched.fetch_add(n as u64, Ordering::Relaxed);
    }
    Ok(())
}

/// `GET` with a `Range`, trying the mirror in front of GitHub first (as [`connect`] does).
fn connect_range(url: &str, start: u64, end: u64, name: &str) -> Result<ureq::Response, Failure> {
    let header = format!("bytes={start}-{end}");
    let describe = |e: ureq::Error| match e {
        ureq::Error::Status(code, _) => (code, format!("GitHub refused {name} ({code}).")),
        ureq::Error::Transport(t) => {
            (0, format!("Could not reach {name} ({}).", crate::transport_detail(&t)))
        }
    };
    let mirror = format!("{MIRROR}{url}");
    let mirror_error = match crate::files::agent()
        .get(&mirror)
        .set("Range", &header)
        .call()
    {
        Ok(response) => return Ok(response),
        Err(e) => describe(e),
    };
    match crate::files::agent()
        .get(url)
        .set("Range", &header)
        .call()
    {
        Ok(response) => Ok(response),
        Err(e) => {
            let (code, message) = describe(e);
            if code != 0 && crate::files::may_pass(code) {
                Err(Failure::Again(format!("{message} Try again in a few minutes.")))
            } else if mirror_error.0 == 0 {
                Err(Failure::Again(format!(
                    "{} Check your internet connection.",
                    mirror_error.1
                )))
            } else {
                Err(Failure::Again(message))
            }
        }
    }
}

/// The file's size and whether the server honours byte ranges, from a 1-byte `Range` probe
/// (its `Content-Range: bytes 0-0/TOTAL` states the size). Tries the mirror, then GitHub.
fn probe(url: &str, _name: &str) -> Option<(u64, bool)> {
    for target in [format!("{MIRROR}{url}"), url.to_string()] {
        let response = match crate::files::agent()
            .get(&target)
            .set("Range", "bytes=0-0")
            .call()
        {
            Ok(response) => response,
            Err(_) => continue,
        };
        let status = response.status();
        if status == 206 {
            let total = response
                .header("Content-Range")
                .and_then(|value| value.rsplit('/').next())
                .and_then(|total| total.trim().parse::<u64>().ok());
            if let Some(total) = total {
                return Some((total, true));
            }
        } else if status == 200 {
            let total = response
                .header("Content-Length")
                .and_then(|value| value.trim().parse::<u64>().ok());
            if let Some(total) = total {
                return Some((total, false));
            }
        }
    }
    None
}

/// Opens `url` for reading, trying a mirror in front of GitHub first and GitHub itself when the
/// mirror refuses. A mirror that simply doesn't carry the file falls through to GitHub; when
/// GitHub's own servers refuse in a way time may fix (`may_pass`) that comes back as
/// [`Failure::Again`], and the SHA-256 pin still checks whatever either of them serves.
fn connect(url: &str, name: &str) -> Result<ureq::Response, Failure> {
    let describe = |e: ureq::Error| match e {
        ureq::Error::Status(code, _) => (code, format!("GitHub refused {name} ({code}).")),
        ureq::Error::Transport(t) => {
            (0, format!("Could not reach {name} ({}).", crate::transport_detail(&t)))
        }
    };
    let mirror = format!("{MIRROR}{url}");
    let mut mirror_error = match crate::files::agent().get(&mirror).call() {
        Ok(response) => return Ok(response),
        Err(e) => describe(e),
    };
    match crate::files::agent().get(url).call() {
        Ok(response) => Ok(response),
        Err(e) => {
            let (code, message) = describe(e);
            if code != 0 && crate::files::may_pass(code) {
                Err(Failure::Again(format!("{message} Try again in a few minutes.")))
            } else {
                if mirror_error.0 == 0 {
                    mirror_error.1.push_str(" Check your internet connection.");
                    return Err(Failure::Again(mirror_error.1));
                }
                Err(Failure::Again(message))
            }
        }
    }
}


fn fetch_once(
    runtime: &Runtime,
    path: &Path,
    progress: &dyn Fn(u64, u64),
    cancelled: &dyn Fn() -> bool,
) -> Result<(), Failure> {
    const NAME: &str = "the Java runtime";
    let response = connect(runtime.url, NAME)?;
    let disk = |e: String| Failure::Final(e);
    let mut part = path.as_os_str().to_owned();
    part.push(".part");
    let part = PathBuf::from(part);
    let mut file =
        std::fs::File::create(&part).map_err(|e| disk(format!("{}: {e}", part.display())))?;
    let mut reader = response.into_reader();
    let mut hasher = Sha256::new();
    let mut buffer = vec![0; 1 << 16];
    let mut fetched = 0u64;
    loop {
        if cancelled() {
            drop(file);
            let _ = std::fs::remove_file(&part);
            return Err(Failure::Final("Stopped.".into()));
        }
        let n = reader
            .read(&mut buffer)
            .map_err(|_| Failure::Again(format!("The download of {NAME} was cut off.")))?;
        if n == 0 {
            break;
        }
        fetched += n as u64;
        if fetched > runtime.size {
            break;
        }
        hasher.update(&buffer[..n]);
        file.write_all(&buffer[..n])
            .map_err(|e| disk(format!("{}: {e}", part.display())))?;
        progress(fetched, runtime.size);
    }
    drop(file);
    // Short: cut off on the way, worth another try. Whole but another file: never.
    if fetched < runtime.size {
        let _ = std::fs::remove_file(&part);
        return Err(Failure::Again(format!(
            "The download of {NAME} was cut off."
        )));
    }
    let digest: String = hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    if fetched != runtime.size || digest != runtime.sha256 {
        let _ = std::fs::remove_file(&part);
        return Err(Failure::Final(format!(
            "{NAME} is not the file Starling expects (its hash differs)."
        )));
    }
    std::fs::rename(&part, path).map_err(|e| disk(format!("{}: {e}", path.display())))
}

/// A `.tar.xz` into `dir`, through a sibling folder renamed once whole. Entries stay inside it;
/// links are made where the system has them (they are only the licence files' copies).
pub fn unpack(archive: &Path, dir: &Path) -> Result<(), String> {
    let mut staging = dir.as_os_str().to_owned();
    staging.push(".unpacking");
    let staging = PathBuf::from(staging);
    for old in [&staging, &dir.to_path_buf()] {
        if old.exists() {
            std::fs::remove_dir_all(old).map_err(|e| format!("{}: {e}", old.display()))?;
        }
    }
    std::fs::create_dir_all(&staging).map_err(|e| format!("{}: {e}", staging.display()))?;
    let tar = staging.with_extension("tar");
    let result = (|| {
        {
            let mut input = BufReader::new(
                std::fs::File::open(archive).map_err(|e| format!("{}: {e}", archive.display()))?,
            );
            let mut output = std::io::BufWriter::new(
                std::fs::File::create(&tar).map_err(|e| format!("{}: {e}", tar.display()))?,
            );
            lzma_rs::xz_decompress(&mut input, &mut output)
                .map_err(|e| format!("The Java runtime's archive is damaged: {e}"))?;
            output.flush().map_err(|e| e.to_string())?;
        }
        let file = std::fs::File::open(&tar).map_err(|e| format!("{}: {e}", tar.display()))?;
        let mut entries = tar::Archive::new(file);
        for entry in entries.entries().map_err(|e| e.to_string())? {
            let mut entry = entry.map_err(|e| e.to_string())?;
            let path = entry.path().map_err(|e| e.to_string())?.into_owned();
            if path
                .components()
                .any(|c| !matches!(c, Component::Normal(_) | Component::CurDir))
            {
                return Err(format!(
                    "The Java runtime's archive names {}",
                    path.display()
                ));
            }
            if entry.header().entry_type().is_symlink() && !cfg!(unix) {
                continue;
            }
            entry
                .unpack_in(&staging)
                .map_err(|e| format!("{}: {e}", path.display()))?;
        }
        Ok(())
    })();
    let _ = std::fs::remove_file(&tar);
    if let Err(e) = result {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(e);
    }
    std::fs::rename(&staging, dir).map_err(|e| format!("{}: {e}", dir.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("starling-java-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn archive(dir: &Path, entries: &[(&str, &[u8])], link: Option<(&str, &str)>) -> PathBuf {
        let mut builder = tar::Builder::new(Vec::new());
        for (name, data) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder.append_data(&mut header, name, *data).unwrap();
        }
        if let Some((name, target)) = link {
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(tar::EntryType::Symlink);
            header.set_size(0);
            builder.append_link(&mut header, name, target).unwrap();
        }
        let tar = builder.into_inner().unwrap();
        let mut xz = Vec::new();
        lzma_rs::xz_compress(&mut &tar[..], &mut xz).unwrap();
        let path = dir.join("runtime.tar.xz");
        std::fs::write(&path, xz).unwrap();
        path
    }

    #[test]
    fn a_runtime_unpacks_whole_into_its_folder() {
        let dir = temp("unpack");
        let archive = archive(
            &dir,
            &[
                ("./release", b"JAVA_VERSION=\"25\"\n"),
                ("./lib/libjli.so", b"elf"),
                ("./legal/java.base/LICENSE", b"GPL"),
            ],
            Some(("./legal/jdk.net/LICENSE", "../java.base/LICENSE")),
        );
        let jre = dir.join("jre");
        std::fs::create_dir_all(jre.join("stale")).unwrap();
        unpack(&archive, &jre).unwrap();
        assert_eq!(std::fs::read(jre.join("lib/libjli.so")).unwrap(), b"elf");
        assert!(
            !jre.join("stale").exists(),
            "an earlier unpacking is replaced"
        );
        if cfg!(unix) {
            assert_eq!(
                std::fs::read(jre.join("legal/jdk.net/LICENSE")).unwrap(),
                b"GPL"
            );
        }
        assert!(!dir.join("jre.unpacking").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A real Android runtime archive (STARLING_TEST_JRE), timed.
    #[test]
    #[ignore = "needs the downloaded runtime archive"]
    fn the_android_runtime_unpacks() {
        let archive = PathBuf::from(std::env::var("STARLING_TEST_JRE").unwrap());
        let dir = temp("real");
        let started = std::time::Instant::now();
        unpack(&archive, &dir.join("jre")).unwrap();
        eprintln!("unpacked in {:?}", started.elapsed());
        for file in ["lib/libjli.so", "lib/modules", "bin/java"] {
            assert!(dir.join("jre").join(file).is_file(), "{file}");
        }
        // JLI selects the Client VM on ARM32 and the Server VM on 64-bit.
        assert!(
            ["client", "server"].iter().any(|vm| dir
                .join("jre/lib")
                .join(vm)
                .join("libjvm.so")
                .is_file()),
            "the archive must contain a JVM that JLI can load"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn entries_outside_the_folder_are_refused() {
        let dir = temp("escape");
        let mut builder = tar::Builder::new(Vec::new());
        let mut header = tar::Header::new_gnu();
        header.set_size(1);
        header.set_mode(0o644);
        // append_data refuses `..`; the raw name goes through the header.
        header.as_gnu_mut().unwrap().name[..9].copy_from_slice(b"../escape");
        header.set_cksum();
        builder.append(&header, &b"x"[..]).unwrap();
        let tar = builder.into_inner().unwrap();
        let mut xz = Vec::new();
        lzma_rs::xz_compress(&mut &tar[..], &mut xz).unwrap();
        let archive = dir.join("bad.tar.xz");
        std::fs::write(&archive, xz).unwrap();
        let err = unpack(&archive, &dir.join("jre")).unwrap_err();
        assert!(err.contains("names"), "{err}");
        assert!(!dir.join("escape").exists() && !dir.join("jre").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod architecture_tests {
    use super::*;

    #[test]
    fn runtime_downloads_match_the_process_and_never_substitute_another_cpu() {
        for (arch, archive) in [
            ("arm", "jre25-arm-20261009-release.tar.xz"),
            ("aarch64", "jre25-android-arm64.tar.xz"),
            ("x86_64", "jre25-android-x86_64.tar.xz"),
        ] {
            assert!(android_runtime(arch).unwrap().url.ends_with(archive));
        }
        assert!(android_runtime("x86").is_none());
        assert!(android_runtime("unknown").is_none());
    }
}
