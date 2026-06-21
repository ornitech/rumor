//! Session log capture: plain-text, ANSI-stripped copies of every process's
//! output, written under `<log base>/sessions/<config>-<timestamp>/`.
//!
//! Capture is best-effort and must never affect process management: every
//! failure here degrades to "no log file" with a `warn!`, never an error.
//!
//! Known limitations (deliberate):
//! - Per-file size is unbounded; the startup cleanup of sessions older than
//!   seven days is the mitigation.
//! - Full-screen TUI children (vim, htop) strip to garbled-but-harmless text;
//!   cursor positioning has no plain-text equivalent.
//! - Two rumor instances started the same second on the same config stem share
//!   a session dir; their appends interleave harmlessly.

use std::collections::HashMap;
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use tracing::warn;

use crate::config::ProcessConfig;

/// Make a process name safe to use as a file name: anything outside
/// `[A-Za-z0-9._-]` becomes `_`, leading dots are stripped (no hidden files),
/// and the result is capped at 100 chars. An empty result becomes "process".
pub fn sanitize_name(name: &str) -> String {
    let mut s: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    s = s.trim_start_matches('.').to_string();
    // All chars are ASCII after the map above, so this never splits a char.
    s.truncate(100);
    if s.is_empty() {
        "process".to_string()
    } else {
        s
    }
}

/// One log path per process slot. Config validation already rejects duplicate
/// raw names, so collisions only arise from sanitization ("a b" and "a_b");
/// those get `-2`, `-3`, ... suffixes.
pub fn assign_log_paths(configs: &[ProcessConfig], session_dir: &Path) -> Vec<Option<PathBuf>> {
    let mut used: HashMap<String, usize> = HashMap::new();
    configs
        .iter()
        .map(|c| {
            let base = sanitize_name(&c.name);
            let n = used.entry(base.clone()).or_insert(0);
            *n += 1;
            let file = if *n == 1 {
                format!("{base}.log")
            } else {
                format!("{base}-{n}.log")
            };
            Some(session_dir.join(file))
        })
        .collect()
}

/// Create `<sessions_root>/<config_stem>-<YYYYMMDD-HHMMSS>` for this run.
/// UTC, not local: `time`'s local-offset lookup refuses multithreaded
/// processes and the tokio runtime threads already exist by the time this
/// runs. On any failure, warn and return None (capture disabled for the run).
pub fn create_session_dir(sessions_root: &Path, config_stem: &str) -> Option<PathBuf> {
    let fmt = time::macros::format_description!("[year][month][day]-[hour][minute][second]");
    let stamp = time::OffsetDateTime::now_utc()
        .format(&fmt)
        .unwrap_or_else(|_| "unknown".to_string());
    let dir = sessions_root.join(format!("{}-{stamp}", sanitize_name(config_stem)));
    match std::fs::create_dir_all(&dir) {
        Ok(()) => Some(dir),
        Err(e) => {
            warn!(dir = %dir.display(), error = %e, "could not create session log dir; capture disabled");
            None
        }
    }
}

/// Delete session dirs whose newest mtime (the dir or any immediate child) is
/// older than `max_age`. Child mtimes matter because restart appends touch
/// files, not the dir, and a still-running old session must survive a second
/// rumor instance's cleanup. Every error is ignored.
pub fn cleanup_old_sessions(sessions_root: &Path, max_age: Duration) {
    let now = SystemTime::now();
    let Ok(entries) = std::fs::read_dir(sessions_root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(newest) = newest_mtime(&path) else {
            continue;
        };
        // duration_since errs on a future mtime; skip those.
        let Ok(age) = now.duration_since(newest) else {
            continue;
        };
        if age > max_age {
            let _ = std::fs::remove_dir_all(&path);
        }
    }
}

fn newest_mtime(dir: &Path) -> Option<SystemTime> {
    let mut newest = std::fs::metadata(dir).ok()?.modified().ok()?;
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            if let Some(m) = e.metadata().ok().and_then(|m| m.modified().ok()) {
                newest = newest.max(m);
            }
        }
    }
    Some(newest)
}

/// Accumulates the printable text of a VT byte stream. CR handling: `\r\n`
/// collapses to `\n`, and a lone `\r` (CR-overwrite progress bars) becomes
/// `\n` so each frame lands on its own line instead of one mega-line.
///
/// Shared by `AnsiLogWriter` (session logs) and the raw-mode line emitter
/// (`process.rs`) so both produce identical ANSI-stripped, CR-normalized text.
#[derive(Default)]
pub(crate) struct TextExtractor {
    pub(crate) out: Vec<u8>,
    pending_cr: bool,
}

impl TextExtractor {
    pub(crate) fn resolve_cr(&mut self) {
        if self.pending_cr {
            self.out.push(b'\n');
            self.pending_cr = false;
        }
    }
}

impl vte::Perform for TextExtractor {
    fn print(&mut self, c: char) {
        self.resolve_cr();
        let mut buf = [0u8; 4];
        self.out
            .extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            b'\n' => {
                self.pending_cr = false;
                self.out.push(b'\n');
            }
            b'\r' => {
                self.resolve_cr();
                self.pending_cr = true;
            }
            b'\t' => {
                self.resolve_cr();
                self.out.push(b'\t');
            }
            _ => {}
        }
    }
    // All escape-sequence dispatches (csi/osc/esc/dcs) default to no-ops,
    // which is exactly "strip them".
}

/// Strips ANSI escapes from PTY chunks and appends the plain text to a file.
/// The vte parser is stateful, so escape sequences and UTF-8 characters split
/// across chunk boundaries are handled by construction. Lives entirely on the
/// process's blocking read thread; no locks. Never reports errors to the
/// caller: a failed write disables capture for this process after one warn.
pub struct AnsiLogWriter {
    file: Option<File>,
    parser: vte::Parser,
    extractor: TextExtractor,
    name: String,
}

impl AnsiLogWriter {
    pub fn new(file: File, name: String) -> Self {
        Self {
            file: Some(file),
            parser: vte::Parser::new(),
            extractor: TextExtractor::default(),
            name,
        }
    }

    pub fn write_chunk(&mut self, bytes: &[u8]) {
        if self.file.is_none() {
            return;
        }
        self.parser.advance(&mut self.extractor, bytes);
        self.write_pending();
    }

    /// Resolve a trailing CR and flush. Call once when the PTY stream ends.
    pub fn flush(&mut self) {
        self.extractor.resolve_cr();
        self.write_pending();
        if let Some(f) = self.file.as_mut() {
            let _ = f.flush();
        }
    }

    fn write_pending(&mut self) {
        if self.extractor.out.is_empty() {
            return;
        }
        if let Some(f) = self.file.as_mut() {
            if let Err(e) = f.write_all(&self.extractor.out) {
                warn!(name = %self.name, error = %e, "session log write failed; disabling capture");
                self.file = None;
            }
        }
        self.extractor.out.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap as StdHashMap;

    fn tmpdir() -> PathBuf {
        // Timestamps alone can collide across parallel test threads (coarse
        // clock resolution), so disambiguate with a process-wide counter.
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let p = std::env::temp_dir().join(format!(
            "rumor-logfile-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn cfg(name: &str) -> ProcessConfig {
        ProcessConfig {
            name: name.into(),
            command: "true".into(),
            args: vec![],
            cwd: PathBuf::from("/"),
            env_files: vec![],
            global_env_files: vec![],
            env: StdHashMap::new(),
            dynamic_ports: StdHashMap::new(),
            depends_on: vec![],
            long_lived: true,
            tags: vec![],
        }
    }

    #[test]
    fn sanitize_name_cases() {
        assert_eq!(sanitize_name("web server"), "web_server");
        assert_eq!(sanitize_name("a/b"), "a_b");
        assert_eq!(sanitize_name("åäö"), "___");
        assert_eq!(sanitize_name(""), "process");
        assert_eq!(sanitize_name("..."), "process");
        assert_eq!(sanitize_name(".hidden"), "hidden");
        assert_eq!(sanitize_name(&"x".repeat(200)).len(), 100);
    }

    #[test]
    fn assign_log_paths_dedupes_sanitization_collisions() {
        let dir = PathBuf::from("/tmp/s");
        let paths = assign_log_paths(&[cfg("a b"), cfg("a_b")], &dir);
        assert_eq!(paths[0].as_deref(), Some(Path::new("/tmp/s/a_b.log")));
        assert_eq!(paths[1].as_deref(), Some(Path::new("/tmp/s/a_b-2.log")));
    }

    fn strip(chunks: &[&[u8]]) -> Vec<u8> {
        let dir = tmpdir();
        let path = dir.join("out.log");
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .unwrap();
        let mut w = AnsiLogWriter::new(file, "test".into());
        for c in chunks {
            w.write_chunk(c);
        }
        w.flush();
        let out = std::fs::read(&path).unwrap();
        std::fs::remove_dir_all(&dir).ok();
        out
    }

    #[test]
    fn strips_csi_color_codes() {
        assert_eq!(strip(&[b"\x1b[31mred\x1b[0m line\n"]), b"red line\n");
    }

    #[test]
    fn handles_escape_split_across_chunks() {
        assert_eq!(strip(&[b"\x1b[3", b"1mred\x1b[0m\n"]), b"red\n");
    }

    #[test]
    fn normalizes_crlf_and_lone_cr() {
        assert_eq!(strip(&[b"a\r\nb\n"]), b"a\nb\n");
        assert_eq!(strip(&[b"50%\r100%\n"]), b"50%\n100%\n");
        // Trailing CR resolves to a newline at flush.
        assert_eq!(strip(&[b"tail\r"]), b"tail\n");
    }

    #[test]
    fn drops_osc_sequences() {
        assert_eq!(strip(&[b"\x1b]0;title\x07hello\n"]), b"hello\n");
    }

    #[test]
    fn cleanup_respects_max_age() {
        let root = tmpdir();
        let session = root.join("cfg-20260101-000000");
        std::fs::create_dir_all(&session).unwrap();
        std::fs::write(session.join("a.log"), "x").unwrap();

        // Fresh dir survives a 1h threshold...
        cleanup_old_sessions(&root, Duration::from_secs(3600));
        assert!(session.exists());
        // ...and is removed when everything counts as old.
        cleanup_old_sessions(&root, Duration::ZERO);
        assert!(!session.exists());

        // Nonexistent root is a no-op.
        cleanup_old_sessions(&root.join("missing"), Duration::ZERO);
        std::fs::remove_dir_all(&root).ok();
    }
}
