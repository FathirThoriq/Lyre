//! Diagnostics & runtime dependencies.
//!
//! Lyre shells out to two external binaries for YouTube Music
//! search/playback: yt-dlp (resolve) and mpv (play). v0.1.1 failed
//! silently when they were missing; this module makes their state
//! explicit everywhere:
//! - `check_deps` scans PATH (used at startup, on the splash screen,
//!   and by the in-app installer behind key `i`).
//! - `doctor_report` powers `lyre doctor` for remote diagnosis.
//! - `resolve_test` powers `lyre resolve-test "Artist - Title"`.
//! - `append_log` writes playback/dependency events to
//!   <config_dir>/lyre.log. Tokens are NEVER passed to the log.

use std::io::Write;
use std::path::PathBuf;
use std::process::Command;

use crate::backend::PlayerBackend;
use crate::config::lyre_config_dir;

// ---------------------------------------------------------------------------
// Log file
// ---------------------------------------------------------------------------

pub fn log_path() -> PathBuf {
    lyre_config_dir().join("lyre.log")
}

/// Serializes tests that mutate process-wide environment
/// variables (HOME / PATH / stub-endpoint overrides) so they can
/// never interleave with each other.
#[cfg(test)]
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
pub fn env_lock() -> std::sync::MutexGuard<'static, ()> {
    ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// Append one timestamped line to lyre.log. Best-effort: logging must
/// never break playback. Callers must not pass tokens/secrets.
pub fn append_log(msg: &str) {
    let path = log_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(f, "[{}] {}", ts, msg);
    }
}

// ---------------------------------------------------------------------------
// Dependency detection (PATH scan - pure enough to unit-test with fake dirs)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct DepStatus {
    pub mpv: bool,
    pub yt_dlp: bool,
}

impl DepStatus {
    pub fn missing(&self) -> Vec<&'static str> {
        let mut v = Vec::new();
        if !self.mpv {
            v.push("mpv");
        }
        if !self.yt_dlp {
            v.push("yt-dlp");
        }
        v
    }
    pub fn all_present(&self) -> bool {
        self.mpv && self.yt_dlp
    }
    /// The exact in-UI warning line (splash + status).
    pub fn warning(&self) -> Option<String> {
        let m = self.missing();
        if m.is_empty() {
            None
        } else {
            Some(format!(
                "Missing dependencies: {} - press i to install automatically",
                m.join(", ")
            ))
        }
    }
}

fn binary_in_path(name: &str, path_env: &str) -> bool {
    let candidates: Vec<String> = if cfg!(target_os = "windows") {
        vec![format!("{}.exe", name), name.to_string()]
    } else {
        vec![name.to_string()]
    };
    for dir in std::env::split_paths(path_env) {
        for cand in &candidates {
            let p = dir.join(cand);
            if p.is_file() {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    if let Ok(meta) = p.metadata() {
                        if meta.permissions().mode() & 0o111 != 0 {
                            return true;
                        }
                        continue;
                    }
                }
                return true;
            }
        }
    }
    false
}

/// Scan an explicit PATH string (tests pass fake dirs).
pub fn check_deps_with(path_env: &str) -> DepStatus {
    DepStatus {
        mpv: binary_in_path("mpv", path_env),
        yt_dlp: binary_in_path("yt-dlp", path_env),
    }
}

pub fn check_deps() -> DepStatus {
    let path = std::env::var_os("PATH").map(|p| p.to_string_lossy().to_string()).unwrap_or_default();
    check_deps_with(&path)
}

/// cava's version line: it uses `-v` (not `--version`) and prefixes
/// an OSC title escape; some distro builds print an empty version,
/// in which case this returns None and doctor says "found in PATH".
fn cava_version() -> Option<String> {
    let out = Command::new("cava").arg("-v").output().ok()?;
    let s = String::from_utf8_lossy(&out.stdout);
    let first = s.lines().next()?;
    // Strip escape sequences (ESC ... BEL / ESC ... letter).
    let mut clean = String::new();
    let mut chars = first.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            for c2 in chars.by_ref() {
                if c2 == '\u{7}' || c2.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            clean.push(c);
        }
    }
    let clean = clean.trim().to_string();
    // Only trust it when it actually carries a version number.
    if clean.chars().any(|c| c.is_ascii_digit()) {
        Some(clean)
    } else {
        None
    }
}

/// First line of `<name> --version`, when the binary runs.
pub fn binary_version(name: &str) -> Option<String> {
    let out = Command::new(name).arg("--version").output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout);
    s.lines().next().map(|l| l.trim().to_string()).filter(|l| !l.is_empty())
}

// ---------------------------------------------------------------------------
// Distro detection + install commands (mirrors scripts/install-deps.sh)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Distro {
    Fedora,
    Debian,
    Arch,
    Windows,
    Unknown,
}

pub fn detect_distro() -> Distro {
    if cfg!(target_os = "windows") {
        return Distro::Windows;
    }
    let osr = std::fs::read_to_string("/etc/os-release").unwrap_or_default().to_lowercase();
    if osr.contains("fedora") || osr.contains("rhel") || osr.contains("centos") {
        Distro::Fedora
    } else if osr.contains("debian") || osr.contains("ubuntu") || osr.contains("linuxmint") {
        Distro::Debian
    } else if osr.contains("arch") || osr.contains("manjaro") || osr.contains("endeavouros") {
        Distro::Arch
    } else {
        Distro::Unknown
    }
}

/// Argv lists (no shell strings) that install the missing binaries for
/// a distro. Linux package names for mpv + yt-dlp coincide on
/// Fedora/Debian/Arch, which keeps this simple.
pub fn install_commands(distro: Distro, missing: &[&str]) -> Vec<Vec<String>> {
    if missing.is_empty() {
        return Vec::new();
    }
    let pkgs: Vec<&str> = missing.to_vec();
    match distro {
        Distro::Fedora => vec![sudo_cmd(&["dnf", "install", "-y"], &pkgs)],
        Distro::Debian => vec![
            vec!["sudo".into(), "apt".into(), "update".into()],
            sudo_cmd(&["apt", "install", "-y"], &pkgs),
        ],
        Distro::Arch => vec![sudo_cmd(&["pacman", "-S", "--needed", "--noconfirm"], &pkgs)],
        Distro::Windows => missing
            .iter()
            .map(|m| {
                let id = if *m == "yt-dlp" { "yt-dlp.yt-dlp" } else { "mpv-player.mpv" };
                vec!["winget".into(), "install".into(), "--id".into(), id.into(), "-e".into(), "--accept-source-agreements".into(), "--accept-package-agreements".into()]
            })
            .collect(),
        Distro::Unknown => Vec::new(),
    }
}

fn sudo_cmd(base: &[&str], pkgs: &[&str]) -> Vec<String> {
    let mut v: Vec<String> = vec!["sudo".to_string()];
    v.extend(base.iter().map(|s| s.to_string()));
    v.extend(pkgs.iter().map(|s| s.to_string()));
    v
}

/// Run the install interactively in the REAL terminal (the caller -
/// main.rs - leaves the alternate screen / raw mode first, and waits
/// for Enter before restoring the TUI). Returns true when every
/// command exited 0.
pub fn run_install(missing: &[&str]) -> bool {
    let distro = detect_distro();
    let cmds = install_commands(distro, missing);
    if cmds.is_empty() {
        println!("Distro not recognized automatically. Install manually: mpv, yt-dlp and cava");
        println!("Fedora: sudo dnf install mpv yt-dlp cava");
        println!("Debian: sudo apt install mpv yt-dlp cava");
        println!("Arch:   sudo pacman -S mpv yt-dlp cava");
        println!("Windows: winget install yt-dlp.yt-dlp ; winget install mpv-player.mpv");
        append_log(&format!("auto-install: distro not recognized, missing={:?}", missing));
        return false;
    }
    println!("Installing Lyre's missing dependencies: {}", missing.join(", "));
    let mut ok = true;
    for argv in &cmds {
        println!("$ {}", argv.join(" "));
        match Command::new(&argv[0]).args(&argv[1..]).status() {
            Ok(s) if s.success() => {}
            Ok(s) => {
                println!("Command failed (code {:?}).", s.code());
                ok = false;
                break;
            }
            Err(e) => {
                println!("Could not run {}: {}", argv[0], e);
                ok = false;
                break;
            }
        }
    }
    if ok {
        println!("Install finished.");
        if let Some(v) = binary_version("mpv") {
            println!("mpv: {}", v);
        }
        if let Some(v) = binary_version("yt-dlp") {
            println!("yt-dlp: {}", v);
        }
    }
    append_log(&format!("auto-install finished: ok={} missing={:?}", ok, missing));
    ok
}

// ---------------------------------------------------------------------------
// `lyre doctor`
// ---------------------------------------------------------------------------

fn lrclib_reachable() -> bool {
    // Any HTTP response (even an error status) proves reachability;
    // only a transport failure counts as unreachable.
    match ureq::get("https://lrclib.net/api/get?artist_name=Tulus&track_name=Monokrom")
        .timeout(std::time::Duration::from_secs(5))
        .call()
    {
        Ok(_) => true,
        Err(ureq::Error::Status(_, _)) => true,
        Err(_) => false,
    }
}

/// Guest-source health for `lyre doctor` (v0.1.5: there are no
/// accounts to report on). Each probe makes one small real request
/// and reports what actually happened. v0.1.7: the Spotify line
/// also reports the rate-limit state - while a 429 cooldown runs
/// (persisted across restarts) the doctor shows the retry time and
/// makes NO probe call, so checking health never breaks the
/// budget it reports on.
fn guest_source_lines() -> Vec<String> {
    let backend = crate::backend::SpotifyBackend::new();
    let spot = if let Some(until) = backend.guest.cooldown_until() {
        let (_, _, step, status) = backend.guest.doctor_state();
        match step {
            Some(step) => format!(
                "[RATE LIMITED] Spotify (Guest Mode): {} (last block: step {}, HTTP {})",
                crate::backend::rate_limit_note(until),
                step,
                status
            ),
            None => format!(
                "[RATE LIMITED] Spotify (Guest Mode): {}",
                crate::backend::rate_limit_note(until)
            ),
        }
    } else {
        match backend.probe() {
            Ok(msg) => format!("[OK] Spotify (Guest Mode): {}", msg),
            Err(e) => format!("[FAILED] Spotify (Guest Mode): {}", e),
        }
    };
    let ytm = match crate::innertube::probe() {
        Ok(msg) => format!("[OK] YouTube Music (guest Innertube): {}", msg),
        Err(e) => format!("[FAILED] YouTube Music (guest Innertube): {}", e),
    };
    vec![spot, ytm]
}

pub fn doctor_report() -> String {
    doctor_report_inner(&guest_source_lines(), Some(lrclib_reachable()))
}

fn doctor_report_inner(guest_lines: &[String], lrclib: Option<bool>) -> String {
    let deps = check_deps();
    let mut out = String::new();
    out.push_str(&format!("Lyre doctor - lyre {}\n", env!("CARGO_PKG_VERSION")));
    let dep_line = |name: &str, present: bool| {
        if present {
            match binary_version(name) {
                Some(v) => format!("[OK] {}: {}", name, v),
                None => format!("[OK] {}: found in PATH", name),
            }
        } else {
            format!("[MISSING] {}: not found in PATH", name)
        }
    };
    out.push_str(&format!("{}\n", dep_line("mpv", deps.mpv)));
    out.push_str(&format!("{}\n", dep_line("yt-dlp", deps.yt_dlp)));
    // cava powers only the sidebar visualizer: reported with its
    // version, but its absence never blocks playback.
    let cava_present = crate::backend::binary_on_path("cava");
    if cava_present {
        match cava_version() {
            Some(v) => out.push_str(&format!("[OK] cava: {}\n", v)),
            None => out.push_str("[OK] cava: found in PATH\n"),
        }
    } else {
        out.push_str("[MISSING] cava: not found in PATH (only the visualizer needs it)\n");
    }
    out.push_str(&format!("Config dir: {}\n", lyre_config_dir().display()));
    out.push_str("Mode: Guest (no login, no accounts)\n");
    for line in guest_lines {
        out.push_str(&format!("{}\n", line));
    }
    out.push_str(&format!(
        "LRCLib reachable: {}\n",
        match lrclib {
            Some(true) => "yes",
            Some(false) => "no",
            None => "not checked",
        }
    ));
    out.push_str(&format!("Log file: {}\n", log_path().display()));
    out
}

// ---------------------------------------------------------------------------
// `lyre resolve-test "Artist - Title"`
// ---------------------------------------------------------------------------

/// Resolve a query through the REAL yt-dlp path (search + stream URL)
/// and describe the match. The stream URL itself is never printed in
/// full - host + scheme only (URLs can carry signed parameters).
pub fn resolve_test(query: &str) -> Result<String, String> {
    let yt = crate::backend::YtMusicBackend;
    let settings = crate::config::Settings::default();
    let results = yt.search(query)?;
    let first = results
        .first()
        .cloned()
        .ok_or_else(|| format!("No matching result for '{}'", query))?;
    let url = yt.resolve_stream(&first, &settings)?;
    let host = url
        .split("://")
        .nth(1)
        .and_then(|rest| rest.split('/').next())
        .unwrap_or("(host unreadable)")
        .to_string();
    let report = format!(
        "Query: {}\nMatch: {} - {} ({}:{:02})\nStream: https host {} (signed URL, not printed in full)\nyt-dlp: {}",
        query,
        first.artist,
        first.title,
        first.duration_secs / 60,
        first.duration_secs % 60,
        host,
        binary_version("yt-dlp").unwrap_or_else(|| "unknown".to_string())
    );
    append_log(&format!("resolve-test '{}' -> {} - {} via host {}", query, first.artist, first.title, host));
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    fn fake_bin_dir(names: &[&str]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lyre-diag-test-{}-{}", std::process::id(), names.join("_")));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for n in names {
            let p = dir.join(n);
            let mut f = std::fs::File::create(&p).unwrap();
            writeln!(f, "#!/bin/sh\nexit 0").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
        }
        dir
    }

    #[test]
    fn dep_detection_with_fake_path_dirs() {
        // Both present.
        let dir = fake_bin_dir(&["mpv", "yt-dlp"]);
        let path = dir.to_string_lossy().to_string();
        let st = check_deps_with(&path);
        assert!(st.mpv && st.yt_dlp && st.all_present());
        assert!(st.warning().is_none());
        // Only mpv present -> yt-dlp reported missing by name.
        let dir2 = fake_bin_dir(&["mpv"]);
        let st2 = check_deps_with(&dir2.to_string_lossy().to_string());
        assert!(st2.mpv && !st2.yt_dlp);
        assert_eq!(st2.missing(), vec!["yt-dlp"]);
        let w = st2.warning().unwrap();
        assert!(w.contains("yt-dlp") && w.contains("press i"), "warning: {}", w);
        // Empty PATH -> both missing.
        let st3 = check_deps_with("");
        assert_eq!(st3.missing(), vec!["mpv", "yt-dlp"]);
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&dir2);
    }

    #[test]
    fn install_commands_per_distro() {
        let f = install_commands(Distro::Fedora, &["mpv", "yt-dlp"]);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0], vec!["sudo", "dnf", "install", "-y", "mpv", "yt-dlp"]);
        let d = install_commands(Distro::Debian, &["yt-dlp"]);
        assert_eq!(d.len(), 2);
        assert!(d[1].contains(&"apt".to_string()) && d[1].contains(&"yt-dlp".to_string()));
        let a = install_commands(Distro::Arch, &["mpv"]);
        assert!(a[0].contains(&"pacman".to_string()));
        let w = install_commands(Distro::Windows, &["mpv", "yt-dlp"]);
        assert_eq!(w.len(), 2);
        assert!(install_commands(Distro::Fedora, &[]).is_empty());
        assert!(install_commands(Distro::Unknown, &["mpv"]).is_empty());
    }

    #[test]
    fn doctor_report_has_all_sections() {
        let guest = vec![
            "[OK] Spotify (Guest Mode): anonymous token fetched via get_access_token".to_string(),
            "[OK] YouTube Music (guest Innertube): reachable".to_string(),
        ];
        let r = doctor_report_inner(&guest, Some(true));
        for needle in ["mpv", "yt-dlp", "cava", "Config dir:", "Mode: Guest", "Spotify (Guest Mode):", "YouTube Music (guest Innertube):", "LRCLib reachable: yes", "Log file:", "lyre.log"] {
            assert!(r.contains(needle), "doctor report missing '{}':\n{}", needle, r);
        }
        // Guest-only: no account/login language anywhere.
        assert!(!r.contains("belum login") && !r.contains("terhubung ("), "report: {}", r);
    }
}
