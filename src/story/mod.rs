//! Story dialogue: full script index, row linking, cache and export.

pub mod export;
pub mod index;
pub mod link;

use crate::tables::{AUDIO_TABLE, CONTENT_TABLE, LANGUAGE_TABLE, StoryTables};
use anyhow::{Context, Result, bail};
use index::{CharacterFile, Grid, ScanStats};
use link::Linked;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
    time::{Duration, Instant, UNIX_EPOCH},
};

pub const CACHE_VERSION: u32 = 3;
pub const SHEET_PREFIX: &str = "Assets/Utage/BuildResources/";

/// How a load may use the cached index files. A cache with another format
/// version or for another data root is never used.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CachePolicy {
    /// Ignore the cache and rebuild.
    Rebuild,
    /// Use the cache when its fingerprint matches the game files.
    Current,
    /// Use the cache even when the game files have changed since.
    AcceptStale,
}

impl CachePolicy {
    /// `use_cache` of the plain `load_or_build` entry points.
    pub fn from_use_cache(use_cache: bool) -> Self {
        if use_cache {
            Self::Current
        } else {
            Self::Rebuild
        }
    }

    pub fn accepts(self, cached: &str, current: &str) -> bool {
        match self {
            Self::Rebuild => false,
            Self::Current => cached == current,
            Self::AcceptStale => true,
        }
    }
}

/// Finds `LocalCache/Data` under the chosen game directory.
pub fn resolve_data_root(game: &Path) -> Result<PathBuf> {
    let candidates = [
        game.to_path_buf(),
        game.join("LocalCache/Data"),
        game.join("GF2_Exilium_Data/LocalCache/Data"),
        game.join("GF2 Game/GF2_Exilium_Data/LocalCache/Data"),
    ];
    for path in candidates {
        if path.join("Table").join(CONTENT_TABLE).is_file() {
            return std::path::absolute(&path)
                .context(tr!("无法解析数据目录", "Cannot resolve the data directory"));
        }
    }
    bail!(tr!(
        "在所选游戏目录下找不到 LocalCache/Data/Table",
        "Cannot find LocalCache/Data/Table under the selected game directory"
    ))
}

/// FNV-1a; stable across builds, unlike `DefaultHasher`.
struct Fingerprint(u64);
impl Fingerprint {
    fn new() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }
    fn write(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.0 ^= u64::from(*byte);
            self.0 = self.0.wrapping_mul(0x0100_0000_01b3);
        }
        self.0 ^= 0xff;
        self.0 = self.0.wrapping_mul(0x0100_0000_01b3);
    }
    fn file(&mut self, path: &Path) -> Result<()> {
        self.stat(path, &FileStat::read(path)?);
        Ok(())
    }
    fn stat(&mut self, path: &Path, stat: &FileStat) {
        self.write(
            path.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .as_bytes(),
        );
        self.write(&stat.length.to_le_bytes());
        self.write(&stat.modified.to_le_bytes());
    }
}

/// The parts of a file's metadata that go into the fingerprint.
struct FileStat {
    length: u64,
    modified: u128,
}

impl FileStat {
    fn read(path: &Path) -> Result<Self> {
        let metadata = path
            .metadata()
            .with_context(|| tr!("无法读取 {}", "Cannot read {}", path.display()))?;
        let modified = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |time| time.as_nanos());
        Ok(Self {
            length: metadata.len(),
            modified,
        })
    }
}

/// Launcher resource version lines from `config.ini` near the game files.
pub fn resource_version(game: &Path, data_root: &Path) -> String {
    let mut candidates: Vec<PathBuf> = game
        .ancestors()
        .take(2)
        .map(|p| p.join("config.ini"))
        .collect();
    candidates.extend(data_root.ancestors().map(|p| p.join("config.ini")));
    for path in candidates {
        if let Ok(text) = std::fs::read_to_string(&path) {
            let versions: Vec<&str> = text
                .lines()
                .map(str::trim)
                .filter(|line| {
                    ["download_ab_version", "f_r_ab_v"]
                        .iter()
                        .any(|key| line.split('=').next().map(str::trim) == Some(key))
                })
                .collect();
            if !versions.is_empty() {
                return versions.join(";");
            }
        }
    }
    String::new()
}

pub fn fingerprint(version: &str, data_root: &Path, bundles: &[PathBuf]) -> Result<String> {
    let mut hash = Fingerprint::new();
    hash.write(version.as_bytes());
    for name in [LANGUAGE_TABLE, CONTENT_TABLE, AUDIO_TABLE] {
        hash.file(&data_root.join("Table").join(name))?;
    }
    hash.write(&(bundles.len() as u64).to_le_bytes());
    let stats = bundles
        .par_iter()
        .map(|bundle| FileStat::read(bundle))
        .collect::<Result<Vec<_>>>()?;
    for (bundle, stat) in bundles.iter().zip(&stats) {
        hash.stat(bundle, stat);
    }
    Ok(format!("{:016x}", hash.0))
}

#[derive(Serialize, Deserialize)]
pub struct CacheFile {
    pub version: u32,
    pub fingerprint: String,
    pub resource_version: String,
    pub data_root: PathBuf,
    pub scan: ScanStats,
    /// How many warnings the scan had. Their text depends on the language,
    /// so it stays out of the file and only a fresh scan has it.
    pub scan_warnings: usize,
    #[serde(skip)]
    pub scan_warning_text: Vec<String>,
    pub scan_elapsed_ms: u128,
    pub grids: Vec<Grid>,
    pub character_files: Vec<CharacterFile>,
}

pub fn cache_path() -> Option<PathBuf> {
    crate::paths::data_dir().map(|dir| dir.join("story-index.json"))
}

fn load_cache(
    path: &Path,
    policy: CachePolicy,
    fingerprint: &str,
    data_root: &Path,
) -> Option<CacheFile> {
    let cache: CacheFile = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    (cache.version == CACHE_VERSION
        && policy.accepts(&cache.fingerprint, fingerprint)
        && cache.data_root == data_root)
        .then_some(cache)
}

fn save_cache(path: &Path, cache: &CacheFile) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension("json.partial");
    std::fs::write(&temporary, serde_json::to_vec(cache)?)?;
    std::fs::rename(&temporary, path)?;
    Ok(())
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct IndexSummary {
    pub scripts: usize,
    pub scripts_with_text: usize,
    pub grids: usize,
    pub text_lines: usize,
    pub voiced_lines: usize,
    pub control_rows: usize,
    pub unresolved: usize,
    pub elapsed_ms: u128,
    pub from_cache: bool,
    pub scan: ScanStats,
    pub scan_warnings: usize,
    pub cache_path: Option<PathBuf>,
    pub resource_version: String,
}

/// The loaded index: raw grids plus the tables needed to link them.
pub struct StoryIndex {
    /// Fingerprint of the game files this index was built from.
    pub fingerprint: String,
    pub game_root: PathBuf,
    pub data_root: PathBuf,
    pub grids: Vec<Grid>,
    /// Character setting table (`Arg2` keys and their prefab files).
    pub character_files: Vec<CharacterFile>,
    pub tables: StoryTables,
    pub scan: ScanStats,
    pub scan_warnings: Vec<String>,
    pub summary: IndexSummary,
    /// Every grid, linked once at load; whoever needs all lines takes them
    /// from here instead of linking again.
    pub linked: Linked,
}

pub enum IndexProgress {
    Stage(String),
    Bundles { done: usize, total: usize },
}

/// Loads the index from cache (unless `use_cache` is false) or rebuilds it.
/// Returns `Ok(None)` when cancelled.
pub fn load_or_build(
    game: &Path,
    use_cache: bool,
    cancel: &AtomicBool,
    progress: &(dyn Fn(IndexProgress) + Sync),
) -> Result<Option<StoryIndex>> {
    load_with_policy(
        game,
        CachePolicy::from_use_cache(use_cache),
        cancel,
        progress,
    )
}

/// Loads the index as `policy` allows, rebuilding it otherwise.
/// Returns `Ok(None)` when cancelled.
pub fn load_with_policy(
    game: &Path,
    policy: CachePolicy,
    cancel: &AtomicBool,
    progress: &(dyn Fn(IndexProgress) + Sync),
) -> Result<Option<StoryIndex>> {
    let started = Instant::now();
    let data_root = resolve_data_root(game)?;
    let bundle_root = data_root.join("AssetBundles_Windows");
    let bundles = index::physical_bundles(&bundle_root)?;
    let version = resource_version(game, &data_root);
    progress(IndexProgress::Stage(
        tr!("计算资源指纹", "Computing the resource fingerprint").into(),
    ));
    let fingerprint = fingerprint(&version, &data_root, &bundles)?;
    // Independent of the index, so it reads while the index is parsed.
    let tables = std::thread::spawn({
        let directory = data_root.join("Table");
        move || StoryTables::load(&directory)
    });
    let cache = cache_path();
    let cached = cache
        .as_deref()
        .filter(|_| policy != CachePolicy::Rebuild)
        .and_then(|path| load_cache(path, policy, &fingerprint, &data_root));
    let from_cache = cached.is_some();
    let cached = match cached {
        Some(cached) => {
            progress(IndexProgress::Stage(
                tr!("已命中索引缓存", "Using the cached index").into(),
            ));
            cached
        }
        None => {
            progress(IndexProgress::Stage(tr!(
                "扫描 {} 个资源包",
                "Scanning {} resource bundles",
                bundles.len()
            )));
            let scan_started = Instant::now();
            let output = index::scan_bundles(&bundles, cancel, &|done, total| {
                progress(IndexProgress::Bundles { done, total })
            });
            if output.cancelled {
                return Ok(None);
            }
            let file = CacheFile {
                version: CACHE_VERSION,
                fingerprint,
                resource_version: version.clone(),
                data_root: data_root.clone(),
                scan: output.stats,
                scan_warnings: output.warnings.len(),
                scan_warning_text: output.warnings,
                scan_elapsed_ms: scan_started.elapsed().as_millis(),
                grids: output.grids,
                character_files: output.character_files,
            };
            if let Some(path) = &cache {
                progress(IndexProgress::Stage(
                    tr!("写入索引缓存", "Writing the index cache").into(),
                ));
                save_cache(path, &file)
                    .context(tr!("无法写入索引缓存", "Cannot write the index cache"))?;
            }
            file
        }
    };
    progress(IndexProgress::Stage(
        tr!(
            "读取语言表、AvgContent 与 AudioData",
            "Reading the language table, AvgContent and AudioData"
        )
        .into(),
    ));
    let tables = tables
        .join()
        .unwrap_or_else(|panic| std::panic::resume_unwind(panic))?;
    let grid_refs: Vec<&Grid> = cached.grids.iter().collect();
    let linked = link::link(&grid_refs, &tables);
    let summary = summarize(
        &cached.grids,
        &linked,
        started.elapsed(),
        from_cache,
        &cached,
        cache,
    );
    Ok(Some(StoryIndex {
        fingerprint: cached.fingerprint,
        game_root: game.to_path_buf(),
        data_root,
        grids: cached.grids,
        character_files: cached.character_files,
        tables,
        scan: cached.scan,
        scan_warnings: cached.scan_warning_text,
        summary,
        linked,
    }))
}

fn summarize(
    grids: &[Grid],
    linked: &Linked,
    elapsed: Duration,
    from_cache: bool,
    cache: &CacheFile,
    cache_path: Option<PathBuf>,
) -> IndexSummary {
    let scripts: BTreeSet<&str> = grids.iter().map(|grid| grid.book.as_str()).collect();
    let with_text: BTreeSet<&str> = linked
        .lines
        .iter()
        .map(|line| line.identity.script.as_str())
        .collect();
    IndexSummary {
        scripts: scripts.len(),
        scripts_with_text: with_text.len(),
        grids: grids.len(),
        text_lines: linked.lines.len(),
        voiced_lines: linked.lines.iter().filter(|l| !l.voices.is_empty()).count(),
        control_rows: linked.control_rows.len(),
        unresolved: linked.unresolved.len(),
        elapsed_ms: elapsed.as_millis(),
        from_cache,
        scan: cache.scan.clone(),
        scan_warnings: cache.scan_warnings,
        cache_path,
        resource_version: cache.resource_version.clone(),
    }
}

/// What to export: one exact script, or every grid whose sheet path
/// contains `<keyword>/` (the reference chapter filter), or the scripts a
/// character speaks in (exported with a line filter, see
/// [`export::export_filtered`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Selection {
    Script(String),
    Chapter(String),
    Character {
        /// Folder and `selection.value` name, e.g. the resource name.
        name: String,
        /// Books (`<script>.book`) to link before filtering.
        books: Vec<String>,
    },
}

impl Selection {
    pub fn label(&self) -> String {
        match self {
            Self::Script(book) => book.strip_suffix(".book").unwrap_or(book).to_owned(),
            Self::Chapter(keyword) => keyword.clone(),
            Self::Character { name, .. } => name.clone(),
        }
    }
}

impl StoryIndex {
    pub fn resolve_selection(&self, selector: &str) -> Result<Selection> {
        let selector = selector.trim();
        let book = link::book_name(selector);
        if self.grids.iter().any(|grid| grid.book == book) {
            return Ok(Selection::Script(book));
        }
        let keyword = selector.trim_end_matches('/');
        if !keyword.is_empty() && !self.select(&Selection::Chapter(keyword.into())).is_empty() {
            return Ok(Selection::Chapter(keyword.into()));
        }
        bail!(tr!(
            "没有名为 {selector} 的剧情脚本，也没有路径包含 {keyword}/ 的脚本",
            "No story script is named {selector}, and no script path contains {keyword}/",
            selector = selector,
            keyword = keyword
        ))
    }

    pub fn select(&self, selection: &Selection) -> Vec<&Grid> {
        match selection {
            Selection::Script(book) => self.grids.iter().filter(|g| &g.book == book).collect(),
            Selection::Chapter(keyword) => {
                let needle = format!("{keyword}/");
                self.grids
                    .iter()
                    .filter(|g| g.sheet.contains(&needle))
                    .collect()
            }
            Selection::Character { books, .. } => self
                .grids
                .iter()
                .filter(|g| books.contains(&g.book))
                .collect(),
        }
    }

    pub fn link(&self, selection: &Selection) -> Linked {
        link::link(&self.select(selection), &self.tables)
    }
}

/// Directory part of a sheet path after the Utage build prefix, e.g.
/// `MainStory/CN_Chapter01`.
pub fn sheet_directory(sheet: &str) -> String {
    let path = sheet.split(':').next().unwrap_or(sheet);
    let path = path.strip_prefix(SHEET_PREFIX).unwrap_or(path);
    path.rsplit_once('/')
        .map(|(dir, _)| dir.to_owned())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sheet_directory_strips_prefix_and_file() {
        assert_eq!(
            sheet_directory(
                "Assets/Utage/BuildResources/MainStory/CN_Chapter01/CN_M01001.xls:CN_M01001"
            ),
            "MainStory/CN_Chapter01"
        );
        assert_eq!(sheet_directory("X.xls:X"), "");
    }

    #[test]
    fn cache_policies_compare_fingerprints() {
        for (policy, same, different) in [
            (CachePolicy::Rebuild, false, false),
            (CachePolicy::Current, true, false),
            (CachePolicy::AcceptStale, true, true),
        ] {
            assert_eq!(policy.accepts("a", "a"), same);
            assert_eq!(policy.accepts("a", "b"), different);
        }
        assert_eq!(CachePolicy::from_use_cache(true), CachePolicy::Current);
        assert_eq!(CachePolicy::from_use_cache(false), CachePolicy::Rebuild);
    }

    #[test]
    fn a_stale_cache_is_only_loaded_when_accepted() {
        let directory =
            std::env::temp_dir().join(format!("echo-archive-story-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("story-index.json");
        let root = Path::new("root");
        let file = |version, data_root: &str| CacheFile {
            version,
            fingerprint: "old".to_owned(),
            resource_version: String::new(),
            data_root: PathBuf::from(data_root),
            scan: ScanStats::default(),
            scan_warnings: 0,
            scan_warning_text: Vec::new(),
            scan_elapsed_ms: 0,
            grids: Vec::new(),
            character_files: Vec::new(),
        };
        save_cache(&path, &file(CACHE_VERSION, "root")).unwrap();
        assert!(load_cache(&path, CachePolicy::AcceptStale, "new", root).is_some());
        assert!(load_cache(&path, CachePolicy::Current, "new", root).is_none());
        assert!(load_cache(&path, CachePolicy::Current, "old", root).is_some());
        // The format version and the data root are checked under every policy.
        save_cache(&path, &file(CACHE_VERSION + 1, "root")).unwrap();
        assert!(load_cache(&path, CachePolicy::AcceptStale, "new", root).is_none());
        save_cache(&path, &file(CACHE_VERSION, "other")).unwrap();
        assert!(load_cache(&path, CachePolicy::AcceptStale, "new", root).is_none());
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn fingerprint_changes_with_version() {
        let mut a = Fingerprint::new();
        a.write(b"4.0");
        let mut b = Fingerprint::new();
        b.write(b"4.1");
        assert_ne!(a.0, b.0);
    }
}
