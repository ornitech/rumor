//! Interactive search over the log view (vt100 scrollback) and the details
//! pane.
//!
//! Log matches are computed against the *displayed* vt100 rows, not the raw
//! ANSI byte stream, so positions map 1:1 to scrollback offsets for jumping
//! and to buffer cells for highlighting (and `\r`-rewritten lines match what
//! the user sees). A full recompute walks ~2000 scrollback rows; it runs only
//! when the cache key (process identity, output generation, query, screen
//! size) changes — never per frame.

use std::ops::Range;
use std::sync::Arc;
use std::time::Instant;

use regex::{Regex, RegexBuilder};
use unicode_width::UnicodeWidthStr;

use crate::process::Process;

/// Minimum time between recomputes triggered purely by new output streaming
/// in while a search is active. Bounds search cost under heavy output.
pub const STREAM_REFRESH: std::time::Duration = std::time::Duration::from_millis(250);

/// One match in the log view. `row` is an absolute display row where 0 is the
/// oldest retained scrollback row; columns are display columns within the row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Match {
    pub row: usize,
    pub col_start: u16,
    pub col_end: u16,
}

/// Why cached log matches no longer reflect the screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Staleness {
    /// Only new output arrived; safe to throttle the rescan.
    GenerationOnly,
    /// Process, query, or screen size changed; rescan immediately.
    Structural,
}

#[derive(PartialEq, Eq)]
struct CacheKey {
    proc_ptr: usize,
    generation: u64,
    query: String,
    rows: u16,
    cols: u16,
}

#[derive(Default)]
pub struct LogSearch {
    /// Input bar open: printable keys edit the query.
    pub editing: bool,
    /// A search is in effect (typing or confirmed): highlights render.
    pub active: bool,
    pub query: String,
    /// Sorted by (row, col_start).
    pub matches: Vec<Match>,
    /// Index into `matches` of the current match.
    pub current: usize,
    /// Total scrollback rows when `matches` was computed.
    pub total_at_compute: usize,
    /// Scrollback position when the search bar opened; restored on Esc.
    pub saved_scrollback: Option<usize>,
    cache_key: Option<CacheKey>,
    last_recompute: Option<Instant>,
}

impl LogSearch {
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    /// How the cached matches relate to this process's current state.
    pub fn stale_kind(&self, proc: &Arc<Process>) -> Option<Staleness> {
        let Some(cached) = self.cache_key.as_ref() else {
            return Some(Staleness::Structural); // never computed
        };
        let (rows, cols) = {
            let parser = proc.parser.lock().unwrap();
            parser.screen().size()
        };
        if cached.proc_ptr != Arc::as_ptr(proc) as usize
            || cached.query != self.query
            || cached.rows != rows
            || cached.cols != cols
        {
            Some(Staleness::Structural)
        } else if cached.generation != proc.generation() {
            Some(Staleness::GenerationOnly)
        } else {
            None
        }
    }

    /// Whether enough time has passed to allow a streaming-driven recompute.
    pub fn stream_refresh_due(&self) -> bool {
        self.last_recompute
            .map(|t| t.elapsed() >= STREAM_REFRESH)
            .unwrap_or(true)
    }

    /// Recompute matches if the cache key changed. Returns true if a
    /// recompute happened. Keeps `current` anchored to the nearest row of the
    /// previous current match so the cursor doesn't teleport on refresh.
    pub fn recompute_if_needed(&mut self, proc: &Arc<Process>) -> bool {
        let prev = self.matches.get(self.current).copied();

        let mut parser = proc.parser.lock().unwrap();
        let (rows, cols) = parser.screen().size();
        let key = CacheKey {
            proc_ptr: Arc::as_ptr(proc) as usize,
            generation: proc.generation(),
            query: self.query.clone(),
            rows,
            cols,
        };
        if self.cache_key.as_ref() == Some(&key) {
            return false;
        }

        if self.query.is_empty() {
            self.matches.clear();
            self.total_at_compute = 0;
        } else {
            let re = build_regex(&self.query);
            let (matches, total) = compute_matches(&mut parser, &re);
            self.matches = matches;
            self.total_at_compute = total;
        }
        drop(parser);

        // Re-anchor: keep the exact same match if it survived, else the one
        // nearest its old row, else default to the newest match.
        self.current = match prev {
            Some(pm) => self
                .matches
                .iter()
                .position(|m| *m == pm)
                .unwrap_or_else(|| nearest_match_idx(&self.matches, pm.row)),
            None => self.matches.len().saturating_sub(1),
        };
        self.cache_key = Some(key);
        self.last_recompute = Some(Instant::now());
        true
    }

    /// Step the current match by `delta` (+1 = newer, -1 = older), wrapping.
    pub fn step(&mut self, delta: i32) {
        let n = self.matches.len();
        if n == 0 {
            return;
        }
        let cur = self.current.min(n - 1) as i32;
        self.current = (cur + delta).rem_euclid(n as i32) as usize;
    }
}

/// Case-insensitive literal matcher for `query`.
pub fn build_regex(query: &str) -> Regex {
    RegexBuilder::new(&regex::escape(query))
        .case_insensitive(true)
        .build()
        .expect("escaped literal is always a valid regex")
}

/// Total scrollback row count, O(1): set_scrollback clamps, so pushing it to
/// MAX and reading back yields the max. Restores the caller's position.
pub fn probe_total(parser: &mut vt100::Parser) -> usize {
    let saved = parser.screen().scrollback();
    parser.screen_mut().set_scrollback(usize::MAX);
    let total = parser.screen().scrollback();
    parser.screen_mut().set_scrollback(saved);
    total
}

/// Scan every display row (full scrollback + visible page) for `re`.
/// Returns matches sorted by (row, col_start) plus the total scrollback row
/// count N at scan time. `Screen::rows` only exposes the visible page, so we
/// page through the history by stepping `set_scrollback`; each step is an
/// O(1) offset change. The caller's scrollback position is restored.
pub fn compute_matches(parser: &mut vt100::Parser, re: &Regex) -> (Vec<Match>, usize) {
    let screen = parser.screen();
    let (rows, cols) = screen.size();
    let saved = screen.scrollback();

    parser.screen_mut().set_scrollback(usize::MAX);
    let total = parser.screen().scrollback();

    let mut matches = Vec::new();
    // Page offsets: total, total - rows, ... always ending exactly at 0 so
    // the visible page is covered. `next_abs` skips rows already scanned when
    // the last step is partial (total not a multiple of the page height).
    let mut next_abs = 0usize;
    let mut offset = total;
    loop {
        parser.screen_mut().set_scrollback(offset);
        let page_top_abs = total - offset;
        for (r, text) in parser.screen().rows(0, cols).enumerate() {
            let abs = page_top_abs + r;
            if abs < next_abs {
                continue;
            }
            next_abs = abs + 1;
            for m in re.find_iter(&text) {
                let col_start = text[..m.start()].width() as u16;
                let col_end = col_start + (m.as_str().width() as u16).max(1);
                matches.push(Match {
                    row: abs,
                    col_start,
                    col_end,
                });
            }
        }
        if offset == 0 {
            break;
        }
        offset = offset.saturating_sub(rows as usize);
    }

    parser.screen_mut().set_scrollback(saved);
    (matches, total)
}

/// Index of the match whose row is closest to `row` (ties prefer the later
/// match). Returns 0 for an empty slice.
pub fn nearest_match_idx(matches: &[Match], row: usize) -> usize {
    matches
        .iter()
        .enumerate()
        .min_by_key(|(_, m)| (m.row.abs_diff(row), std::cmp::Reverse(m.row)))
        .map(|(i, _)| i)
        .unwrap_or(0)
}

/// Scrollback offset that puts absolute row `match_row` in the middle of a
/// `screen_rows`-tall window, given `total` scrollback rows. Derivation: the
/// window at offset s covers absolute rows (total - s)..(total - s +
/// screen_rows); solving match_row - (total - s) = screen_rows / 2 gives
/// s = total + screen_rows / 2 - match_row, clamped to [0, total].
pub fn center_scrollback(total: usize, screen_rows: u16, match_row: usize) -> usize {
    (total + (screen_rows as usize) / 2)
        .saturating_sub(match_row)
        .min(total)
}

/// Search state for the details pane. The pane is tiny (at most a few hundred
/// short lines), so matches are recomputed on demand without caching.
#[derive(Default)]
pub struct DetailsSearch {
    pub editing: bool,
    pub active: bool,
    pub query: String,
    /// (line index, byte range within the line's flattened text).
    pub matches: Vec<(usize, Range<usize>)>,
    pub current: usize,
}

impl DetailsSearch {
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    /// Recompute matches over the pane's flattened lines, keeping `current`
    /// anchored to the nearest line of the previous current match.
    pub fn recompute(&mut self, lines: &[String]) {
        let prev = self.matches.get(self.current).cloned();
        self.matches.clear();
        if !self.query.is_empty() {
            let re = build_regex(&self.query);
            for (i, line) in lines.iter().enumerate() {
                for m in re.find_iter(line) {
                    self.matches.push((i, m.range()));
                }
            }
        }
        // Re-anchor: same match if it survived, else nearest line, else first.
        self.current = match prev {
            Some(pm) => self.matches.iter().position(|m| *m == pm).unwrap_or_else(|| {
                self.matches
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, (line, _))| line.abs_diff(pm.0))
                    .map(|(i, _)| i)
                    .unwrap_or(0)
            }),
            None => 0,
        };
    }

    /// Step the current match by `delta`, wrapping.
    pub fn step(&mut self, delta: i32) {
        let n = self.matches.len();
        if n == 0 {
            return;
        }
        let cur = self.current.min(n - 1) as i32;
        self.current = (cur + delta).rem_euclid(n as i32) as usize;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A parser with a screen small enough that scrollback paging needs
    /// several steps, fed `n` numbered lines.
    fn parser_with_lines(rows: u16, cols: u16, lines: &[&str]) -> vt100::Parser {
        let mut p = vt100::Parser::new(rows, cols, 2000);
        for l in lines {
            p.process(l.as_bytes());
            p.process(b"\r\n");
        }
        p
    }

    #[test]
    fn finds_matches_across_scrollback_pages() {
        // 23 lines on a 5-row screen: 4+ pages, last step partial.
        let lines: Vec<String> = (0..23).map(|i| format!("line {i} alpha")).collect();
        let refs: Vec<&str> = lines.iter().map(|s| s.as_str()).collect();
        let mut p = parser_with_lines(5, 20, &refs);

        let re = build_regex("alpha");
        let (matches, total) = compute_matches(&mut p, &re);
        // 23 lines + trailing newline leaves the cursor on row 23; the rows
        // pushed into scrollback are everything above the visible 5.
        assert_eq!(matches.len(), 23);
        assert_eq!(total, probe_total(&mut p));
        // Rows are consecutive and start at the oldest line.
        let rows: Vec<usize> = matches.iter().map(|m| m.row).collect();
        for w in rows.windows(2) {
            assert_eq!(w[1], w[0] + 1, "no duplicate or skipped rows");
        }
    }

    #[test]
    fn match_columns_account_for_prefix_width() {
        let mut p = parser_with_lines(5, 30, &["xx needle yy"]);
        let re = build_regex("needle");
        let (matches, _) = compute_matches(&mut p, &re);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].col_start, 3);
        assert_eq!(matches[0].col_end, 9);
    }

    #[test]
    fn wide_chars_shift_display_columns() {
        // "你好" occupies 4 display columns.
        let mut p = parser_with_lines(5, 30, &["你好needle"]);
        let re = build_regex("needle");
        let (matches, _) = compute_matches(&mut p, &re);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].col_start, 4);
        assert_eq!(matches[0].col_end, 10);
    }

    #[test]
    fn ansi_styled_text_still_matches() {
        let mut p = vt100::Parser::new(5, 40, 2000);
        p.process(b"\x1b[31mred needle here\x1b[0m\r\n");
        let re = build_regex("needle");
        let (matches, _) = compute_matches(&mut p, &re);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].col_start, 4);
    }

    #[test]
    fn case_insensitive_literal_not_regex() {
        let mut p = parser_with_lines(5, 30, &["found A.B not AxB"]);
        let (matches, _) = compute_matches(&mut p, &build_regex("a.b"));
        assert_eq!(matches.len(), 1, "dot must be literal, case-insensitive");
        assert_eq!(matches[0].col_start, 6);
    }

    #[test]
    fn scrollback_position_is_restored() {
        let lines: Vec<String> = (0..50).map(|i| format!("l{i}")).collect();
        let refs: Vec<&str> = lines.iter().map(|s| s.as_str()).collect();
        let mut p = parser_with_lines(5, 20, &refs);
        p.screen_mut().set_scrollback(7);
        let re = build_regex("l1");
        compute_matches(&mut p, &re);
        assert_eq!(p.screen().scrollback(), 7);
        probe_total(&mut p);
        assert_eq!(p.screen().scrollback(), 7);
    }

    #[test]
    fn center_scrollback_math() {
        // 100 scrollback rows, 10-row screen. Window at offset s covers
        // absolute rows (100 - s)..(110 - s).
        // Newest row (109) -> offset 0 region.
        assert_eq!(center_scrollback(100, 10, 109), 0);
        // Oldest row (0) -> clamped to total.
        assert_eq!(center_scrollback(100, 10, 0), 100);
        // Middle row 50: s = 100 + 5 - 50 = 55; window covers 45..55. 50 is
        // inside, centered.
        let s = center_scrollback(100, 10, 50);
        assert_eq!(s, 55);
        let (lo, hi) = (100 - s, 100 - s + 10);
        assert!((lo..hi).contains(&50));
    }

    #[test]
    fn step_wraps_both_directions() {
        let mut s = LogSearch {
            matches: vec![
                Match { row: 0, col_start: 0, col_end: 1 },
                Match { row: 1, col_start: 0, col_end: 1 },
                Match { row: 2, col_start: 0, col_end: 1 },
            ],
            current: 2,
            ..Default::default()
        };
        s.step(1);
        assert_eq!(s.current, 0, "wraps forward");
        s.step(-1);
        assert_eq!(s.current, 2, "wraps backward");
    }

    #[test]
    fn details_search_finds_and_steps() {
        let lines = vec![
            "PID:    123".to_string(),
            "Status: running".to_string(),
            "PATH=/usr/bin".to_string(),
        ];
        let mut s = DetailsSearch {
            query: "ru".to_string(),
            ..Default::default()
        };
        s.recompute(&lines);
        assert_eq!(s.matches.len(), 1);
        assert_eq!(s.matches[0].0, 1);
        s.query = "i".to_string();
        s.recompute(&lines);
        assert_eq!(s.matches.len(), 3); // PID, runnIng, /usr/bIn
    }

    #[test]
    fn empty_query_yields_no_matches() {
        let lines = vec!["anything".to_string()];
        let mut s = DetailsSearch::default();
        s.recompute(&lines);
        assert!(s.matches.is_empty());
    }

    #[test]
    fn nearest_match_anchoring() {
        let ms = vec![
            Match { row: 5, col_start: 0, col_end: 1 },
            Match { row: 20, col_start: 0, col_end: 1 },
            Match { row: 40, col_start: 0, col_end: 1 },
        ];
        assert_eq!(nearest_match_idx(&ms, 18), 1);
        assert_eq!(nearest_match_idx(&ms, 0), 0);
        assert_eq!(nearest_match_idx(&ms, 100), 2);
        assert_eq!(nearest_match_idx(&[], 3), 0);
    }

    /// Perf tripwire: a full recompute over a maxed-out scrollback at nowrap
    /// width must stay well under one frame budget in release builds.
    #[test]
    #[ignore = "perf tripwire; run with --release --ignored"]
    fn full_scan_under_50ms() {
        let mut p = vt100::Parser::new(50, 1000, 2000);
        for i in 0..2050 {
            let line = format!("{i:06} {}\r\n", "x".repeat(180));
            p.process(line.as_bytes());
        }
        let re = build_regex("needle-that-never-matches");
        let start = std::time::Instant::now();
        let (matches, _) = compute_matches(&mut p, &re);
        let elapsed = start.elapsed();
        assert!(matches.is_empty());
        assert!(elapsed.as_millis() < 50, "scan took {elapsed:?}");
    }
}
