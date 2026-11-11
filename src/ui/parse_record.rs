//! Record of the last finished parse and the startup decision built on it:
//! whether a launch reuses the last results and whether the game has changed
//! since. The check reads this small record and stats the game files; it
//! never opens the large index files.

use crate::{
    live2d, portrait,
    roster::survey,
    story::{self, fingerprint, index::physical_bundles, resolve_data_root, resource_version},
};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParseRecord {
    pub game_root: PathBuf,
    /// Fingerprint of the game files the results were built from.
    pub fingerprint: String,
    pub characters: usize,
    /// Seconds since the Unix epoch.
    pub finished_at: u64,
}

impl ParseRecord {
    pub fn new(game_root: PathBuf, fingerprint: String, characters: usize) -> Self {
        Self {
            game_root,
            fingerprint,
            characters,
            finished_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |time| time.as_secs()),
        }
    }

    fn load(path: &Path) -> Option<Self> {
        serde_json::from_slice(&std::fs::read(path).ok()?).ok()
    }

    fn write(&self, path: &Path) -> Result<()> {
        portrait::write_atomically(path, &serde_json::to_vec_pretty(self)?)
    }

    pub fn save(&self) -> Result<()> {
        let path = record_path().ok_or_else(|| {
            anyhow::anyhow!(tr!(
                "无法确定程序所在目录",
                "Cannot determine the program's folder"
            ))
        })?;
        self.write(&path)
    }
}

fn record_path() -> Option<PathBuf> {
    crate::paths::data_dir().map(|dir| dir.join("parse-state.json"))
}

/// What a launch does with the last parse.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StartupDecision {
    /// Nothing to reuse: wait for the user to parse.
    NotParsed,
    /// Load the last results; the game has not changed.
    Restore,
    /// Load the last results and offer an update; the game has changed.
    RestoreStale,
}

/// `current` is the game's fingerprint now, `None` when it cannot be
/// computed (the directory no longer holds the game's data).
pub fn decide(
    record: Option<&ParseRecord>,
    game_root: &Path,
    indexes_exist: bool,
    current: Option<&str>,
) -> StartupDecision {
    let (Some(record), Some(current)) = (record, current) else {
        return StartupDecision::NotParsed;
    };
    if record.game_root != game_root || !indexes_exist {
        StartupDecision::NotParsed
    } else if record.fingerprint == current {
        StartupDecision::Restore
    } else {
        StartupDecision::RestoreStale
    }
}

fn current_fingerprint(game: &Path) -> Result<String> {
    let data_root = resolve_data_root(game)?;
    let bundles = physical_bundles(&data_root.join("AssetBundles_Windows"))?;
    fingerprint(&resource_version(game, &data_root), &data_root, &bundles)
}

fn indexes_exist() -> bool {
    [
        story::cache_path(),
        portrait::cache_path(),
        live2d::cache_path(),
        survey::cache_path(),
    ]
    .iter()
    .all(|path| path.as_deref().is_some_and(Path::is_file))
}

/// The startup check; blocks on the file system, so it runs in the background.
pub fn check(game: &Path) -> StartupDecision {
    let record = record_path().and_then(|path| ParseRecord::load(&path));
    // Without a record there is nothing to compare, as on a first launch.
    let current = record
        .is_some()
        .then(|| current_fingerprint(game).ok())
        .flatten();
    decide(record.as_ref(), game, indexes_exist(), current.as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record() -> ParseRecord {
        ParseRecord::new(PathBuf::from(r"F:\Game"), "aaaa".to_owned(), 113)
    }

    fn game() -> &'static Path {
        Path::new(r"F:\Game")
    }

    #[test]
    fn no_record_means_not_parsed() {
        assert_eq!(
            decide(None, game(), true, Some("aaaa")),
            StartupDecision::NotParsed
        );
    }

    #[test]
    fn another_game_directory_means_not_parsed() {
        assert_eq!(
            decide(Some(&record()), Path::new(r"D:\Other"), true, Some("aaaa")),
            StartupDecision::NotParsed
        );
    }

    #[test]
    fn a_missing_index_file_means_not_parsed() {
        assert_eq!(
            decide(Some(&record()), game(), false, Some("aaaa")),
            StartupDecision::NotParsed
        );
    }

    #[test]
    fn the_same_fingerprint_restores() {
        assert_eq!(
            decide(Some(&record()), game(), true, Some("aaaa")),
            StartupDecision::Restore
        );
    }

    #[test]
    fn a_different_fingerprint_restores_and_offers_an_update() {
        assert_eq!(
            decide(Some(&record()), game(), true, Some("bbbb")),
            StartupDecision::RestoreStale
        );
    }

    #[test]
    fn an_unreadable_game_directory_means_not_parsed() {
        assert_eq!(
            decide(Some(&record()), game(), true, None),
            StartupDecision::NotParsed
        );
    }

    #[test]
    fn the_record_survives_a_round_trip() {
        let directory =
            std::env::temp_dir().join(format!("echo-archive-record-{}", std::process::id()));
        let path = directory.join("parse-state.json");
        let saved = record();
        saved.write(&path).unwrap();
        assert_eq!(ParseRecord::load(&path), Some(saved));
        // A damaged record counts as none.
        std::fs::write(&path, b"{").unwrap();
        assert_eq!(ParseRecord::load(&path), None);
        std::fs::remove_dir_all(directory).unwrap();
    }
}
