//! Filesystem watcher: surface perf work that happens *outside* the
//! dashboard while it's running.
//!
//! Polling, not FSEvents/inotify — two directories every two seconds is
//! nothing, and polling needs no extra dependency and no platform code.
//! Watched roots:
//!
//! * `tmp/perf-vs/*/cmp.json` — a `perf-vs` run finished in a terminal.
//! * `.aatxe/baselines/*.json` — a local baseline was saved (PR #10's
//!   `aatxe baseline save`); ingested as a plain `RunReport`.
//!
//! Files that already exist when the session starts are seeded as seen
//! without emitting — the dashboard shows what happens from now on, not
//! a replay of last week's tmp pile.

use crate::events::UiEvent;
use crate::state::SharedState;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchKind {
    PerfVsCompare,
    BaselineRun,
}

/// One scan pass: anything new or modified since the last pass, with the
/// seen-map updated in place. Pure-ish (filesystem only) for testability.
pub fn collect_updates(
    repo_root: &Path,
    seen: &mut HashMap<PathBuf, SystemTime>,
) -> Vec<(WatchKind, PathBuf)> {
    let mut out = Vec::new();
    for path in scan_perf_vs(&repo_root.join("tmp/perf-vs")) {
        if is_new(&path, seen) {
            out.push((WatchKind::PerfVsCompare, path));
        }
    }
    for path in scan_json_dir(&repo_root.join(".aatxe/baselines")) {
        if is_new(&path, seen) {
            out.push((WatchKind::BaselineRun, path));
        }
    }
    out
}

/// Seed the seen-map with current state, emitting nothing.
pub fn seed(repo_root: &Path, seen: &mut HashMap<PathBuf, SystemTime>) {
    let _ = collect_updates(repo_root, seen);
}

fn is_new(path: &Path, seen: &mut HashMap<PathBuf, SystemTime>) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
    match seen.insert(path.to_path_buf(), mtime) {
        Some(prev) => prev != mtime,
        None => true,
    }
}

/// `tmp/perf-vs/<slug>/cmp.json`, one level deep.
fn scan_perf_vs(root: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    entries
        .filter_map(|e| e.ok())
        .map(|e| e.path().join("cmp.json"))
        .filter(|p| p.is_file())
        .collect()
}

fn scan_json_dir(root: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().is_some_and(|x| x == "json"))
        .collect()
}

fn rel_display(repo_root: &Path, path: &Path) -> String {
    path.strip_prefix(repo_root)
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned()
}

pub async fn watch_loop(state: SharedState) {
    let mut seen: HashMap<PathBuf, SystemTime> = HashMap::new();
    seed(&state.cfg.repo_root, &mut seen);
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        for (kind, path) in collect_updates(&state.cfg.repo_root, &mut seen) {
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let source = rel_display(&state.cfg.repo_root, &path);
            match kind {
                WatchKind::PerfVsCompare => {
                    if let Ok(report) = serde_json::from_str(&text) {
                        state.bus.emit(UiEvent::ExternalCompare {
                            source,
                            report: Box::new(report),
                        });
                    }
                }
                WatchKind::BaselineRun => {
                    if let Ok(report) = serde_json::from_str(&text) {
                        state.bus.emit(UiEvent::RunIngested {
                            source,
                            report: Box::new(report),
                        });
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_scan_reports_then_dedups_until_mtime_changes() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        let pv = repo.join("tmp/perf-vs/abc-council");
        std::fs::create_dir_all(&pv).unwrap();
        std::fs::write(pv.join("cmp.json"), "{}").unwrap();

        let mut seen = HashMap::new();
        let first = collect_updates(repo, &mut seen);
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].0, WatchKind::PerfVsCompare);

        // Unchanged file → silent.
        assert!(collect_updates(repo, &mut seen).is_empty());

        // Touch with a future mtime → reported again.
        let later = SystemTime::now() + std::time::Duration::from_secs(5);
        let f = std::fs::File::open(pv.join("cmp.json")).unwrap();
        f.set_modified(later).unwrap();
        assert_eq!(collect_updates(repo, &mut seen).len(), 1);
    }

    #[test]
    fn seed_swallows_preexisting_files() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        let bl = repo.join(".aatxe/baselines");
        std::fs::create_dir_all(&bl).unwrap();
        std::fs::write(bl.join("master.json"), "{}").unwrap();

        let mut seen = HashMap::new();
        seed(repo, &mut seen);
        assert!(collect_updates(repo, &mut seen).is_empty());

        std::fs::write(bl.join("fresh.json"), "{}").unwrap();
        let updates = collect_updates(repo, &mut seen);
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].0, WatchKind::BaselineRun);
    }

    #[test]
    fn non_json_and_missing_dirs_are_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        let bl = repo.join(".aatxe/baselines");
        std::fs::create_dir_all(&bl).unwrap();
        std::fs::write(bl.join("README.md"), "x").unwrap();
        let mut seen = HashMap::new();
        assert!(collect_updates(repo, &mut seen).is_empty());
    }
}
