//! Full scan of the physical bundles for `Avatar_Whole_*` sprites and the
//! textures they reference, including textures in other sub-bundles.

use crate::unity::{
    SubBundle, UnityFs, Value,
    bundle::Node,
    serialized::{HEADER_PROBE, SerializedFile, parse_file_header},
    split_physical_bundle,
    sprite::{PPtr, Rect, Sprite},
    texture::{ImageData, Texture2D, format_name, is_supported},
    typetree::read_object,
};
use anyhow::{Context, Result, bail, ensure};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs::File,
    io::{Read, Seek},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};

pub const NAME_PREFIX: &str = "Avatar_Whole_";
const SPRITE_CLASS: i32 = 213;

/// A sub-bundle inside a physical `.bundle` file.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Container {
    pub bundle: String,
    pub offset: u64,
    pub size: u64,
}

/// A directory node (CAB file or `.resS`) inside a sub-bundle.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct NodeRef {
    #[serde(flatten)]
    pub container: Container,
    pub node: String,
}

/// Where a texture's first mip level can be read without parsing metadata.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DataRef {
    /// Range of a `.resS` node.
    Stream {
        node: NodeRef,
        offset: u64,
        size: u64,
    },
    /// Pixels stored in the `Texture2D` object itself.
    Inline { node: NodeRef, path_id: i64 },
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TextureInfo {
    pub name: String,
    pub path_id: i64,
    pub width: usize,
    pub height: usize,
    pub format: i64,
    pub format_name: String,
}

/// Why a sprite cannot be decoded. Stored in the index cache, so it holds no
/// display text.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Unsupported {
    NoTexture,
    AtlasTextureMissing,
    SeparateAlpha,
    TextureFileMissing { cab: String },
    Format { format: String },
    TextureUnreadable,
}

impl Unsupported {
    pub fn text(&self) -> String {
        match self {
            Self::NoTexture => {
                tr!("Sprite 没有引用贴图", "The sprite references no texture").into()
            }
            Self::AtlasTextureMissing => tr!(
                "来自 SpriteAtlas，找不到贴图",
                "From a SpriteAtlas; texture not found"
            )
            .into(),
            Self::SeparateAlpha => {
                tr!("使用单独的 Alpha 贴图", "Uses a separate alpha texture").into()
            }
            Self::TextureFileMissing { cab } => tr!(
                "找不到贴图所在文件 {}",
                "Cannot find the file holding the texture: {}",
                cab
            ),
            Self::Format { format } => tr!(
                "不支持的贴图格式 {}",
                "Unsupported texture format {}",
                format
            ),
            Self::TextureUnreadable => tr!(
                "无法读取贴图，详见扫描警告",
                "Cannot read the texture; see the scan warnings"
            )
            .into(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Portrait {
    pub name: String,
    pub source: NodeRef,
    pub path_id: i64,
    pub texture_rect: Rect,
    pub settings_raw: u32,
    /// Set for tightly packed or rotated sprites (cropped by rectangle only).
    pub packing: Option<String>,
    pub texture: Option<TextureInfo>,
    pub data: Option<DataRef>,
    /// Why this sprite cannot be decoded; `None` when it can.
    pub unsupported: Option<Unsupported>,
    /// Size and modification time of the source bundles; part of the
    /// thumbnail cache key.
    pub stamp: String,
}

impl Portrait {
    pub fn width(&self) -> usize {
        self.texture_rect.width.round() as usize
    }
    pub fn height(&self) -> usize {
        self.texture_rect.height.round() as usize
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanStats {
    pub physical_bundles: usize,
    pub sub_bundles: usize,
    pub metadata_read: usize,
    /// Serialized files whose type trees contain a Sprite.
    pub sprite_files: usize,
    pub sprites_peeked: usize,
    pub portraits: usize,
    pub textures_read: usize,
    /// Node names that appear in more than one sub-bundle.
    pub duplicate_nodes: usize,
}

impl ScanStats {
    fn add(&mut self, other: &Self) {
        self.physical_bundles += other.physical_bundles;
        self.sub_bundles += other.sub_bundles;
        self.metadata_read += other.metadata_read;
        self.sprite_files += other.sprite_files;
        self.sprites_peeked += other.sprites_peeked;
        self.portraits += other.portraits;
    }
}

/// A matching sprite before its texture has been located.
struct Candidate {
    portrait: Portrait,
    texture_cab: Option<String>,
    texture_path_id: i64,
    alpha_texture: PPtr,
    sprite_atlas: PPtr,
}

/// Any Sprite seen during a scan, by name only (not decoded).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpriteRef {
    pub name: String,
    pub source: NodeRef,
    pub path_id: i64,
}

/// Which sprites a scan resolves fully, and whether it lists every name.
pub struct ScanSpec<'a> {
    pub matches: &'a (dyn Fn(&str) -> bool + Sync),
    pub collect_names: bool,
}

#[derive(Default)]
struct BundleScan {
    candidates: Vec<Candidate>,
    names: Vec<SpriteRef>,
    nodes: Vec<(String, NodeRef)>,
    warnings: Vec<String>,
    stats: ScanStats,
}

pub struct ScanOutput {
    pub portraits: Vec<Portrait>,
    /// Every Sprite name; empty unless `ScanSpec::collect_names` was set.
    pub sprite_names: Vec<SpriteRef>,
    pub warnings: Vec<String>,
    pub stats: ScanStats,
    pub cancelled: bool,
}

pub(crate) fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Lower-cased last path component, the key used for CAB / `.resS` lookups
/// (`archive:/CAB-x/CAB-x.resS` → `cab-x.ress`).
pub fn node_key(path: &str) -> String {
    path.rsplit(['/', '\\'])
        .next()
        .unwrap_or(path)
        .to_ascii_lowercase()
}

pub(crate) fn stamp(path: &Path) -> String {
    path.metadata()
        .map(|metadata| {
            let modified = metadata
                .modified()
                .ok()
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |time| time.as_secs());
            format!("{}-{modified}", metadata.len())
        })
        .unwrap_or_default()
}

pub fn scan_bundles(
    bundle_root: &Path,
    bundles: &[PathBuf],
    cancel: &AtomicBool,
    progress: &(dyn Fn(usize, usize) + Sync),
) -> ScanOutput {
    let spec = ScanSpec {
        matches: &|name| name.starts_with(NAME_PREFIX),
        collect_names: false,
    };
    scan_sprites(bundle_root, bundles, &spec, cancel, progress)
}

/// Like [`scan_bundles`] for any sprite name filter; optionally also lists
/// the name of every Sprite in the bundles.
pub fn scan_sprites(
    bundle_root: &Path,
    bundles: &[PathBuf],
    spec: &ScanSpec,
    cancel: &AtomicBool,
    progress: &(dyn Fn(usize, usize) + Sync),
) -> ScanOutput {
    let done = AtomicUsize::new(0);
    let total = bundles.len();
    let parts: Vec<_> = bundles
        .par_iter()
        .map(|path| {
            if cancel.load(Ordering::Relaxed) {
                return None;
            }
            let result = scan_physical(path, spec);
            progress(done.fetch_add(1, Ordering::Relaxed) + 1, total);
            Some(result)
        })
        .collect();
    let mut output = ScanOutput {
        portraits: Vec::new(),
        sprite_names: Vec::new(),
        warnings: Vec::new(),
        stats: ScanStats::default(),
        cancelled: cancel.load(Ordering::Relaxed),
    };
    if output.cancelled {
        return output;
    }
    let mut nodes: HashMap<String, NodeRef> = HashMap::new();
    let mut candidates = Vec::new();
    for part in parts.into_iter().flatten() {
        output.stats.add(&part.stats);
        output.warnings.extend(part.warnings);
        candidates.extend(part.candidates);
        output.sprite_names.extend(part.names);
        for (key, node) in part.nodes {
            match nodes.entry(key) {
                std::collections::hash_map::Entry::Occupied(_) => {
                    output.stats.duplicate_nodes += 1;
                }
                std::collections::hash_map::Entry::Vacant(slot) => {
                    slot.insert(node);
                }
            }
        }
    }
    let resolved: Vec<(Portrait, Option<String>, bool)> = candidates
        .into_par_iter()
        .map(|candidate| resolve(candidate, &nodes, bundle_root))
        .collect();
    for (portrait, warning, read_texture) in resolved {
        output.stats.textures_read += usize::from(read_texture);
        output.warnings.extend(warning);
        output.portraits.push(portrait);
    }
    output.sprite_names.sort_by(|a, b| {
        a.name
            .cmp(&b.name)
            .then_with(|| a.source.container.bundle.cmp(&b.source.container.bundle))
            .then_with(|| a.path_id.cmp(&b.path_id))
    });
    output.portraits.sort_by(|a, b| {
        a.name
            .cmp(&b.name)
            .then_with(|| a.source.container.bundle.cmp(&b.source.container.bundle))
            .then_with(|| a.path_id.cmp(&b.path_id))
    });
    output
}

fn scan_physical(path: &Path, spec: &ScanSpec) -> BundleScan {
    let name = file_name(path);
    let mut output = BundleScan {
        stats: ScanStats {
            physical_bundles: 1,
            ..ScanStats::default()
        },
        ..BundleScan::default()
    };
    let opened = File::open(path).and_then(|file| Ok((file.metadata()?.len(), file)));
    let (total, mut file) = match opened {
        Ok(value) => value,
        Err(error) => {
            output.warnings.push(format!("{name}: {error}"));
            return output;
        }
    };
    let (parts, warnings) = split_physical_bundle(&mut file, total, &name);
    output.warnings.extend(warnings);
    output.stats.sub_bundles = parts.len();
    let stamp = stamp(path);
    for part in parts {
        if let Err(error) = scan_sub_bundle(&mut file, &name, &stamp, part, spec, &mut output) {
            output
                .warnings
                .push(format!("{name}@{}: {error:#}", part.offset));
        }
    }
    output
}

pub(crate) fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

/// Reads only `m_Name` (the first field of every named object).
pub(crate) fn peek_name<R: Read + Seek>(
    fs: &mut UnityFs<R>,
    node: &Node,
    start: u64,
    length: usize,
    big_endian: bool,
) -> Result<String> {
    ensure!(length >= 4, tr!("对象过小", "The object is too small"));
    let head = fs.read_node(node, start, 4)?;
    let size = if big_endian {
        u32::from_be_bytes(head[..4].try_into().expect("4 bytes"))
    } else {
        u32::from_le_bytes(head[..4].try_into().expect("4 bytes"))
    } as usize;
    ensure!(
        size <= length - 4,
        tr!("对象名称长度无效", "Invalid object name length")
    );
    // Only the prefix decides whether the object is read in full.
    let probe = size.min(256);
    let bytes = fs.read_node(node, start + 4, probe)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn scan_sub_bundle<R: Read + Seek>(
    source: &mut R,
    bundle_name: &str,
    stamp: &str,
    part: SubBundle,
    spec: &ScanSpec,
    output: &mut BundleScan,
) -> Result<()> {
    let mut fs = UnityFs::open(&mut *source, part.offset, part.size)?;
    let container = Container {
        bundle: bundle_name.to_owned(),
        offset: part.offset,
        size: part.size,
    };
    for node in &fs.nodes {
        output.nodes.push((
            node_key(&node.path),
            NodeRef {
                container: container.clone(),
                node: node.path.clone(),
            },
        ));
    }
    for node in fs.nodes.clone() {
        if !node.is_serialized_file() || node.size < HEADER_PROBE as u64 {
            continue;
        }
        let head = fs.read_node(&node, 0, HEADER_PROBE)?;
        let header = parse_file_header(&head)?;
        let metadata_end = header.header_size as u64 + header.metadata_size;
        ensure!(
            metadata_end <= node.size,
            tr!(
                "SerializedFile 元数据超出节点",
                "SerializedFile metadata exceeds its node"
            )
        );
        let metadata = fs.read_node(&node, 0, metadata_end as usize)?;
        output.stats.metadata_read += 1;
        if !contains(&metadata, b"m_RD") {
            continue;
        }
        let file = SerializedFile::parse(&metadata)?;
        if !file.types.iter().any(|t| t.class_id == SPRITE_CLASS) {
            continue;
        }
        output.stats.sprite_files += 1;
        for object in &file.objects {
            let kind = &file.types[object.type_index];
            if kind.class_id != SPRITE_CLASS {
                continue;
            }
            let (start, length) = file.object_range(object);
            output.stats.sprites_peeked += 1;
            let name = peek_name(&mut fs, &node, start, length, file.header.big_endian)?;
            if spec.collect_names {
                output.names.push(SpriteRef {
                    name: name.clone(),
                    source: NodeRef {
                        container: container.clone(),
                        node: node.path.clone(),
                    },
                    path_id: object.path_id,
                });
            }
            if !(spec.matches)(&name) {
                continue;
            }
            let data = fs.read_node(&node, start, length)?;
            let value = read_object(&kind.nodes, &data, file.header.big_endian)
                .with_context(|| format!("Sprite {}", object.path_id))?;
            let sprite = Sprite::from_value(&value)?;
            output.stats.portraits += 1;
            let texture_cab = match sprite.texture.file_id {
                _ if sprite.texture.is_null() => None,
                0 => Some(node_key(&node.path)),
                id => file
                    .externals
                    .get(id as usize - 1)
                    .map(|path| node_key(path)),
            };
            output.candidates.push(Candidate {
                portrait: Portrait {
                    name: sprite.name.clone(),
                    source: NodeRef {
                        container: container.clone(),
                        node: node.path.clone(),
                    },
                    path_id: object.path_id,
                    texture_rect: sprite.texture_rect,
                    settings_raw: sprite.settings.0,
                    packing: sprite.settings.packing_label(),
                    stamp: stamp.to_owned(),
                    ..Portrait::default()
                },
                texture_cab,
                texture_path_id: sprite.texture.path_id,
                alpha_texture: sprite.alpha_texture,
                sprite_atlas: sprite.sprite_atlas,
            });
        }
    }
    Ok(())
}

pub(crate) fn open_node<'a>(
    bundle_root: &Path,
    node: &NodeRef,
    file: &'a mut Option<File>,
) -> Result<(UnityFs<&'a mut File>, Node)> {
    let path = bundle_root.join(&node.container.bundle);
    let handle = file.insert(
        File::open(&path).with_context(|| tr!("无法打开 {}", "Cannot open {}", path.display()))?,
    );
    let fs = UnityFs::open(handle, node.container.offset, node.container.size)?;
    let found = fs
        .nodes
        .iter()
        .find(|n| n.path == node.node)
        .cloned()
        .with_context(|| tr!("子包中没有 {}", "{} is not in the sub-bundle", node.node))?;
    Ok((fs, found))
}

/// Reads one object of a SerializedFile node as a value tree.
pub fn read_node_object(bundle_root: &Path, node: &NodeRef, path_id: i64) -> Result<Value> {
    read_object_with_siblings(bundle_root, node, path_id).map(|(value, _)| value)
}

/// Also returns the node paths of the same sub-bundle.
pub(crate) fn read_object_with_siblings(
    bundle_root: &Path,
    node: &NodeRef,
    path_id: i64,
) -> Result<(Value, Vec<String>)> {
    let mut file = None;
    let (mut fs, found) = open_node(bundle_root, node, &mut file)?;
    let head = fs.read_node(&found, 0, HEADER_PROBE)?;
    let header = parse_file_header(&head)?;
    let metadata_end = header.header_size as u64 + header.metadata_size;
    ensure!(
        metadata_end <= found.size,
        tr!(
            "SerializedFile 元数据超出节点",
            "SerializedFile metadata exceeds its node"
        )
    );
    let metadata = fs.read_node(&found, 0, metadata_end as usize)?;
    let serialized = SerializedFile::parse(&metadata)?;
    let object = serialized.find_object(path_id).with_context(|| {
        tr!(
            "{} 中没有对象 {path_id}",
            "{} has no object {path_id}",
            node.node,
            path_id = path_id
        )
    })?;
    let (start, length) = serialized.object_range(object);
    let data = fs.read_node(&found, start, length)?;
    let value = read_object(
        &serialized.types[object.type_index].nodes,
        &data,
        serialized.header.big_endian,
    )?;
    Ok((value, fs.nodes.iter().map(|n| n.path.clone()).collect()))
}

fn resolve(
    candidate: Candidate,
    nodes: &HashMap<String, NodeRef>,
    bundle_root: &Path,
) -> (Portrait, Option<String>, bool) {
    let mut portrait = candidate.portrait;
    let Some(cab) = candidate.texture_cab else {
        portrait.unsupported = Some(if candidate.sprite_atlas.is_null() {
            Unsupported::NoTexture
        } else {
            Unsupported::AtlasTextureMissing
        });
        return (portrait, None, false);
    };
    if !candidate.alpha_texture.is_null() {
        portrait.unsupported = Some(Unsupported::SeparateAlpha);
        return (portrait, None, false);
    }
    let Some(texture_node) = nodes.get(&cab) else {
        portrait.unsupported = Some(Unsupported::TextureFileMissing { cab });
        return (portrait, None, false);
    };
    match locate_texture(bundle_root, texture_node, candidate.texture_path_id, nodes) {
        Ok((info, data)) => {
            if !is_supported(info.format) {
                portrait.unsupported = Some(Unsupported::Format {
                    format: info.format_name.clone(),
                });
            }
            portrait.texture = Some(info);
            portrait.data = Some(data);
            (portrait, None, true)
        }
        Err(error) => {
            let warning = format!(
                "{} ({}#{}): {error:#}",
                portrait.name, portrait.source.container.bundle, portrait.path_id
            );
            portrait.unsupported = Some(Unsupported::TextureUnreadable);
            (portrait, Some(warning), true)
        }
    }
}

pub(crate) fn locate_texture(
    bundle_root: &Path,
    node: &NodeRef,
    path_id: i64,
    nodes: &HashMap<String, NodeRef>,
) -> Result<(TextureInfo, DataRef)> {
    let (value, siblings) = read_object_with_siblings(bundle_root, node, path_id)?;
    let texture = Texture2D::from_value(&value)?;
    let info = TextureInfo {
        name: texture.name.clone(),
        path_id,
        width: texture.width,
        height: texture.height,
        format: texture.format,
        format_name: format_name(texture.format),
    };
    let data = match texture.data {
        ImageData::Inline(_) => DataRef::Inline {
            node: node.clone(),
            path_id,
        },
        ImageData::Stream { path, offset, size } => {
            let key = node_key(&path);
            // Prefer the `.resS` next to the texture's own CAB file.
            let stream_node = match siblings.iter().find(|s| node_key(s) == key) {
                Some(local) => NodeRef {
                    container: node.container.clone(),
                    node: local.clone(),
                },
                None => nodes.get(&key).cloned().with_context(|| {
                    tr!(
                        "找不到贴图数据文件 {path}",
                        "Cannot find the texture data file {path}",
                        path = path
                    )
                })?,
            };
            DataRef::Stream {
                node: stream_node,
                offset,
                size,
            }
        }
    };
    Ok((info, data))
}

/// Reads the raw first-mip bytes of a texture.
pub fn read_texture_data(bundle_root: &Path, data: &DataRef) -> Result<Vec<u8>> {
    match data {
        DataRef::Stream { node, offset, size } => {
            let mut file = None;
            let (mut fs, found) = open_node(bundle_root, node, &mut file)?;
            fs.read_node(&found, *offset, usize::try_from(*size)?)
        }
        DataRef::Inline { node, path_id } => {
            let value = read_node_object(bundle_root, node, *path_id)?;
            match Texture2D::from_value(&value)?.data {
                ImageData::Inline(bytes) => Ok(bytes),
                ImageData::Stream { .. } => bail!(tr!(
                    "贴图数据位置已变化，请重建立绘索引",
                    "The texture data has moved; please rebuild the portrait index"
                )),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn portraits_round_trip_through_the_cache_format() {
        let node = NodeRef {
            container: Container {
                bundle: "a.bundle".into(),
                offset: 7,
                size: 9,
            },
            node: "CAB-a.resS".into(),
        };
        let portrait = Portrait {
            name: "Avatar_Whole_TestSSR".into(),
            source: node.clone(),
            path_id: -3,
            packing: Some("tight".into()),
            data: Some(DataRef::Stream {
                node,
                offset: 16,
                size: 32,
            }),
            ..Portrait::default()
        };
        let json = serde_json::to_string(&portrait).unwrap();
        assert_eq!(serde_json::from_str::<Portrait>(&json).unwrap(), portrait);
    }

    #[test]
    fn node_keys_ignore_archive_prefix_and_case() {
        assert_eq!(node_key("archive:/CAB-abc/CAB-abc.resS"), "cab-abc.ress");
        assert_eq!(node_key("CAB-abc"), "cab-abc");
        assert_eq!(node_key("archive:/cab-ABC/cab-abc"), "cab-abc");
    }

    use crate::unity::{
        bundle::tests::{build_bundle, encrypt},
        serialized::tests::{FileBuilder, NodeSpec},
    };

    fn pptr(level: u8, name: &'static str) -> Vec<NodeSpec> {
        vec![
            (level, "PPtr<Object>", name, 0, 0),
            (level + 1, "int", "m_FileID", 0, 0),
            (level + 1, "SInt64", "m_PathID", 0, 0),
        ]
    }

    fn rect(level: u8, name: &'static str) -> Vec<NodeSpec> {
        let mut nodes = vec![(level, "Rectf", name, 0, 0)];
        for field in ["x", "y", "width", "height"] {
            nodes.push((level + 1, "float", field, 0, 0));
        }
        nodes
    }

    fn string(level: u8, name: &'static str) -> Vec<NodeSpec> {
        vec![
            (level, "string", name, 0x8000, 0),
            (level + 1, "Array", "Array", 0x4000, 1),
            (level + 2, "int", "size", 0, 0),
            (level + 2, "char", "data", 0, 0),
        ]
    }

    fn sprite_bytes(name: &str, file_id: i32, path_id: i64) -> Vec<u8> {
        let mut data = (name.len() as i32).to_le_bytes().to_vec();
        data.extend(name.as_bytes());
        data.resize(data.len().div_ceil(4) * 4, 0);
        for value in [0.0f32, 0.0, 4.0, 2.0] {
            data.extend(value.to_le_bytes()); // m_Rect
        }
        data.extend(0i32.to_le_bytes()); // m_SpriteAtlas
        data.extend(0i64.to_le_bytes());
        data.extend(file_id.to_le_bytes()); // m_RD.texture
        data.extend(path_id.to_le_bytes());
        data.extend(0i32.to_le_bytes()); // m_RD.alphaTexture
        data.extend(0i64.to_le_bytes());
        for value in [1.0f32, 0.0, 2.0, 2.0] {
            data.extend(value.to_le_bytes()); // m_RD.textureRect
        }
        data.extend(64u32.to_le_bytes()); // settingsRaw
        data
    }

    fn sprite_file(names: &[&str], external: &'static str) -> Vec<u8> {
        let mut nodes = vec![(0, "Sprite", "Base", 0, 0)];
        nodes.extend(string(1, "m_Name"));
        nodes.extend(rect(1, "m_Rect"));
        nodes.extend(pptr(1, "m_SpriteAtlas"));
        nodes.push((1, "SpriteRenderData", "m_RD", 0, 0));
        nodes.extend(pptr(2, "texture"));
        nodes.extend(pptr(2, "alphaTexture"));
        nodes.extend(rect(2, "textureRect"));
        nodes.push((2, "unsigned int", "settingsRaw", 0, 0));
        FileBuilder {
            types: vec![(213, nodes)],
            objects: names
                .iter()
                .enumerate()
                .map(|(i, name)| (i as i64 + 1, 0, sprite_bytes(name, 1, 77)))
                .collect(),
            externals: vec![external],
        }
        .build()
    }

    fn texture_file(pixels: &[u8]) -> Vec<u8> {
        let mut nodes = vec![(0, "Texture2D", "Base", 0, 0)];
        nodes.extend(string(1, "m_Name"));
        for field in ["m_Width", "m_Height", "m_TextureFormat"] {
            nodes.push((1, "int", field, 0, 0));
        }
        nodes.push((1, "TypelessData", "image data", 0x4000, 0));
        let mut data = 1i32.to_le_bytes().to_vec();
        data.extend(b"t\0\0\0");
        for value in [4i32, 2, 4] {
            data.extend(value.to_le_bytes());
        }
        data.extend((pixels.len() as i32).to_le_bytes());
        data.extend(pixels);
        FileBuilder {
            types: vec![(28, nodes)],
            objects: vec![(77, 0, data)],
            externals: vec![],
        }
        .build()
    }

    /// The sprite's texture lives in a different sub-bundle of another
    /// physical bundle and is found through the external file table.
    #[test]
    fn resolves_textures_in_other_sub_bundles() {
        let directory = std::env::temp_dir().join(format!(
            "echo-archive-portrait-index-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let sprites = sprite_file(
            &["Avatar_Whole_TestSSR", "UI_Icon"],
            "archive:/CAB-tex/CAB-TEX",
        );
        let a = encrypt(
            &build_bundle(&[("CAB-sprite", &sprites, 4)], 64),
            b"0123456789abcdef",
        );
        // 4×2 RGBA32, bottom row first: value = x + 10 * y.
        let pixels: Vec<u8> = (0..2u8)
            .flat_map(|y| (0..4u8).flat_map(move |x| [x + 10 * y, 0, 0, 255]))
            .collect();
        let filler = encrypt(
            &build_bundle(&[("CAB-other", b"nothing", 0)], 64),
            b"ffffffffffffffff",
        );
        let mut b = filler;
        b.extend(encrypt(
            &build_bundle(&[("CAB-tex", &texture_file(&pixels), 4)], 64),
            b"fedcba9876543210",
        ));
        std::fs::write(directory.join("a.bundle"), a).unwrap();
        std::fs::write(directory.join("b.bundle"), b).unwrap();
        let bundles = crate::story::index::physical_bundles(&directory).unwrap();
        let output = scan_bundles(&directory, &bundles, &AtomicBool::new(false), &|_, _| {});
        assert!(output.warnings.is_empty(), "{:?}", output.warnings);
        assert_eq!(output.stats.sprites_peeked, 2);
        assert_eq!(output.portraits.len(), 1);
        let portrait = &output.portraits[0];
        assert_eq!(portrait.name, "Avatar_Whole_TestSSR");
        assert_eq!(portrait.unsupported, None);
        let texture = portrait.texture.as_ref().unwrap();
        assert_eq!((texture.width, texture.height, texture.format), (4, 2, 4));
        let Some(DataRef::Inline { node, path_id: 77 }) = &portrait.data else {
            panic!("{:?}", portrait.data);
        };
        assert_eq!(node.container.bundle, "b.bundle");
        assert!(node.container.offset > 0);
        // Decoding crops textureRect (x 1, width 2, full height) and flips.
        let image = super::super::decode(&directory, portrait).unwrap();
        let reds: Vec<u8> = image.pixels().map(|px| px[0]).collect();
        assert_eq!(reds, [11, 12, 1, 2]);
        assert!(output.sprite_names.is_empty());
        // A name survey lists every sprite and resolves only the matches.
        let spec = ScanSpec {
            matches: &|name| name.starts_with("UI_"),
            collect_names: true,
        };
        let survey = scan_sprites(
            &directory,
            &bundles,
            &spec,
            &AtomicBool::new(false),
            &|_, _| {},
        );
        let names: Vec<_> = survey
            .sprite_names
            .iter()
            .map(|s| s.name.as_str())
            .collect();
        assert_eq!(names, ["Avatar_Whole_TestSSR", "UI_Icon"]);
        assert_eq!(survey.portraits.len(), 1);
        assert_eq!(survey.portraits[0].name, "UI_Icon");
        std::fs::remove_dir_all(directory).unwrap();
    }
}
