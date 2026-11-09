//! File-backed persistence for the composer input history (#7).
//!
//! One JSON object per line — `{"text": "...", "cwd": "..."}` — in a single
//! global file. Recall is filtered by `cwd` (the workspace root): what you
//! reach for with ↑ is project-local, while the store stays one flat file
//! that survives sessions and restarts. The file is trimmed to the newest
//! [`HISTORY_CAP`] lines on load — the cap is global, not per project.

use std::io::Write as _;
use std::path::PathBuf;

use phi_tui::history::HISTORY_CAP;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
struct Record {
    text: String,
    cwd: String,
}

/// The product's history file behind [`crate::app::App`]'s recall ring.
pub(crate) struct InputHistoryStore {
    path: PathBuf,
    /// The workspace root as a plain string — the record key and the filter.
    cwd: String,
}

impl InputHistoryStore {
    pub(crate) fn new(path: PathBuf, cwd: String) -> Self {
        Self { path, cwd }
    }

    /// This workspace's entries, oldest first. A file over the cap is
    /// rewritten to its newest [`HISTORY_CAP`] lines as a side effect;
    /// unreadable or malformed lines are skipped (history is a convenience —
    /// a corrupt entry never blocks a start).
    pub(crate) fn load(&self) -> Vec<String> {
        let raw = std::fs::read_to_string(&self.path).unwrap_or_default();
        let mut recs: Vec<Record> = raw
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect();
        if recs.len() > HISTORY_CAP {
            recs.drain(..recs.len() - HISTORY_CAP);
            self.rewrite(&recs);
        }
        recs.into_iter()
            .filter(|r| r.cwd == self.cwd)
            .map(|r| r.text)
            .collect()
    }

    /// Append one entry. I/O errors are traced, never fatal: losing one
    /// history line must not disturb a turn.
    pub(crate) fn append(&self, text: &str) {
        let rec = Record {
            text: text.to_string(),
            cwd: self.cwd.clone(),
        };
        let line = match serde_json::to_string(&rec) {
            Ok(l) => l,
            Err(e) => {
                tracing::warn!(error = %e, "input-history: record not serializable");
                return;
            }
        };
        self.write_lines(std::iter::once(line), true);
    }

    fn rewrite(&self, recs: &[Record]) {
        let lines: Vec<String> = recs
            .iter()
            .filter_map(|r| serde_json::to_string(r).ok())
            .collect();
        self.write_lines(lines, false);
    }

    /// Append (`append = true`) or atomically-ish replace (`false`) the file
    /// with `lines` (one JSON record each).
    fn write_lines(&self, lines: impl IntoIterator<Item = String>, append: bool) {
        if let Some(parent) = self.path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let opened = if append {
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.path)
        } else {
            std::fs::File::create(&self.path)
        };
        match opened {
            Ok(mut f) => {
                for l in lines {
                    if writeln!(f, "{l}").is_err() {
                        tracing::warn!("input-history: short write");
                        return;
                    }
                }
            }
            Err(e) => tracing::warn!(error = %e, "input-history: file not writable"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn store(dir: &Path, cwd: &str) -> InputHistoryStore {
        InputHistoryStore::new(dir.join("input-history.jsonl"), cwd.to_string())
    }

    #[test]
    fn roundtrip_filters_by_cwd() {
        let dir = tempfile::tempdir().unwrap();
        let a = store(dir.path(), "/proj/a");
        a.append("one");
        a.append("two");
        let b = store(dir.path(), "/proj/b");
        b.append("other");
        assert_eq!(a.load(), vec!["one".to_string(), "two".to_string()]);
        assert_eq!(b.load(), vec!["other".to_string()]);
    }

    #[test]
    fn append_is_incremental_across_stores() {
        let dir = tempfile::tempdir().unwrap();
        store(dir.path(), "/p").append("first");
        store(dir.path(), "/p").append("second");
        assert_eq!(
            store(dir.path(), "/p").load(),
            vec!["first".to_string(), "second".to_string()]
        );
    }

    #[test]
    fn load_trims_the_file_to_the_cap() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path(), "/p");
        for i in 0..HISTORY_CAP + 20 {
            s.append(&format!("e{i}"));
        }
        let loaded = s.load();
        assert_eq!(loaded.len(), HISTORY_CAP);
        assert_eq!(loaded[0], format!("e{}", 20));
        // The rewrite persisted: a second load sees the trimmed file.
        let reloaded = store(dir.path(), "/p").load();
        assert_eq!(reloaded.len(), HISTORY_CAP);
    }

    #[test]
    fn malformed_lines_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("input-history.jsonl");
        std::fs::write(
            &path,
            "not json\n{\"text\":\"good\",\"cwd\":\"/p\"}\n{\"text\":\"other\",\"cwd\":\"/q\"}\n",
        )
        .unwrap();
        assert_eq!(store(dir.path(), "/p").load(), vec!["good".to_string()]);
    }
}
