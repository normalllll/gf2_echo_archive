//! Live2D runtime models: index (model3 manifests and the files they
//! reference), cache, texture previews and whole-model export.

pub mod core;
pub mod manifest;
pub mod scan;

use crate::{
    portrait::{
        self, CharacterLookup, FileOutcome, encode_png,
        index::{DataRef, NodeRef, Portrait, TextureInfo, locate_texture, node_key},
        write_atomically, write_output,
    },
    story::{CachePolicy, fingerprint, resolve_data_root, resource_version},
    unity::{
        sprite::{Rect, crop_flip},
        texture,
    },
};
use anyhow::{Context, Result, bail, ensure};
use image::RgbaImage;
use manifest::{Reference, Role, references, safe_join, text_key};
use rayon::prelude::*;
use scan::{Located, NodeReader, ObjectRef, Prefab, ScanOutput, ScanStats, is_manifest_name};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    time::Instant,
};

pub const CACHE_VERSION: u32 = 2;
pub const OUTPUT_FOLDER: &str = "Live2D";
pub const REPORT_FILE: &str = "export-report.json";
pub const MOC_MAGIC: &[u8; 4] = b"MOC3";

pub fn cache_path() -> Option<PathBuf> {
    crate::paths::data_dir().map(|dir| dir.join("live2d-index.json"))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileStatus {
    Ok,
    /// No source object has this name.
    Missing,
    /// Several sources with different content; none is chosen.
    Ambiguous,
    /// The prefab does not bind a texture for this entry.
    TextureNotFound,
    /// Found, but not valid JSON, not a moc3, or an unsupported texture.
    Invalid,
}

impl FileStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Ok => tr!("正常", "OK"),
            Self::Missing => tr!("缺失", "Missing"),
            Self::Ambiguous => tr!("歧义", "Ambiguous"),
            Self::TextureNotFound => tr!("找不到贴图", "Texture not found"),
            Self::Invalid => tr!("无效", "Invalid"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TextureSource {
    pub info: TextureInfo,
    pub data: DataRef,
    /// Source bundle stamp, part of the preview cache key.
    pub stamp: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModelFile {
    pub role: Role,
    /// Relative path inside the model directory, as in the manifest.
    pub path: String,
    pub status: FileStatus,
    pub source: Option<ObjectRef>,
    /// Payload size (texture: decoded RGBA size is not known; `None`).
    pub size: Option<u64>,
    pub texture: Option<TextureSource>,
    /// Number of distinct sources found (more than one: ambiguous).
    pub candidates: usize,
    pub detail: Option<FileDetail>,
}

/// Why a file has its status. Stored in the index cache, so it holds no
/// display text.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileDetail {
    Unreadable { sources: Vec<String> },
    InvalidJson,
    InvalidMoc,
    DifferentCopies { copies: Vec<(String, usize)> },
    UnnamedMoc,
    ChosenByScope,
    NoPrefab,
    PrefabsDiffer { roots: Vec<String> },
    TextureFileMissing { cab: String },
    TextureUnreadable { cab: String, path_id: i64 },
    UnsupportedFormat { format: String },
    NoTextureNamed { bound: usize, stem: String },
    SeveralTexturesNamed { count: usize, stem: String },
}

impl FileDetail {
    pub fn text(&self) -> String {
        match self {
            Self::Unreadable { sources } => tr!(
                "无法读取：{}",
                "Could not be read: {}",
                sources.join("; ")
            ),
            Self::InvalidJson => tr!("JSON 无法解析", "The JSON cannot be parsed").into(),
            Self::InvalidMoc => tr!("moc3 签名无效", "Invalid moc3 signature").into(),
            Self::DifferentCopies { copies } => {
                let list = copies
                    .iter()
                    .map(|(source, bytes)| {
                        tr!("{} ({} 字节)", "{} ({} bytes)", source, bytes)
                    })
                    .collect::<Vec<_>>()
                    .join(tr!("、", ", "));
                tr!(
                    "{} 份内容不同的同名资源：{}",
                    "{} same-name resources with different content: {}",
                    copies.len(),
                    list
                )
            }
            Self::UnnamedMoc => tr!(
                "CubismMoc 没有名称，取模型清单所在文件中的 moc",
                "The CubismMoc has no name; using the moc in the manifest's file"
            )
            .into(),
            Self::ChosenByScope => tr!(
                "同名资源有多份，按模型清单所在文件及其依赖选择",
                "Several resources share this name; chosen by the manifest's file and its dependencies"
            )
            .into(),
            Self::NoPrefab => tr!(
                "找不到与模型同名、带渲染组件的根 GameObject",
                "No root GameObject with a renderer has the model's name"
            )
            .into(),
            Self::PrefabsDiffer { roots } => tr!(
                "{} 个同名模型预制体绑定的贴图不同：{}",
                "{} same-name model prefabs bind different textures: {}",
                roots.len(),
                roots.join(tr!("、", ", "))
            ),
            Self::TextureFileMissing { cab } => tr!(
                "找不到贴图所在文件 {}",
                "Cannot find the file holding the texture: {}",
                cab
            ),
            Self::TextureUnreadable { cab, path_id } => tr!(
                "无法读取贴图 {}#{}",
                "Cannot read texture {}#{}",
                cab,
                path_id
            ),
            Self::UnsupportedFormat { format } => tr!(
                "不支持的贴图格式 {}",
                "Unsupported texture format {}",
                format
            ),
            Self::NoTextureNamed { bound, stem } => tr!(
                "预制体绑定了 {} 张贴图，没有名为 {} 的贴图",
                "The prefab binds {} textures, none named {}",
                bound,
                stem
            ),
            Self::SeveralTexturesNamed { count, stem } => tr!(
                "预制体中有 {} 张名为 {} 的贴图",
                "The prefab has {} textures named {}",
                count,
                stem
            ),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrefabSummary {
    pub root: ObjectRef,
    pub descendants: usize,
    pub renderers: usize,
    pub textures: usize,
    pub unresolved_renderers: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Model {
    pub name: String,
    /// Name of the manifest TextAsset (`cha_lenna_ssr_01.model3`).
    pub manifest_asset: String,
    pub files: Vec<ModelFile>,
    pub prefab: Option<PrefabSummary>,
}

impl Model {
    pub fn is_complete(&self) -> bool {
        self.files.iter().all(|file| file.status == FileStatus::Ok)
    }
    pub fn count(&self, role: Role) -> usize {
        self.files.iter().filter(|file| file.role == role).count()
    }
    /// Problems grouped like the index summary.
    pub fn problems(&self) -> Vec<(Problem, &ModelFile)> {
        self.files
            .iter()
            .filter_map(|file| Problem::of(file.status).map(|problem| (problem, file)))
            .collect()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Problem {
    MissingFile,
    Ambiguous,
    TextureNotFound,
    Invalid,
}

impl Problem {
    fn of(status: FileStatus) -> Option<Self> {
        match status {
            FileStatus::Ok => None,
            FileStatus::Missing => Some(Self::MissingFile),
            FileStatus::Ambiguous => Some(Self::Ambiguous),
            FileStatus::TextureNotFound => Some(Self::TextureNotFound),
            FileStatus::Invalid => Some(Self::Invalid),
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::MissingFile => tr!("缺文件", "Missing files"),
            Self::Ambiguous => tr!("歧义", "Ambiguous"),
            Self::TextureNotFound => tr!("找不到贴图", "Texture not found"),
            Self::Invalid => tr!("文件无效", "Invalid files"),
        }
    }
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
    pub resolve_elapsed_ms: u128,
    pub models: Vec<Model>,
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

// ---------------------------------------------------------------------------
// Resolving manifests against the scan.

fn group<T>(items: &[T], key: impl Fn(&T) -> String) -> HashMap<String, Vec<&T>> {
    let mut map: HashMap<String, Vec<&T>> = HashMap::new();
    for item in items {
        map.entry(key(item)).or_default().push(item);
    }
    map
}

/// Reads objects, keeping one `NodeReader` per node.
struct Reader<'a> {
    bundle_root: &'a Path,
    open: HashMap<NodeRef, NodeReader>,
}

impl<'a> Reader<'a> {
    fn new(bundle_root: &'a Path) -> Self {
        Self {
            bundle_root,
            open: HashMap::new(),
        }
    }
    fn node(&mut self, node: &NodeRef) -> Result<&mut NodeReader> {
        if !self.open.contains_key(node) {
            let reader = NodeReader::open(self.bundle_root, node)?;
            self.open.insert(node.clone(), reader);
        }
        Ok(self.open.get_mut(node).expect("inserted"))
    }
    fn text(&mut self, object: &ObjectRef) -> Result<Vec<u8>> {
        self.node(&object.node)?.text_asset(object.path_id)
    }
    fn moc(&mut self, object: &ObjectRef) -> Result<Vec<u8>> {
        self.node(&object.node)?.moc_bytes(object.path_id)
    }
}

/// Distinct contents among the candidates, in candidate order.
fn distinct(
    candidates: &[&Located],
    read: &mut dyn FnMut(&ObjectRef) -> Result<Vec<u8>>,
) -> (Vec<(ObjectRef, Vec<u8>)>, Vec<String>) {
    let mut result: Vec<(ObjectRef, Vec<u8>)> = Vec::new();
    let mut errors = Vec::new();
    for candidate in candidates {
        match read(&candidate.object) {
            Ok(bytes) => {
                if !result.iter().any(|(_, existing)| *existing == bytes) {
                    result.push((candidate.object.clone(), bytes));
                }
            }
            Err(_) => errors.push(candidate.object.label()),
        }
    }
    (result, errors)
}

fn file_from_candidates(
    reference: &Reference,
    found: Vec<(ObjectRef, Vec<u8>)>,
    errors: Vec<String>,
    validate: impl Fn(&[u8]) -> Result<(), FileDetail>,
) -> ModelFile {
    let mut file = ModelFile {
        role: reference.role,
        path: reference.path.clone(),
        status: FileStatus::Missing,
        source: None,
        size: None,
        texture: None,
        candidates: found.len(),
        detail: (!errors.is_empty()).then_some(FileDetail::Unreadable { sources: errors }),
    };
    match found.len() {
        0 => {}
        1 => {
            let (source, bytes) = found.into_iter().next().expect("one");
            file.size = Some(bytes.len() as u64);
            file.source = Some(source);
            file.status = match validate(&bytes) {
                Ok(()) => FileStatus::Ok,
                Err(detail) => {
                    file.detail = Some(detail);
                    FileStatus::Invalid
                }
            };
        }
        _ => {
            file.status = FileStatus::Ambiguous;
            file.detail = Some(FileDetail::DifferentCopies {
                copies: found
                    .iter()
                    .map(|(source, bytes)| (source.label(), bytes.len()))
                    .collect(),
            });
        }
    }
    file
}

fn validate_json(bytes: &[u8]) -> Result<(), FileDetail> {
    serde_json::from_slice::<serde_json::Value>(bytes)
        .map(|_| ())
        .map_err(|_| FileDetail::InvalidJson)
}

fn validate_moc(bytes: &[u8]) -> Result<(), FileDetail> {
    if bytes.starts_with(MOC_MAGIC) {
        Ok(())
    } else {
        Err(FileDetail::InvalidMoc)
    }
}

/// Model, moc and prefab names are matched case-insensitively and
/// without surrounding spaces (`6P62BOSS_S ` vs `6P62BOSS_S`).
fn name_key(name: &str) -> String {
    name.trim().to_ascii_lowercase()
}

struct Lookups<'a> {
    texts: HashMap<String, Vec<&'a Located>>,
    mocs: HashMap<String, Vec<&'a Located>>,
    /// Unnamed mocs by the node key of their file.
    unnamed_mocs: HashMap<String, Vec<Located>>,
    prefabs: HashMap<String, Vec<&'a Prefab>>,
    scan: &'a ScanOutput,
    bundle_root: &'a Path,
}

/// One manifest (or several identical copies) under a model name.
struct ManifestGroup {
    name: String,
    asset_name: String,
    found: Vec<(ObjectRef, Vec<u8>)>,
    errors: Vec<String>,
}

fn resolve(scan: &ScanOutput, bundle_root: &Path) -> (Vec<Model>, Vec<String>) {
    let lookups = Lookups {
        texts: group(&scan.text_assets, |a| a.name.to_ascii_lowercase()),
        mocs: group(&scan.mocs, |a| name_key(&a.name)),
        unnamed_mocs: group(
            &scan
                .mocs
                .iter()
                .filter(|m| m.name.trim().is_empty())
                .cloned()
                .collect::<Vec<_>>(),
            |a| node_key(&a.object.node.node),
        )
        .into_iter()
        .map(|(key, values)| (key, values.into_iter().cloned().collect()))
        .collect(),
        prefabs: group(&scan.prefabs, |p| name_key(&p.name)),
        scan,
        bundle_root,
    };
    let mut warnings = Vec::new();
    // Read every manifest and group by model name.
    let manifests: Vec<&Located> = scan
        .text_assets
        .iter()
        .filter(|asset| is_manifest_name(&asset.name))
        .collect();
    // (manifest bytes, model name)
    type Read = Result<(Vec<u8>, String)>;
    let read: Vec<(&Located, Read)> = manifests
        .par_iter()
        .map(|asset| {
            let result = (|| {
                let bytes = Reader::new(bundle_root).text(&asset.object)?;
                let json: serde_json::Value = serde_json::from_slice(&bytes)
                    .context(tr!("model3.json 无法解析", "model3.json cannot be parsed"))?;
                let refs = references(&json)?;
                Ok((bytes, manifest::model_name(&refs, &asset.name)))
            })();
            (*asset, result)
        })
        .collect();
    let mut groups: BTreeMap<String, ManifestGroup> = BTreeMap::new();
    for (asset, result) in read {
        match result {
            Ok((bytes, name)) => {
                let entry =
                    groups
                        .entry(name.to_ascii_lowercase())
                        .or_insert_with(|| ManifestGroup {
                            name: name.clone(),
                            asset_name: asset.name.clone(),
                            found: Vec::new(),
                            errors: Vec::new(),
                        });
                if !entry.found.iter().any(|(_, existing)| *existing == bytes) {
                    entry.found.push((asset.object.clone(), bytes));
                }
            }
            Err(error) => warnings.push(format!(
                "{} ({}): {error:#}",
                asset.name,
                asset.object.label()
            )),
        }
    }
    let models: Vec<Model> = groups
        .into_values()
        .collect::<Vec<_>>()
        .into_par_iter()
        .map(|group| resolve_model(group, &lookups))
        .collect();
    (models, warnings)
}

fn resolve_model(group: ManifestGroup, lookups: &Lookups) -> Model {
    let manifest_reference = Reference {
        role: Role::Manifest,
        path: format!("{}.model3.json", group.name),
    };
    let manifest_bytes = group.found.first().map(|(_, bytes)| bytes.clone());
    let manifest_cab = group
        .found
        .first()
        .map(|(source, _)| node_key(&source.node.node));
    let manifest_file = file_from_candidates(
        &manifest_reference,
        group.found,
        group.errors,
        validate_json,
    );
    let mut model = Model {
        name: group.name.clone(),
        manifest_asset: group.asset_name,
        files: vec![manifest_file],
        prefab: None,
    };
    // With differing manifests the first one still lists the files, but the
    // model stays incomplete because its manifest is ambiguous.
    let Some(refs) = manifest_bytes
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .and_then(|json| references(&json).ok())
    else {
        return model;
    };
    let mut reader = Reader::new(lookups.bundle_root);
    let lower_name = name_key(&group.name);
    // The manifest's own file and the files it references.
    let scope: HashSet<String> = manifest_cab
        .iter()
        .flat_map(|cab| {
            std::iter::once(cab.clone()).chain(
                lookups
                    .scan
                    .externals
                    .get(cab)
                    .into_iter()
                    .flatten()
                    .cloned(),
            )
        })
        .collect();
    let mut pick =
        |candidates: Vec<&Located>,
         read: &mut dyn FnMut(&mut Reader, &ObjectRef) -> Result<Vec<u8>>| {
            let inside: Vec<&Located> = candidates
                .iter()
                .copied()
                .filter(|c| scope.contains(&node_key(&c.object.node.node)))
                .collect();
            let narrowed = !inside.is_empty() && inside.len() < candidates.len();
            let chosen = if inside.is_empty() {
                candidates
            } else {
                inside
            };
            let (found, errors) = distinct(&chosen, &mut |o| read(&mut reader, o));
            (found, errors, narrowed)
        };
    let textures: Vec<&Reference> = refs.iter().filter(|r| r.role == Role::Texture).collect();
    let mut texture_files = resolve_textures(
        &lower_name,
        manifest_cab.as_deref(),
        &textures,
        lookups,
        &mut model,
    );
    for reference in &refs {
        let file = match reference.role {
            Role::Texture => texture_files.remove(0),
            Role::Moc => {
                let mut candidates = lookups.mocs.get(&lower_name).cloned().unwrap_or_default();
                let mut unnamed = false;
                if candidates.is_empty()
                    && let Some(local) = manifest_cab
                        .as_ref()
                        .and_then(|cab| lookups.unnamed_mocs.get(cab))
                {
                    candidates = local.iter().collect();
                    unnamed = true;
                }
                let (found, errors, narrowed) = pick(candidates, &mut |r, o| r.moc(o));
                let mut file = note_scope(
                    file_from_candidates(reference, found, errors, validate_moc),
                    narrowed,
                );
                if unnamed && file.status == FileStatus::Ok {
                    file.detail = Some(FileDetail::UnnamedMoc);
                }
                file
            }
            _ => {
                let candidates = lookups
                    .texts
                    .get(&text_key(&reference.path))
                    .cloned()
                    .unwrap_or_default();
                let (found, errors, narrowed) = pick(candidates, &mut |r, o| r.text(o));
                let is_json = reference.path.to_ascii_lowercase().ends_with(".json");
                let file = file_from_candidates(reference, found, errors, |bytes| {
                    if is_json {
                        validate_json(bytes)
                    } else {
                        Ok(())
                    }
                });
                note_scope(file, narrowed)
            }
        };
        model.files.push(file);
    }
    model
}

/// Marks a file chosen among same-name sources by the manifest's
/// dependency scope.
fn note_scope(mut file: ModelFile, narrowed: bool) -> ModelFile {
    if narrowed && file.status == FileStatus::Ok {
        file.detail = Some(FileDetail::ChosenByScope);
    }
    file
}

fn texture_file(
    reference: &Reference,
    status: FileStatus,
    detail: Option<FileDetail>,
) -> ModelFile {
    ModelFile {
        role: Role::Texture,
        path: reference.path.clone(),
        status,
        source: None,
        size: None,
        texture: None,
        candidates: 0,
        detail,
    }
}

/// Binds manifest textures to the textures the model prefab's renderers
/// use. One texture binds directly; several bind by Texture2D name equal
/// to the file stem (`texture_00`). Returns one file per reference.
fn resolve_textures(
    lower_name: &str,
    manifest_cab: Option<&str>,
    references: &[&Reference],
    lookups: &Lookups,
    model: &mut Model,
) -> Vec<ModelFile> {
    let fail = |status, detail: FileDetail| {
        references
            .iter()
            .map(|r| texture_file(r, status, Some(detail.clone())))
            .collect::<Vec<_>>()
    };
    if references.is_empty() {
        return Vec::new();
    }
    let mut prefabs = lookups.prefabs.get(lower_name).cloned().unwrap_or_default();
    // Prefer prefabs whose file references the manifest's file (the model
    // component points at the moc stored next to the manifest).
    if let Some(cab) = manifest_cab {
        let linked: Vec<&Prefab> = prefabs
            .iter()
            .copied()
            .filter(|p| p.files.iter().any(|f| f == cab))
            .collect();
        if !linked.is_empty() {
            prefabs = linked;
        }
    }
    // Identical texture bindings in several copies of the prefab are fine.
    let mut distinct_prefabs: Vec<&Prefab> = Vec::new();
    for prefab in prefabs {
        if !distinct_prefabs
            .iter()
            .any(|p| p.textures == prefab.textures)
        {
            distinct_prefabs.push(prefab);
        }
    }
    let prefab = match distinct_prefabs.as_slice() {
        [] => {
            return fail(FileStatus::TextureNotFound, FileDetail::NoPrefab);
        }
        [one] => *one,
        many => {
            return fail(
                FileStatus::Ambiguous,
                FileDetail::PrefabsDiffer {
                    roots: many.iter().map(|p| p.root.label()).collect(),
                },
            );
        }
    };
    model.prefab = Some(PrefabSummary {
        root: prefab.root.clone(),
        descendants: prefab.descendants,
        renderers: prefab.renderers,
        textures: prefab.textures.len(),
        unresolved_renderers: prefab.unresolved,
    });
    let mut located = Vec::new();
    for target in &prefab.textures {
        let Some(node) = lookups.scan.nodes.get(&target.cab) else {
            located.push(Err(FileDetail::TextureFileMissing {
                cab: target.cab.clone(),
            }));
            continue;
        };
        located.push(
            locate_texture(
                lookups.bundle_root,
                node,
                target.path_id,
                &lookups.scan.nodes,
            )
            .map(|(info, data)| (node.clone(), info, data))
            .map_err(|_| FileDetail::TextureUnreadable {
                cab: target.cab.clone(),
                path_id: target.path_id,
            }),
        );
    }
    let bind = |reference: &Reference, index: usize| -> ModelFile {
        match &located[index] {
            Err(error) => texture_file(reference, FileStatus::TextureNotFound, Some(error.clone())),
            Ok((node, info, data)) => {
                let supported = texture::is_supported(info.format);
                ModelFile {
                    role: Role::Texture,
                    path: reference.path.clone(),
                    status: if supported {
                        FileStatus::Ok
                    } else {
                        FileStatus::Invalid
                    },
                    source: Some(ObjectRef {
                        node: node.clone(),
                        path_id: info.path_id,
                    }),
                    size: None,
                    texture: Some(TextureSource {
                        info: info.clone(),
                        data: data.clone(),
                        stamp: lookups
                            .scan
                            .stamps
                            .get(&node.container.bundle)
                            .cloned()
                            .unwrap_or_default(),
                    }),
                    candidates: 1,
                    detail: (!supported).then(|| FileDetail::UnsupportedFormat {
                        format: info.format_name.clone(),
                    }),
                }
            }
        }
    };
    if references.len() == 1 && located.len() == 1 {
        return vec![bind(references[0], 0)];
    }
    references
        .iter()
        .map(|reference| {
            let stem = reference
                .path
                .rsplit(['/', '\\'])
                .next()
                .unwrap_or(&reference.path)
                .to_ascii_lowercase();
            let stem = stem.rsplit_once('.').map_or(stem.as_str(), |(s, _)| s);
            let matches: Vec<usize> = located
                .iter()
                .enumerate()
                .filter(|(_, l)| {
                    l.as_ref()
                        .is_ok_and(|(_, info, _)| info.name.eq_ignore_ascii_case(stem))
                })
                .map(|(index, _)| index)
                .collect();
            match matches.as_slice() {
                [one] => bind(reference, *one),
                [] => texture_file(
                    reference,
                    FileStatus::TextureNotFound,
                    Some(FileDetail::NoTextureNamed {
                        bound: located.len(),
                        stem: stem.to_owned(),
                    }),
                ),
                many => texture_file(
                    reference,
                    FileStatus::Ambiguous,
                    Some(FileDetail::SeveralTexturesNamed {
                        count: many.len(),
                        stem: stem.to_owned(),
                    }),
                ),
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Index, summary and selection.

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Sample {
    pub model: String,
    pub file: String,
    pub detail: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct IndexSummary {
    pub models: usize,
    pub complete: usize,
    pub incomplete: usize,
    /// Models with at least one file of each problem kind.
    pub incomplete_by_reason: BTreeMap<Problem, usize>,
    pub samples: BTreeMap<Problem, Vec<Sample>>,
    pub characters: usize,
    pub by_character: BTreeMap<String, usize>,
    pub expressions: usize,
    pub motions: usize,
    pub textures: usize,
    pub elapsed_ms: u128,
    pub scan_elapsed_ms: u128,
    pub resolve_elapsed_ms: u128,
    pub from_cache: bool,
    pub scan: ScanStats,
    pub scan_warnings: usize,
    pub scan_warning_samples: Vec<String>,
    pub cache_path: Option<PathBuf>,
    pub resource_version: String,
}

#[derive(Clone, Debug)]
pub struct Entry {
    pub model: Model,
    pub character_resource: String,
    pub character_name: String,
}

impl Entry {
    pub fn output_dir(&self, output: &Path) -> PathBuf {
        output
            .join(OUTPUT_FOLDER)
            .join(directory_name(&self.model.name))
    }
}

pub struct Live2dIndex {
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

/// A Windows-safe directory name for a model (`6P62BOSS_S ` has a
/// trailing space in the game data).
pub fn directory_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    let trimmed = cleaned.trim().trim_end_matches('.');
    if trimmed.is_empty() {
        "_".into()
    } else {
        trimmed.to_owned()
    }
}

/// `CHA_<resource>_...` → `<resource>`; other names keep their second part
/// or, failing that, the whole name.
pub fn resource_name(model: &str) -> String {
    let mut parts = model.split('_');
    let first = parts.next().unwrap_or_default();
    match parts.next() {
        Some(second) if first.eq_ignore_ascii_case("CHA") && !second.is_empty() => {
            second.to_owned()
        }
        _ => model.to_owned(),
    }
}

/// Loads the index from cache (unless `use_cache` is false) or rebuilds it.
/// Returns `Ok(None)` when cancelled.
pub fn load_or_build(
    game: &Path,
    use_cache: bool,
    cancel: &AtomicBool,
    progress: &(dyn Fn(IndexProgress) + Sync),
) -> Result<Option<Live2dIndex>> {
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
) -> Result<Option<Live2dIndex>> {
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
                tr!("已命中 Live2D 索引缓存", "Using the cached Live2D index").into(),
            ));
            cached
        }
        None => {
            progress(IndexProgress::Stage(tr!(
                "扫描 {} 个资源包中的 Live2D 模型",
                "Scanning {} resource bundles for Live2D models",
                bundles.len()
            )));
            let scan_started = Instant::now();
            let output = scan::scan_bundles(&bundles, cancel, &|done, total| {
                progress(IndexProgress::Bundles { done, total })
            });
            if output.cancelled || cancel.load(Ordering::Relaxed) {
                return Ok(None);
            }
            let scan_elapsed_ms = scan_started.elapsed().as_millis();
            progress(IndexProgress::Stage(tr!(
                "解析 {} 个模型清单的引用",
                "Resolving references of {} model manifests",
                output.stats.manifests
            )));
            let resolve_started = Instant::now();
            let (models, resolve_warnings) = resolve(&output, &bundle_root);
            if cancel.load(Ordering::Relaxed) {
                return Ok(None);
            }
            let mut warnings = output.warnings;
            warnings.extend(resolve_warnings);
            let file = CacheFile {
                version: CACHE_VERSION,
                fingerprint,
                resource_version: version,
                data_root: data_root.clone(),
                scan: output.stats,
                scan_warnings: warnings.len(),
                scan_warning_text: warnings,
                scan_elapsed_ms,
                resolve_elapsed_ms: resolve_started.elapsed().as_millis(),
                models,
            };
            if let Some(path) = &cache {
                progress(IndexProgress::Stage(
                    tr!("写入 Live2D 索引缓存", "Writing the Live2D index cache").into(),
                ));
                write_atomically(path, &serde_json::to_vec(&file)?).context(tr!(
                    "无法写入 Live2D 索引缓存",
                    "Cannot write the Live2D index cache"
                ))?;
            }
            file
        }
    };
    let lookup = CharacterLookup::load();
    let entries: Vec<Entry> = std::mem::take(&mut file.models)
        .into_iter()
        .map(|model| {
            let resource = resource_name(&model.name);
            let (character_resource, character_name) = lookup
                .by_resource(&resource)
                .unwrap_or_else(|| (resource.clone(), resource.clone()));
            Entry {
                model,
                character_resource,
                character_name,
            }
        })
        .collect();
    let summary = summarize(&entries, &file, from_cache, started, cache);
    Ok(Some(Live2dIndex {
        game_root: game.to_path_buf(),
        bundle_root,
        entries,
        scan_warnings: file.scan_warning_text,
        summary,
    }))
}

const SAMPLE_LIMIT: usize = 8;

fn summarize(
    entries: &[Entry],
    file: &CacheFile,
    from_cache: bool,
    started: Instant,
    cache_path: Option<PathBuf>,
) -> IndexSummary {
    let mut summary = IndexSummary {
        models: entries.len(),
        elapsed_ms: started.elapsed().as_millis(),
        scan_elapsed_ms: file.scan_elapsed_ms,
        resolve_elapsed_ms: file.resolve_elapsed_ms,
        from_cache,
        scan: file.scan.clone(),
        scan_warnings: file.scan_warnings,
        scan_warning_samples: file
            .scan_warning_text
            .iter()
            .take(SAMPLE_LIMIT)
            .cloned()
            .collect(),
        cache_path,
        resource_version: file.resource_version.clone(),
        ..IndexSummary::default()
    };
    for entry in entries {
        let model = &entry.model;
        *summary
            .by_character
            .entry(entry.character_resource.clone())
            .or_default() += 1;
        summary.expressions += model.count(Role::Expression);
        summary.motions += model.count(Role::Motion);
        summary.textures += model.count(Role::Texture);
        if model.is_complete() {
            summary.complete += 1;
            continue;
        }
        summary.incomplete += 1;
        let mut seen = Vec::new();
        for (problem, file) in model.problems() {
            let samples = summary.samples.entry(problem).or_default();
            if !seen.contains(&problem) {
                seen.push(problem);
                *summary.incomplete_by_reason.entry(problem).or_default() += 1;
                if samples.len() < SAMPLE_LIMIT {
                    samples.push(Sample {
                        model: model.name.clone(),
                        file: file.path.clone(),
                        detail: file
                            .detail
                            .as_ref()
                            .map(FileDetail::text)
                            .unwrap_or_default(),
                    });
                }
            }
        }
    }
    summary.characters = summary.by_character.len();
    summary
}

impl Live2dIndex {
    /// Exact model name first; otherwise case-insensitive substring of the
    /// model name or exact character resource / display name.
    pub fn select(&self, keyword: &str) -> Vec<&Entry> {
        let needle = keyword.trim().to_lowercase();
        if let Some(exact) = self
            .entries
            .iter()
            .find(|entry| entry.model.name.to_lowercase() == needle)
        {
            return vec![exact];
        }
        self.entries
            .iter()
            .filter(|entry| {
                needle.is_empty()
                    || entry.model.name.to_lowercase().contains(&needle)
                    || entry.character_resource.to_lowercase() == needle
                    || entry.character_name.to_lowercase() == needle
            })
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Texture decoding and previews.

/// Decodes a whole texture to a top-down RGBA image.
pub fn decode_texture(bundle_root: &Path, source: &TextureSource) -> Result<RgbaImage> {
    let info = &source.info;
    let data = portrait::index::read_texture_data(bundle_root, &source.data)?;
    let pixels = texture::decode(info.format, info.width, info.height, &data)?;
    let rect = Rect {
        x: 0.0,
        y: 0.0,
        width: info.width as f32,
        height: info.height as f32,
    };
    let (width, height, flipped) = crop_flip(&pixels, info.width, info.height, rect)?;
    RgbaImage::from_raw(width as u32, height as u32, flipped).context(tr!(
        "图像缓冲区大小不符",
        "The image buffer size does not match"
    ))
}

/// The texture as a portrait record, so the portrait thumbnail cache can be
/// reused for previews.
pub fn preview_portrait(model: &Model, source: &TextureSource) -> Portrait {
    let node = match &source.data {
        DataRef::Stream { node, .. } | DataRef::Inline { node, .. } => node.clone(),
    };
    Portrait {
        name: model.name.clone(),
        source: node,
        path_id: source.info.path_id,
        texture_rect: Rect {
            x: 0.0,
            y: 0.0,
            width: source.info.width as f32,
            height: source.info.height as f32,
        },
        texture: Some(source.info.clone()),
        data: Some(source.data.clone()),
        stamp: source.stamp.clone(),
        ..Portrait::default()
    }
}

// ---------------------------------------------------------------------------
// Export.

#[derive(Clone, Debug)]
pub struct ExportOptions {
    pub output: PathBuf,
    pub overwrite: bool,
    /// Run the optional Cubism Core check with the game's DLL.
    pub verify_core: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExportedFile {
    pub role: Role,
    pub path: String,
    pub status: FileStatus,
    pub outcome: Option<FileOutcome>,
    pub size: Option<u64>,
    pub source_bundle: Option<String>,
    pub source_path_id: Option<i64>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub texture_format: Option<String>,
    pub detail: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct JsonCheck {
    pub path: String,
    pub parses: bool,
    pub unmatched_parameter_ids: Vec<String>,
    pub unmatched_part_ids: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Integrity {
    pub references: usize,
    pub missing_on_disk: Vec<String>,
    pub json_files: usize,
    pub json_invalid: Vec<String>,
    pub moc_signature: bool,
    pub complete: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum CoreCheck {
    Skipped {
        reason: String,
    },
    Failed {
        error: String,
    },
    Passed {
        #[serde(flatten)]
        report: Box<core::CoreReport>,
        /// Every expression / motion ID exists in the model.
        ids_match: bool,
        json: Vec<JsonCheck>,
    },
}

/// Written to `<model>/export-report.json`. Deterministic for the same
/// sources and options, so re-exports compare equal.
#[derive(Clone, Debug, Serialize)]
pub struct ModelReport {
    pub model: String,
    pub character: String,
    pub manifest_asset: String,
    pub runtime_only: &'static str,
    pub prefab: Option<PrefabSummary>,
    pub files: Vec<ExportedFile>,
    pub integrity: Integrity,
    pub core: Option<CoreCheck>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ModelExport {
    pub model: String,
    pub directory: PathBuf,
    pub written: usize,
    pub unchanged: usize,
    pub conflicts: usize,
    pub failed: usize,
    pub skipped: usize,
    pub complete: bool,
    pub report: ModelReport,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct ExportSummary {
    pub requested: usize,
    pub models: Vec<ModelExport>,
    pub written: usize,
    pub unchanged: usize,
    pub conflicts: usize,
    pub failed: usize,
    pub incomplete: usize,
    pub core_passed: usize,
    pub core_skipped: usize,
    pub core_failed: usize,
    pub cancelled: bool,
}

fn runtime_note() -> &'static str {
    tr!(
        "导出的是 Cubism 运行时资源（model3.json、moc3、贴图、表情、动作），不是 Cubism Editor 的 .cmo3 工程",
        "These are Cubism runtime resources (model3.json, moc3, textures, expressions, motions), not a Cubism Editor .cmo3 project"
    )
}

/// File bytes and, for textures, the decoded size.
type Payload = (Vec<u8>, Option<(u32, u32)>);

fn source_bytes(bundle_root: &Path, reader: &mut Reader, file: &ModelFile) -> Result<Payload> {
    let source = file.source.as_ref().context(tr!("没有来源", "No source"))?;
    Ok(match file.role {
        Role::Moc => (reader.moc(source)?, None),
        Role::Texture => {
            let texture = file
                .texture
                .as_ref()
                .context(tr!("缺少贴图信息", "Missing texture information"))?;
            let image = decode_texture(bundle_root, texture)?;
            let size = (image.width(), image.height());
            (encode_png(&image)?, Some(size))
        }
        _ => (reader.text(source)?, None),
    })
}

fn export_model(
    index: &Live2dIndex,
    entry: &Entry,
    options: &ExportOptions,
    core_library: Option<&Path>,
) -> Result<ModelExport> {
    let model = &entry.model;
    let directory = entry.output_dir(&options.output);
    let mut reader = Reader::new(&index.bundle_root);
    let mut export = ModelExport {
        model: model.name.clone(),
        directory: directory.clone(),
        written: 0,
        unchanged: 0,
        conflicts: 0,
        failed: 0,
        skipped: 0,
        complete: false,
        report: ModelReport {
            model: model.name.clone(),
            character: entry.character_name.clone(),
            manifest_asset: model.manifest_asset.clone(),
            runtime_only: runtime_note(),
            prefab: model.prefab.clone(),
            files: Vec::new(),
            integrity: Integrity::default(),
            core: None,
        },
    };
    let mut moc_bytes = None;
    for file in &model.files {
        let mut row = ExportedFile {
            role: file.role,
            path: file.path.clone(),
            status: file.status,
            outcome: None,
            size: file.size,
            source_bundle: file
                .source
                .as_ref()
                .map(|s| s.node.container.bundle.clone()),
            source_path_id: file.source.as_ref().map(|s| s.path_id),
            width: file.texture.as_ref().map(|t| t.info.width as u32),
            height: file.texture.as_ref().map(|t| t.info.height as u32),
            texture_format: file.texture.as_ref().map(|t| t.info.format_name.clone()),
            detail: file.detail.as_ref().map(FileDetail::text),
        };
        if file.status != FileStatus::Ok {
            export.skipped += 1;
            export.report.files.push(row);
            continue;
        }
        let result = safe_join(&directory, &file.path).and_then(|path| {
            let (bytes, dimensions) = source_bytes(&index.bundle_root, &mut reader, file)?;
            if file.role == Role::Moc {
                moc_bytes = Some(bytes.clone());
            }
            if let Some((width, height)) = dimensions {
                row.width = Some(width);
                row.height = Some(height);
            }
            row.size = Some(bytes.len() as u64);
            write_output(&path, &bytes, options.overwrite)
        });
        match result {
            Ok(outcome) => {
                match outcome {
                    FileOutcome::Written => export.written += 1,
                    FileOutcome::Unchanged => export.unchanged += 1,
                    FileOutcome::Conflict => export.conflicts += 1,
                    FileOutcome::Failed => export.failed += 1,
                }
                row.outcome = Some(outcome);
            }
            Err(error) => {
                export.failed += 1;
                row.outcome = Some(FileOutcome::Failed);
                row.detail = Some(format!("{error:#}"));
            }
        }
        export.report.files.push(row);
    }
    let integrity = check_integrity(&directory, model);
    export.complete = integrity.complete && model.is_complete() && export.failed == 0;
    export.report.integrity = integrity;
    if options.verify_core {
        export.report.core = Some(match (core_library, &moc_bytes) {
            (None, _) => CoreCheck::Skipped {
                reason: tr!(
                    "游戏目录中找不到 {}",
                    "Cannot find {} in the game directory",
                    core::CORE_RELATIVE
                ),
            },
            (Some(_), None) => CoreCheck::Skipped {
                reason: tr!("没有可用的 moc3", "No usable moc3").into(),
            },
            (Some(library), Some(moc)) => match core::verify(library, moc) {
                Ok(report) => {
                    let json = check_ids(&directory, model, &report);
                    let ids_match = json.iter().all(|check| {
                        check.parses
                            && check.unmatched_parameter_ids.is_empty()
                            && check.unmatched_part_ids.is_empty()
                    });
                    CoreCheck::Passed {
                        report: Box::new(report),
                        ids_match,
                        json,
                    }
                }
                Err(error) => CoreCheck::Failed {
                    error: format!("{error:#}"),
                },
            },
        });
    }
    let report_bytes = serde_json::to_vec_pretty(&export.report)?;
    write_atomically(&directory.join(REPORT_FILE), &report_bytes).context(tr!(
        "无法写入 export-report.json",
        "Cannot write export-report.json"
    ))?;
    Ok(export)
}

/// Checks the files on disk: every manifest reference exists, JSON parses
/// and the moc3 has its signature.
fn check_integrity(directory: &Path, model: &Model) -> Integrity {
    let mut integrity = Integrity::default();
    for file in &model.files {
        if file.role != Role::Manifest {
            integrity.references += 1;
        }
        let Ok(path) = safe_join(directory, &file.path) else {
            integrity.missing_on_disk.push(file.path.clone());
            continue;
        };
        let Ok(bytes) = std::fs::read(&path) else {
            integrity.missing_on_disk.push(file.path.clone());
            continue;
        };
        if file.path.to_ascii_lowercase().ends_with(".json") {
            integrity.json_files += 1;
            if validate_json(&bytes).is_err() {
                integrity.json_invalid.push(file.path.clone());
            }
        }
        if file.role == Role::Moc {
            integrity.moc_signature = bytes.starts_with(MOC_MAGIC);
        }
    }
    integrity.complete = integrity.missing_on_disk.is_empty()
        && integrity.json_invalid.is_empty()
        && integrity.moc_signature;
    integrity
}

fn check_ids(directory: &Path, model: &Model, report: &core::CoreReport) -> Vec<JsonCheck> {
    model
        .files
        .iter()
        .filter(|file| matches!(file.role, Role::Expression | Role::Motion))
        .map(|file| {
            let mut check = JsonCheck {
                path: file.path.clone(),
                ..JsonCheck::default()
            };
            let json = safe_join(directory, &file.path)
                .ok()
                .and_then(|path| std::fs::read(path).ok())
                .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok());
            if let Some(json) = json {
                check.parses = true;
                let used = core::used_ids(&json);
                check.unmatched_parameter_ids = used
                    .parameters
                    .into_iter()
                    .filter(|id| !report.parameter_ids.contains(id))
                    .collect();
                check.unmatched_part_ids = used
                    .parts
                    .into_iter()
                    .filter(|id| !report.part_ids.contains(id))
                    .collect();
            }
            check
        })
        .collect()
}

/// Exports the selected models as runtime packages.
pub fn export(
    index: &Live2dIndex,
    entries: &[&Entry],
    options: &ExportOptions,
    cancel: &AtomicBool,
    progress: &(dyn Fn(usize, usize, &str) + Sync),
) -> Result<ExportSummary> {
    ensure!(
        !entries.is_empty(),
        tr!(
            "没有选择要导出的 Live2D 模型",
            "No Live2D model selected for export"
        )
    );
    let core_library = options
        .verify_core
        .then(|| core::find_core(&index.game_root))
        .flatten();
    let mut summary = ExportSummary {
        requested: entries.len(),
        ..ExportSummary::default()
    };
    for (position, entry) in entries.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            summary.cancelled = true;
            break;
        }
        progress(position, entries.len(), &entry.model.name);
        let export = match export_model(index, entry, options, core_library.as_deref()) {
            Ok(export) => export,
            Err(error) => bail!(tr!(
                "{}：{error:#}",
                "{}: {error:#}",
                entry.model.name,
                error = error
            )),
        };
        summary.written += export.written;
        summary.unchanged += export.unchanged;
        summary.conflicts += export.conflicts;
        summary.failed += export.failed;
        summary.incomplete += usize::from(!export.complete);
        match &export.report.core {
            Some(CoreCheck::Passed { .. }) => summary.core_passed += 1,
            Some(CoreCheck::Skipped { .. }) => summary.core_skipped += 1,
            Some(CoreCheck::Failed { .. }) => summary.core_failed += 1,
            None => {}
        }
        summary.models.push(export);
    }
    progress(summary.models.len(), entries.len(), "");
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directory_names_are_windows_safe() {
        assert_eq!(directory_name("CHA_Lenna_SSR_01"), "CHA_Lenna_SSR_01");
        assert_eq!(directory_name("6P62BOSS_S "), "6P62BOSS_S");
        assert_eq!(directory_name("a:b*c?."), "a_b_c_");
        assert_eq!(directory_name("  "), "_");
    }

    #[test]
    fn character_resource_from_model_name() {
        assert_eq!(resource_name("CHA_Lenna_SSR_01"), "Lenna");
        assert_eq!(resource_name("cha_agent_04"), "agent");
        assert_eq!(resource_name("6P62BOSS_S "), "6P62BOSS_S ");
        assert_eq!(resource_name("CHA_"), "CHA_");
    }

    fn located(name: &str, cab: &str, path_id: i64) -> Located {
        Located {
            name: name.into(),
            object: ObjectRef {
                node: NodeRef {
                    node: cab.into(),
                    ..NodeRef::default()
                },
                path_id,
            },
            size: 0,
        }
    }

    fn reference(path: &str) -> Reference {
        Reference {
            role: Role::Motion,
            path: path.into(),
        }
    }

    #[test]
    fn identical_duplicates_are_fine_and_different_ones_are_ambiguous() {
        let a = located("m.motion3", "CAB-a", 1);
        let b = located("m.motion3", "CAB-b", 2);
        let c = located("m.motion3", "CAB-c", 3);
        let contents: HashMap<i64, Vec<u8>> = [
            (1, b"{}".to_vec()),
            (2, b"{}".to_vec()),
            (3, b"[1]".to_vec()),
        ]
        .into();
        let mut read = |o: &ObjectRef| Ok(contents[&o.path_id].clone());
        let (found, errors) = distinct(&[&a, &b], &mut read);
        assert_eq!((found.len(), errors.len()), (1, 0));
        let file = file_from_candidates(&reference("m.motion3.json"), found, errors, validate_json);
        assert_eq!(file.status, FileStatus::Ok);
        assert_eq!(file.source.unwrap().path_id, 1);

        let (found, errors) = distinct(&[&a, &b, &c], &mut read);
        let file = file_from_candidates(&reference("m.motion3.json"), found, errors, validate_json);
        assert_eq!(file.status, FileStatus::Ambiguous);
        assert_eq!(file.candidates, 2);
        assert!(file.source.is_none());

        let file =
            file_from_candidates(&reference("m.motion3.json"), vec![], vec![], validate_json);
        assert_eq!(file.status, FileStatus::Missing);

        let bad = vec![(a.object.clone(), b"not json".to_vec())];
        let file = file_from_candidates(&reference("m.motion3.json"), bad, vec![], validate_json);
        assert_eq!(file.status, FileStatus::Invalid);
        assert!(validate_moc(b"MOC3\x03").is_ok() && validate_moc(b"MOC").is_err());
    }

    fn texture_target(cab: &str, path_id: i64) -> scan::TextureTarget {
        scan::TextureTarget {
            cab: cab.into(),
            path_id,
        }
    }

    fn prefab(name: &str, cab: &str, files: &[&str], textures: Vec<scan::TextureTarget>) -> Prefab {
        Prefab {
            name: name.into(),
            root: ObjectRef {
                node: NodeRef {
                    node: cab.into(),
                    ..NodeRef::default()
                },
                path_id: 9,
            },
            descendants: 2,
            renderers: 1,
            textures,
            unresolved: 0,
            files: files.iter().map(|f| (*f).to_owned()).collect(),
        }
    }

    fn texture_status(scan: &ScanOutput, cab: Option<&str>) -> (FileStatus, Option<String>) {
        let lookups = Lookups {
            texts: HashMap::new(),
            mocs: HashMap::new(),
            unnamed_mocs: HashMap::new(),
            prefabs: group(&scan.prefabs, |p| name_key(&p.name)),
            scan,
            bundle_root: Path::new("."),
        };
        let reference = Reference {
            role: Role::Texture,
            path: "M.2048/texture_00.png".into(),
        };
        let mut model = Model {
            name: "M".into(),
            manifest_asset: "m.model3".into(),
            files: Vec::new(),
            prefab: None,
        };
        let files = resolve_textures("m", cab, &[&reference], &lookups, &mut model);
        (
            files[0].status,
            files[0].detail.as_ref().map(FileDetail::text),
        )
    }

    #[test]
    fn prefab_texture_binding_rules() {
        // No prefab with the model's name.
        let mut scan = ScanOutput::default();
        assert_eq!(texture_status(&scan, None).0, FileStatus::TextureNotFound);
        // Two prefabs (names compared trimmed, case-insensitive) with
        // different textures: ambiguous ...
        scan.prefabs = vec![
            prefab(
                "M",
                "cab-p1",
                &["cab-p1", "cab-other"],
                vec![texture_target("cab-t", 1)],
            ),
            prefab(
                "m ",
                "cab-p2",
                &["cab-p2", "cab-m"],
                vec![texture_target("cab-t", 2)],
            ),
        ];
        assert_eq!(texture_status(&scan, None).0, FileStatus::Ambiguous);
        // ... unless only one references the manifest's file. Its texture
        // file is not in the (empty) node table, so it is reported as not
        // found rather than guessed.
        let (status, detail) = texture_status(&scan, Some("cab-m"));
        assert_eq!(status, FileStatus::TextureNotFound);
        assert!(detail.unwrap().contains("cab-t"));
        // Identical bindings in two copies are not ambiguous.
        scan.prefabs[1].textures = scan.prefabs[0].textures.clone();
        assert_eq!(texture_status(&scan, None).0, FileStatus::TextureNotFound);
    }

    /// Compares the CHA_Lenna_SSR_01 export with the local reference in
    /// `.validation/visual-assets/live2d` (bytes, pixels, sources and the
    /// Core check). Run with `cargo test --release -- --ignored`.
    #[test]
    #[ignore = "needs GF2_GAME_DIR and local reference files"]
    fn matches_reference_lenna() {
        let root =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".validation/visual-assets/live2d");
        let Some(game) = crate::paths::test_game_dir().filter(|_| root.is_dir()) else {
            eprintln!("skipped: set GF2_GAME_DIR and provide the reference files");
            return;
        };
        let sample = root.join("sample-CHA_Lenna_SSR_01");
        let index = load_or_build(&game, true, &AtomicBool::new(false), &|_| {})
            .unwrap()
            .unwrap();
        let entries = index.select("CHA_Lenna_SSR_01");
        assert_eq!(entries.len(), 1);
        let entry = entries[0];
        assert!(entry.model.is_complete());
        let output =
            std::env::temp_dir().join(format!("echo-archive-live2d-{}", std::process::id()));
        let options = ExportOptions {
            output: output.clone(),
            overwrite: false,
            verify_core: true,
        };
        let summary = export(
            &index,
            &entries,
            &options,
            &AtomicBool::new(false),
            &|_, _, _| {},
        )
        .unwrap();
        let exported = &summary.models[0];
        assert!(exported.complete && exported.report.integrity.complete);
        let directory = entry.output_dir(&output);

        // 16 non-texture files byte for byte, the texture pixel for pixel.
        let mut identical = 0;
        for file in &entry.model.files {
            let ours = std::fs::read(safe_join(&directory, &file.path).unwrap()).unwrap();
            let theirs = std::fs::read(safe_join(&sample, &file.path).unwrap()).unwrap();
            if file.role == Role::Texture {
                let a = image::load_from_memory(&ours).unwrap().to_rgba8();
                let b = image::load_from_memory(&theirs).unwrap().to_rgba8();
                assert_eq!(a.dimensions(), b.dimensions());
                let (mut max_diff, mut differing) = (0u8, 0usize);
                for (x, y) in a.pixels().zip(b.pixels()) {
                    let diff = (0..4).map(|i| x[i].abs_diff(y[i])).max().unwrap();
                    max_diff = max_diff.max(diff);
                    differing += usize::from(diff != 0);
                }
                println!(
                    "{}: {:?} max_diff {max_diff} differing_pixels {differing}",
                    file.path,
                    a.dimensions()
                );
                assert_eq!(differing, 0);
            } else {
                assert_eq!(ours, theirs, "{}", file.path);
                identical += 1;
            }
        }
        println!("byte-identical files: {identical}");
        assert_eq!(identical, 16);

        // Sources: path IDs and CAB names from sample-export.json; the
        // decoded copy hit-003.bundle is physical bundle
        // 047314567d374c5e4143e1093e8e59b0.bundle @ 5107108 (README).
        let reference: serde_json::Value =
            serde_json::from_slice(&std::fs::read(root.join("sample-export.json")).unwrap())
                .unwrap();
        for artifact in reference["artifacts"].as_array().unwrap() {
            let path = artifact["file"].as_str().unwrap().replace('\\', "/");
            let file = entry.model.files.iter().find(|f| f.path == path).unwrap();
            let source = file.source.as_ref().unwrap();
            assert_eq!(Some(source.path_id), artifact["path_id"].as_i64(), "{path}");
            let bundle = artifact["source_bundle"].as_str().unwrap();
            if bundle == "hit-003.bundle" {
                assert_eq!(
                    source.node.container.bundle,
                    "047314567d374c5e4143e1093e8e59b0.bundle"
                );
                assert_eq!(source.node.container.offset, 5_107_108);
            } else {
                assert_eq!(format!("{}.bundle", source.node.node), bundle, "{path}");
            }
        }
        let binding: serde_json::Value = serde_json::from_slice(
            &std::fs::read(root.join("lenna-texture-binding.json")).unwrap(),
        )
        .unwrap();
        let prefab = entry.model.prefab.as_ref().unwrap();
        assert_eq!(Some(prefab.root.path_id), binding["root"].as_i64());
        assert_eq!(
            Some(prefab.descendants as u64),
            binding["descendants"].as_u64()
        );
        let texture = entry
            .model
            .files
            .iter()
            .find(|f| f.role == Role::Texture)
            .unwrap();
        let texture_source = texture.source.as_ref().unwrap();
        for bound in binding["textures"].as_array().unwrap() {
            assert_eq!(
                Some(texture_source.path_id),
                bound["ref"]["m_PathID"].as_i64()
            );
            assert!(
                bound["external"]
                    .as_str()
                    .unwrap()
                    .ends_with(&texture_source.node.node)
            );
        }

        // Core check against runtime-validation.json.
        let runtime: serde_json::Value =
            serde_json::from_slice(&std::fs::read(root.join("runtime-validation.json")).unwrap())
                .unwrap();
        let Some(CoreCheck::Passed {
            report, ids_match, ..
        }) = &exported.report.core
        else {
            panic!("{:?}", exported.report.core);
        };
        println!(
            "core: parameters {} parts {} drawables {} mouth {:?}",
            report.parameter_count,
            report.part_count,
            report.drawable_count,
            report.mouth_changed_vertices
        );
        assert!(*ids_match);
        assert_eq!(
            Some(report.parameter_count as u64),
            runtime["parameter_count"].as_u64()
        );
        assert_eq!(
            Some(report.part_count as u64),
            runtime["part_count"].as_u64()
        );
        assert_eq!(
            Some(report.drawable_count as u64),
            runtime["drawable_count"].as_u64()
        );
        assert_eq!(
            report.mouth_changed_vertices.map(|v| v as u64),
            runtime["mouth_parameter_changed_vertices"].as_u64()
        );
        assert_eq!(
            serde_json::json!(report.parameter_ids),
            runtime["parameter_ids"]
        );
        assert_eq!(serde_json::json!(report.part_ids), runtime["part_ids"]);

        // A second export finds everything unchanged.
        let again = export(
            &index,
            &entries,
            &options,
            &AtomicBool::new(false),
            &|_, _, _| {},
        )
        .unwrap();
        assert_eq!(
            (again.written, again.unchanged, again.conflicts),
            (0, 17, 0)
        );
        std::fs::remove_dir_all(output).unwrap();
    }
}
