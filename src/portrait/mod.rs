//! Full-body portraits (`Avatar_Whole_*` sprites): index, cache, decoding,
//! thumbnails and PNG export.

pub mod index;

use crate::{
    characters::{CharacterOption, load_character_options},
    story::{CachePolicy, fingerprint, resolve_data_root, resource_version},
    unity::{sprite::crop_flip, texture},
};
use anyhow::{Context, Result, bail, ensure};
use image::{ImageEncoder, RgbaImage, codecs::png::PngEncoder};
use index::{NAME_PREFIX, Portrait, ScanStats};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashMap},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    time::Instant,
};

pub const CACHE_VERSION: u32 = 3;
pub const THUMBNAIL_EDGE: u32 = 384;
pub const OUTPUT_FOLDER: &str = "Portraits";

pub fn cache_path() -> Option<PathBuf> {
    crate::paths::data_dir().map(|dir| dir.join("portrait-index.json"))
}

pub fn thumbnail_dir() -> Option<PathBuf> {
    crate::paths::data_dir().map(|dir| dir.join("thumbs"))
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
    pub portraits: Vec<Portrait>,
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

pub(crate) fn write_atomically(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut temporary = path.as_os_str().to_owned();
    temporary.push(".partial");
    let temporary = PathBuf::from(temporary);
    std::fs::write(&temporary, bytes)?;
    std::fs::rename(&temporary, path)?;
    Ok(())
}

/// Character resource name derived from a sprite name:
/// `Avatar_Whole_TololoSSR01` → `Tololo`.
pub fn resource_name(sprite: &str) -> String {
    let name = sprite.strip_prefix(NAME_PREFIX).unwrap_or(sprite);
    // Cut at the last rarity marker that ends the name or is followed by a
    // variant number / `_suffix` (`SextansSSR_Blur`, `CharolicSR03`).
    let cut = ["SSR", "SR"].iter().find_map(|marker| {
        name.rmatch_indices(*marker).find_map(|(at, _)| {
            let before = &name[..at];
            let rest = &name[at + marker.len()..];
            let part_of_ssr = *marker == "SR" && before.ends_with('S');
            (!before.is_empty()
                && !part_of_ssr
                && rest
                    .chars()
                    .next()
                    .is_none_or(|c| c.is_ascii_digit() || c == '_'))
            .then_some(at)
        })
    });
    let name = match cut {
        Some(at) => &name[..at],
        None => name.trim_end_matches(|c: char| c.is_ascii_digit()),
    };
    name.trim_end_matches(['_', '-']).to_owned()
}

/// Maps derived resource names to the built-in character catalogue.
pub struct CharacterLookup {
    by_name: HashMap<String, CharacterOption>,
}

impl CharacterLookup {
    pub fn load() -> Self {
        let mut by_name = HashMap::new();
        for option in load_character_options().unwrap_or_default() {
            for name in std::iter::once(&option.resource_name).chain(&option.aliases) {
                by_name.insert(name.to_ascii_lowercase(), option.clone());
            }
        }
        Self { by_name }
    }

    pub fn contains(&self, resource: &str) -> bool {
        self.by_name.contains_key(&resource.to_ascii_lowercase())
    }

    /// (canonical resource name, display name) for an exact resource name
    /// or alias.
    pub fn by_resource(&self, resource: &str) -> Option<(String, String)> {
        self.by_name
            .get(&resource.to_ascii_lowercase())
            .map(|option| (option.resource_name.clone(), option.official_name.clone()))
    }

    /// (canonical resource name, display name)
    pub fn character(&self, sprite: &str) -> (String, String) {
        let resource = resource_name(sprite);
        match self.by_name.get(&resource.to_ascii_lowercase()) {
            Some(option) => (option.resource_name.clone(), option.official_name.clone()),
            None => (resource.clone(), resource),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Sample {
    pub name: String,
    pub source_bundle: String,
    pub path_id: i64,
    pub detail: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct IndexSummary {
    pub portraits: usize,
    pub characters: usize,
    pub unmatched_characters: usize,
    pub by_character: BTreeMap<String, usize>,
    pub formats: BTreeMap<String, usize>,
    pub unsupported: usize,
    pub unsupported_samples: Vec<Sample>,
    pub packing: usize,
    pub packing_samples: Vec<Sample>,
    pub elapsed_ms: u128,
    pub scan_elapsed_ms: u128,
    pub from_cache: bool,
    pub scan: ScanStats,
    pub scan_warnings: usize,
    pub cache_path: Option<PathBuf>,
    pub resource_version: String,
}

/// One portrait with its character grouping.
#[derive(Clone, Debug)]
pub struct Entry {
    pub portrait: Portrait,
    pub character_resource: String,
    pub character_name: String,
    /// Export file stem; the sprite name unless another sprite shares it.
    pub file_stem: String,
}

impl Entry {
    pub fn output_path(&self, output: &Path) -> PathBuf {
        output
            .join(OUTPUT_FOLDER)
            .join(&self.character_resource)
            .join(format!("{}.png", self.file_stem))
    }
}

pub struct PortraitIndex {
    pub game_root: PathBuf,
    pub bundle_root: PathBuf,
    pub entries: Vec<Entry>,
    pub scan_warnings: Vec<String>,
    pub summary: IndexSummary,
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
) -> Result<Option<PortraitIndex>> {
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
) -> Result<Option<PortraitIndex>> {
    let started = Instant::now();
    let data_root = resolve_data_root(game)?;
    let bundle_root = data_root.join("AssetBundles_Windows");
    let bundles = crate::story::index::physical_bundles(&bundle_root)?;
    let version = resource_version(game, &data_root);
    progress(IndexProgress::Stage(
        tr!("计算资源指纹", "Computing the resource fingerprint").into(),
    ));
    let fingerprint = fingerprint(&version, &data_root, &bundles)?;
    let cache = cache_path();
    let cached = cache
        .as_deref()
        .filter(|_| policy != CachePolicy::Rebuild)
        .and_then(|path| load_cache(path, policy, &fingerprint, &data_root));
    let from_cache = cached.is_some();
    let mut file = match cached {
        Some(cached) => {
            progress(IndexProgress::Stage(
                tr!("已命中立绘索引缓存", "Using the cached portrait index").into(),
            ));
            cached
        }
        None => {
            progress(IndexProgress::Stage(tr!(
                "扫描 {} 个资源包中的立绘",
                "Scanning {} resource bundles for portraits",
                bundles.len()
            )));
            let scan_started = Instant::now();
            let output = index::scan_bundles(&bundle_root, &bundles, cancel, &|done, total| {
                progress(IndexProgress::Bundles { done, total })
            });
            if output.cancelled || cancel.load(Ordering::Relaxed) {
                return Ok(None);
            }
            let file = CacheFile {
                version: CACHE_VERSION,
                fingerprint,
                resource_version: version,
                data_root: data_root.clone(),
                scan: output.stats,
                scan_warnings: output.warnings.len(),
                scan_warning_text: output.warnings,
                scan_elapsed_ms: scan_started.elapsed().as_millis(),
                portraits: output.portraits,
            };
            if let Some(path) = &cache {
                progress(IndexProgress::Stage(
                    tr!("写入立绘索引缓存", "Writing the portrait index cache").into(),
                ));
                write_atomically(path, &serde_json::to_vec(&file)?).context(tr!(
                    "无法写入立绘索引缓存",
                    "Cannot write the portrait index cache"
                ))?;
            }
            file
        }
    };
    let lookup = CharacterLookup::load();
    let entries = build_entries(std::mem::take(&mut file.portraits), &lookup);
    let summary = summarize(&entries, &lookup, &file, from_cache, started, cache);
    Ok(Some(PortraitIndex {
        game_root: game.to_path_buf(),
        bundle_root,
        entries,
        scan_warnings: file.scan_warning_text,
        summary,
    }))
}

fn build_entries(portraits: Vec<Portrait>, lookup: &CharacterLookup) -> Vec<Entry> {
    let mut seen: HashMap<String, usize> = HashMap::new();
    // Unmatched names that differ only in case (`Player_FeMale`) share a group.
    let mut spellings: HashMap<String, String> = HashMap::new();
    for portrait in &portraits {
        *seen.entry(portrait.name.to_ascii_lowercase()).or_default() += 1;
    }
    portraits
        .into_iter()
        .map(|portrait| {
            let (character_resource, character_name) = lookup.character(&portrait.name);
            let character_resource = spellings
                .entry(character_resource.to_ascii_lowercase())
                .or_insert(character_resource)
                .clone();
            let character_name = if lookup.contains(&character_resource) {
                character_name
            } else {
                character_resource.clone()
            };
            let file_stem = if seen[&portrait.name.to_ascii_lowercase()] > 1 {
                format!("{}_{}", portrait.name, portrait.path_id)
            } else {
                portrait.name.clone()
            };
            Entry {
                portrait,
                character_resource,
                character_name,
                file_stem,
            }
        })
        .collect()
}

const SAMPLE_LIMIT: usize = 10;

fn sample(portrait: &Portrait, detail: &str) -> Sample {
    Sample {
        name: portrait.name.clone(),
        source_bundle: portrait.source.container.bundle.clone(),
        path_id: portrait.path_id,
        detail: detail.to_owned(),
    }
}

fn summarize(
    entries: &[Entry],
    lookup: &CharacterLookup,
    file: &CacheFile,
    from_cache: bool,
    started: Instant,
    cache_path: Option<PathBuf>,
) -> IndexSummary {
    let mut summary = IndexSummary {
        portraits: entries.len(),
        elapsed_ms: started.elapsed().as_millis(),
        scan_elapsed_ms: file.scan_elapsed_ms,
        from_cache,
        scan: file.scan.clone(),
        scan_warnings: file.scan_warnings,
        cache_path,
        resource_version: file.resource_version.clone(),
        ..IndexSummary::default()
    };
    for entry in entries {
        *summary
            .by_character
            .entry(entry.character_resource.clone())
            .or_default() += 1;
        if let Some(texture) = &entry.portrait.texture {
            *summary
                .formats
                .entry(texture.format_name.clone())
                .or_default() += 1;
        }
        if let Some(reason) = &entry.portrait.unsupported {
            summary.unsupported += 1;
            if summary.unsupported_samples.len() < SAMPLE_LIMIT {
                summary
                    .unsupported_samples
                    .push(sample(&entry.portrait, &reason.text()));
            }
        }
        if let Some(packing) = &entry.portrait.packing {
            summary.packing += 1;
            if summary.packing_samples.len() < SAMPLE_LIMIT {
                summary
                    .packing_samples
                    .push(sample(&entry.portrait, packing));
            }
        }
    }
    summary.characters = summary.by_character.len();
    summary.unmatched_characters = summary
        .by_character
        .keys()
        .filter(|name| !lookup.by_name.contains_key(&name.to_ascii_lowercase()))
        .count();
    summary
}

impl PortraitIndex {
    /// Case-insensitive match on the sprite name or the character's
    /// resource / display name.
    pub fn select(&self, keyword: &str) -> Vec<&Entry> {
        let needle = keyword.trim().to_lowercase();
        self.entries
            .iter()
            .filter(|entry| {
                needle.is_empty()
                    || entry.portrait.name.to_lowercase().contains(&needle)
                    || entry.character_resource.to_lowercase() == needle
                    || entry.character_name.to_lowercase() == needle
            })
            .collect()
    }
}

/// Decodes a portrait to a top-down RGBA image at its original size.
pub fn decode(bundle_root: &Path, portrait: &Portrait) -> Result<RgbaImage> {
    if let Some(reason) = &portrait.unsupported {
        bail!(tr!("{}：{}", "{}: {}", portrait.name, reason.text()));
    }
    let texture_info = portrait.texture.as_ref().context(tr!(
        "立绘缺少贴图信息",
        "The portrait has no texture information"
    ))?;
    let data = index::read_texture_data(
        bundle_root,
        portrait.data.as_ref().context(tr!(
            "立绘缺少贴图数据位置",
            "The portrait has no texture data location"
        ))?,
    )?;
    let pixels = texture::decode(
        texture_info.format,
        texture_info.width,
        texture_info.height,
        &data,
    )?;
    let (width, height, cropped) = crop_flip(
        &pixels,
        texture_info.width,
        texture_info.height,
        portrait.texture_rect,
    )?;
    RgbaImage::from_raw(width as u32, height as u32, cropped).context(tr!(
        "图像缓冲区大小不符",
        "The image buffer size does not match"
    ))
}

pub fn encode_png(image: &RgbaImage) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    PngEncoder::new(&mut bytes).write_image(
        image.as_raw(),
        image.width(),
        image.height(),
        image::ExtendedColorType::Rgba8,
    )?;
    Ok(bytes)
}

/// Composites onto white and encodes a JPEG preview.
pub fn encode_jpg_preview(image: &RgbaImage) -> Result<Vec<u8>> {
    let mut rgb = Vec::with_capacity(image.as_raw().len() / 4 * 3);
    for px in image.pixels() {
        let alpha = u32::from(px[3]);
        for channel in &px.0[..3] {
            rgb.push(((u32::from(*channel) * alpha + 255 * (255 - alpha) + 127) / 255) as u8);
        }
    }
    let mut bytes = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, 90).write_image(
        &rgb,
        image.width(),
        image.height(),
        image::ExtendedColorType::Rgb8,
    )?;
    Ok(bytes)
}

/// FNV-1a over the parts, as a stable cache key.
pub(crate) fn stable_hash(parts: &[&[u8]]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for part in parts {
        for byte in part.iter().chain([0xffu8].iter()) {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0100_0000_01b3);
        }
    }
    hash
}

pub fn thumbnail_key(portrait: &Portrait) -> String {
    let rect = serde_json::to_vec(&portrait.texture_rect).unwrap_or_default();
    format!(
        "{:016x}",
        stable_hash(&[
            portrait.source.container.bundle.as_bytes(),
            &portrait.source.container.offset.to_le_bytes(),
            &portrait.path_id.to_le_bytes(),
            portrait.stamp.as_bytes(),
            &rect,
            &THUMBNAIL_EDGE.to_le_bytes(),
        ])
    )
}

/// Returns the cached thumbnail, generating it first when missing.
pub fn thumbnail(bundle_root: &Path, portrait: &Portrait) -> Result<PathBuf> {
    let directory = thumbnail_dir().context(tr!(
        "无法确定程序所在目录",
        "Cannot determine the program's folder"
    ))?;
    let path = directory.join(format!("{}.png", thumbnail_key(portrait)));
    if path.is_file() {
        return Ok(path);
    }
    let image = decode(bundle_root, portrait)?;
    let (width, height) = (image.width(), image.height());
    let scale = f64::from(THUMBNAIL_EDGE) / f64::from(width.max(height));
    let small = if scale < 1.0 {
        image::imageops::resize(
            &image,
            ((f64::from(width) * scale).round() as u32).max(1),
            ((f64::from(height) * scale).round() as u32).max(1),
            image::imageops::FilterType::Triangle,
        )
    } else {
        image
    };
    write_atomically(&path, &encode_png(&small)?)?;
    Ok(path)
}

#[derive(Clone, Debug)]
pub struct ExportOptions {
    pub output: PathBuf,
    pub jpg: bool,
    pub overwrite: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FileOutcome {
    Written,
    /// Same size and content as the existing file.
    Unchanged,
    /// A different file exists and overwriting was not allowed.
    Conflict,
    Failed,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExportedFile {
    pub name: String,
    pub source_bundle: String,
    pub source_path_id: i64,
    pub output: PathBuf,
    pub width: u32,
    pub height: u32,
    pub alpha_min: u8,
    pub alpha_max: u8,
    pub outcome: FileOutcome,
    pub preview: Option<PathBuf>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct ExportSummary {
    pub requested: usize,
    pub written: usize,
    pub unchanged: usize,
    pub conflicts: usize,
    pub failed: usize,
    pub cancelled: bool,
    pub files: Vec<ExportedFile>,
}

/// Writes `bytes` unless an identical file exists. Returns the outcome.
pub(crate) fn write_output(path: &Path, bytes: &[u8], overwrite: bool) -> Result<FileOutcome> {
    if let Ok(metadata) = path.metadata() {
        if metadata.len() == bytes.len() as u64
            && std::fs::read(path).is_ok_and(|existing| existing == bytes)
        {
            return Ok(FileOutcome::Unchanged);
        }
        if !overwrite {
            return Ok(FileOutcome::Conflict);
        }
    }
    write_atomically(path, bytes)
        .with_context(|| tr!("无法写入 {}", "Cannot write {}", path.display()))?;
    Ok(FileOutcome::Written)
}

fn export_one(bundle_root: &Path, entry: &Entry, options: &ExportOptions) -> Result<ExportedFile> {
    let image = decode(bundle_root, &entry.portrait)?;
    let output = entry.output_path(&options.output);
    let (alpha_min, alpha_max) = image
        .pixels()
        .fold((255u8, 0u8), |(lo, hi), px| (lo.min(px[3]), hi.max(px[3])));
    let mut outcome = write_output(&output, &encode_png(&image)?, options.overwrite)?;
    let preview = if options.jpg {
        let path = output.with_extension("preview.jpg");
        let jpg = write_output(&path, &encode_jpg_preview(&image)?, options.overwrite)?;
        if jpg == FileOutcome::Conflict {
            outcome = FileOutcome::Conflict;
        }
        Some(path)
    } else {
        None
    };
    Ok(ExportedFile {
        name: entry.portrait.name.clone(),
        source_bundle: entry.portrait.source.container.bundle.clone(),
        source_path_id: entry.portrait.path_id,
        output,
        width: image.width(),
        height: image.height(),
        alpha_min,
        alpha_max,
        outcome,
        preview,
        error: None,
    })
}

/// Exports the selected portraits as original-size RGBA PNGs.
pub fn export(
    index: &PortraitIndex,
    entries: &[&Entry],
    options: &ExportOptions,
    cancel: &AtomicBool,
    progress: &(dyn Fn(usize, usize, &str) + Sync),
) -> Result<ExportSummary> {
    ensure!(
        !entries.is_empty(),
        tr!("没有选择要导出的立绘", "No portrait selected for export")
    );
    let mut summary = ExportSummary {
        requested: entries.len(),
        ..ExportSummary::default()
    };
    for (position, entry) in entries.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            summary.cancelled = true;
            break;
        }
        progress(position, entries.len(), &entry.portrait.name);
        let file =
            export_one(&index.bundle_root, entry, options).unwrap_or_else(|error| ExportedFile {
                name: entry.portrait.name.clone(),
                source_bundle: entry.portrait.source.container.bundle.clone(),
                source_path_id: entry.portrait.path_id,
                output: entry.output_path(&options.output),
                width: 0,
                height: 0,
                alpha_min: 0,
                alpha_max: 0,
                outcome: FileOutcome::Failed,
                preview: None,
                error: Some(format!("{error:#}")),
            });
        match file.outcome {
            FileOutcome::Written => summary.written += 1,
            FileOutcome::Unchanged => summary.unchanged += 1,
            FileOutcome::Conflict => summary.conflicts += 1,
            FileOutcome::Failed => summary.failed += 1,
        }
        summary.files.push(file);
    }
    progress(summary.files.len(), entries.len(), "");
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derives_character_resource_names() {
        assert_eq!(resource_name("Avatar_Whole_TololoSSR"), "Tololo");
        assert_eq!(resource_name("Avatar_Whole_TololoSSR01"), "Tololo");
        assert_eq!(resource_name("Avatar_Whole_Groza_02"), "Groza");
        assert_eq!(resource_name("Avatar_Whole_SSR"), "SSR");
        assert_eq!(resource_name("Avatar_Whole_SextansSSR_Blur"), "Sextans");
        assert_eq!(resource_name("Avatar_Whole_CharolicSR03"), "Charolic");
        assert_eq!(
            resource_name("Avatar_Whole_Player_Female02"),
            "Player_Female"
        );
        assert_eq!(
            resource_name("Avatar_Whole_NeuralCloud_Lind"),
            "NeuralCloud_Lind"
        );
        assert_eq!(resource_name("Avatar_Whole_Mayling01"), "Mayling");
        let lookup = CharacterLookup::load();
        assert_eq!(
            lookup.character("Avatar_Whole_TololoSSR02"),
            ("Tololo".into(), "托洛洛".into())
        );
        assert_eq!(
            lookup.character("Avatar_Whole_Nobody01"),
            ("Nobody".into(), "Nobody".into())
        );
    }

    #[test]
    fn write_output_skips_identical_and_guards_conflicts() {
        let directory =
            std::env::temp_dir().join(format!("echo-archive-portrait-test-{}", std::process::id()));
        let path = directory.join("a.png");
        assert_eq!(
            write_output(&path, b"one", false).unwrap(),
            FileOutcome::Written
        );
        assert_eq!(
            write_output(&path, b"one", false).unwrap(),
            FileOutcome::Unchanged
        );
        assert_eq!(
            write_output(&path, b"two", false).unwrap(),
            FileOutcome::Conflict
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"one");
        assert_eq!(
            write_output(&path, b"two", true).unwrap(),
            FileOutcome::Written
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"two");
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn jpg_preview_composites_on_white() {
        let image = RgbaImage::from_raw(1, 1, vec![0, 0, 0, 0]).unwrap();
        let jpg = encode_jpg_preview(&image).unwrap();
        let decoded = image::load_from_memory(&jpg).unwrap().to_rgb8();
        assert!(decoded.pixels().all(|px| px.0.iter().all(|c| *c > 250)));
    }

    /// Compares against the UnityPy exports in `.validation/visual-assets`
    /// (local only). Run with `cargo test --release -- --ignored`.
    #[test]
    #[ignore = "needs GF2_GAME_DIR and local reference PNGs"]
    fn matches_reference_portraits() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".validation/visual-assets");
        let Some(game) = crate::paths::test_game_dir().filter(|_| root.is_dir()) else {
            eprintln!("skipped: set GF2_GAME_DIR and provide the reference files");
            return;
        };
        #[derive(Deserialize)]
        struct Reference {
            name: String,
            source_bundle: String,
            source_path_id: i64,
        }
        let references: Vec<Reference> =
            serde_json::from_slice(&std::fs::read(root.join("portrait-exports.json")).unwrap())
                .unwrap();
        let index = load_or_build(&game, true, &AtomicBool::new(false), &|_| {})
            .unwrap()
            .unwrap();
        for reference in references {
            let entry = index
                .entries
                .iter()
                .find(|e| e.portrait.name == reference.name)
                .unwrap();
            assert_eq!(
                entry.portrait.source.container.bundle,
                reference.source_bundle
            );
            assert_eq!(entry.portrait.path_id, reference.source_path_id);
            let decoded = decode(&index.bundle_root, &entry.portrait).unwrap();
            // Compare what is written to disk, not only the decoded buffer.
            let ours = image::load_from_memory(&encode_png(&decoded).unwrap())
                .unwrap()
                .to_rgba8();
            assert_eq!(ours, decoded);
            let theirs = image::open(
                root.join("portraits")
                    .join(format!("{}.png", reference.name)),
            )
            .unwrap()
            .to_rgba8();
            assert_eq!(ours.dimensions(), theirs.dimensions());
            let alpha = |image: &RgbaImage| {
                image
                    .pixels()
                    .fold((255u8, 0u8), |(lo, hi), px| (lo.min(px[3]), hi.max(px[3])))
            };
            let (mut max_diff, mut differing) = (0u8, 0usize);
            for (a, b) in ours.pixels().zip(theirs.pixels()) {
                let diff = (0..4).map(|i| a[i].abs_diff(b[i])).max().unwrap();
                max_diff = max_diff.max(diff);
                differing += usize::from(diff != 0);
            }
            println!(
                "{}: {}x{} alpha {:?}/{:?} max_diff {max_diff} differing_pixels {differing}",
                reference.name,
                ours.width(),
                ours.height(),
                alpha(&ours),
                alpha(&theirs)
            );
            assert_eq!(alpha(&ours), alpha(&theirs));
            assert_eq!(differing, 0, "{} differs", reference.name);
        }
    }
}
