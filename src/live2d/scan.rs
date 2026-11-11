//! Full scan of the physical bundles for the pieces of Live2D runtime
//! models: `TextAsset`s (model3 manifests and the JSON files they list),
//! `CubismMoc` MonoBehaviours (`_bytes`) and model prefabs (root
//! GameObjects whose hierarchy carries renderers with `_mainTexture`).

use crate::{
    portrait::index::{Container, NodeRef, contains, file_name, node_key, stamp},
    unity::{
        SubBundle, UnityFs, Value,
        bundle::Node,
        reader::Reader,
        serialized::{HEADER_PROBE, SerializedFile, SerializedType, parse_file_header},
        split_physical_bundle,
        sprite::PPtr,
        typetree::read_object,
    },
};
use anyhow::{Context, Result, ensure};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    fs::File,
    io::{Read, Seek},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};

pub const GAME_OBJECT_CLASS: i32 = 1;
pub const TRANSFORM_CLASS: i32 = 4;
pub const TEXT_ASSET_CLASS: i32 = 49;
pub const MONO_BEHAVIOUR_CLASS: i32 = 114;
pub const RECT_TRANSFORM_CLASS: i32 = 224;
/// Field of `CubismMoc` holding the moc3 file.
pub const MOC_FIELD: &str = "_bytes";
/// Field of `CubismRenderer` holding the drawable's texture.
pub const TEXTURE_FIELD: &str = "_mainTexture";
/// Longest object name read while peeking.
const NAME_LIMIT: usize = 1024;

/// One object inside a SerializedFile node.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ObjectRef {
    pub node: NodeRef,
    pub path_id: i64,
}

impl ObjectRef {
    pub fn label(&self) -> String {
        format!("{}#{}", self.node.container.bundle, self.path_id)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Located {
    pub name: String,
    pub object: ObjectRef,
    /// Size of the serialized object (not of the payload).
    pub size: u32,
}

/// A texture referenced by a renderer, before it has been located.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct TextureTarget {
    /// Lower-cased CAB name (see [`node_key`]).
    pub cab: String,
    pub path_id: i64,
}

/// A root GameObject whose hierarchy holds `_mainTexture` renderers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Prefab {
    pub name: String,
    pub root: ObjectRef,
    pub descendants: usize,
    pub renderers: usize,
    /// Distinct textures in first-seen order (renderers sorted by path ID).
    pub textures: Vec<TextureTarget>,
    /// Renderers whose `_mainTexture` is empty or cannot be resolved.
    pub unresolved: usize,
    /// Node keys of the prefab file and the files it references.
    pub files: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanStats {
    pub physical_bundles: usize,
    pub sub_bundles: usize,
    pub metadata_read: usize,
    pub text_asset_files: usize,
    pub text_assets_peeked: usize,
    pub moc_files: usize,
    pub mocs: usize,
    pub prefab_files: usize,
    pub prefabs: usize,
    pub manifests: usize,
    /// Node names that appear in more than one sub-bundle.
    pub duplicate_nodes: usize,
}

impl ScanStats {
    fn add(&mut self, other: &Self) {
        self.physical_bundles += other.physical_bundles;
        self.sub_bundles += other.sub_bundles;
        self.metadata_read += other.metadata_read;
        self.text_asset_files += other.text_asset_files;
        self.text_assets_peeked += other.text_assets_peeked;
        self.moc_files += other.moc_files;
        self.mocs += other.mocs;
        self.prefab_files += other.prefab_files;
        self.prefabs += other.prefabs;
    }
}

#[derive(Default)]
pub struct ScanOutput {
    /// TextAssets whose name contains a dot (`x.model3`, `y.exp3`, ...).
    pub text_assets: Vec<Located>,
    pub mocs: Vec<Located>,
    pub prefabs: Vec<Prefab>,
    pub nodes: HashMap<String, NodeRef>,
    /// External file keys of SerializedFiles that hold TextAssets or mocs,
    /// keyed by their own node key.
    pub externals: HashMap<String, Vec<String>>,
    /// Size and modification time per physical bundle.
    pub stamps: HashMap<String, String>,
    pub warnings: Vec<String>,
    pub stats: ScanStats,
    pub cancelled: bool,
}

#[derive(Default)]
struct BundleScan {
    text_assets: Vec<Located>,
    mocs: Vec<Located>,
    prefabs: Vec<Prefab>,
    nodes: Vec<(String, NodeRef)>,
    externals: Vec<(String, Vec<String>)>,
    warnings: Vec<String>,
    stats: ScanStats,
}

pub fn scan_bundles(
    bundles: &[PathBuf],
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
            let result = (file_name(path), stamp(path), scan_physical(path));
            progress(done.fetch_add(1, Ordering::Relaxed) + 1, total);
            Some(result)
        })
        .collect();
    let mut output = ScanOutput {
        cancelled: cancel.load(Ordering::Relaxed),
        ..ScanOutput::default()
    };
    if output.cancelled {
        return output;
    }
    for (name, stamp, part) in parts.into_iter().flatten() {
        output.stamps.insert(name, stamp);
        output.stats.add(&part.stats);
        output.warnings.extend(part.warnings);
        output.text_assets.extend(part.text_assets);
        output.mocs.extend(part.mocs);
        output.prefabs.extend(part.prefabs);
        output.externals.extend(part.externals);
        for (key, node) in part.nodes {
            match output.nodes.entry(key) {
                std::collections::hash_map::Entry::Occupied(_) => {
                    output.stats.duplicate_nodes += 1;
                }
                std::collections::hash_map::Entry::Vacant(slot) => {
                    slot.insert(node);
                }
            }
        }
    }
    output.stats.manifests = output
        .text_assets
        .iter()
        .filter(|asset| is_manifest_name(&asset.name))
        .count();
    output
}

pub fn is_manifest_name(name: &str) -> bool {
    name.to_ascii_lowercase().ends_with(".model3")
}

fn scan_physical(path: &Path) -> BundleScan {
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
    for part in parts {
        if let Err(error) = scan_sub_bundle(&mut file, &name, part, &mut output) {
            output
                .warnings
                .push(format!("{name}@{}: {error:#}", part.offset));
        }
    }
    output
}

fn read_u32(bytes: &[u8], big_endian: bool) -> u32 {
    let bytes: [u8; 4] = bytes[..4].try_into().expect("4 bytes");
    if big_endian {
        u32::from_be_bytes(bytes)
    } else {
        u32::from_le_bytes(bytes)
    }
}

/// Reads a length-prefixed string at `at` inside an object.
fn peek_string<R: Read + Seek>(
    fs: &mut UnityFs<R>,
    node: &Node,
    start: u64,
    length: usize,
    at: usize,
    big_endian: bool,
) -> Result<String> {
    ensure!(length >= at + 4, tr!("对象过小", "The object is too small"));
    let size = read_u32(&fs.read_node(node, start + at as u64, 4)?, big_endian) as usize;
    ensure!(
        size <= length - at - 4,
        tr!("对象名称长度无效", "Invalid object name length")
    );
    let bytes = fs.read_node(node, start + at as u64 + 4, size.min(NAME_LIMIT))?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Names of the level-1 fields of a type, in order.
fn top_fields(kind: &SerializedType) -> Vec<&str> {
    kind.nodes
        .iter()
        .filter(|node| node.level == 1)
        .map(|node| node.name.as_str())
        .collect()
}

/// Offset of `m_Name` in a MonoBehaviour with the standard header
/// (`m_GameObject` PPtr, `m_Enabled` aligned, `m_Script` PPtr).
fn mono_name_offset(kind: &SerializedType) -> Option<usize> {
    let fields = top_fields(kind);
    (fields.len() >= 4 && fields[..4] == ["m_GameObject", "m_Enabled", "m_Script", "m_Name"])
        .then_some(28)
}

fn scan_sub_bundle<R: Read + Seek>(
    source: &mut R,
    bundle_name: &str,
    part: SubBundle,
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
        let file = SerializedFile::parse(&metadata)?;
        let node_ref = NodeRef {
            container: container.clone(),
            node: node.path.clone(),
        };
        let big_endian = file.header.big_endian;
        let has_text = file.types.iter().any(|t| t.class_id == TEXT_ASSET_CLASS);
        let moc_types: HashSet<usize> = matching_types(&file, MOC_FIELD, &metadata);
        let renderer_types: HashSet<usize> = matching_types(&file, TEXTURE_FIELD, &metadata);
        output.stats.text_asset_files += usize::from(has_text);
        output.stats.moc_files += usize::from(!moc_types.is_empty());
        if has_text || !moc_types.is_empty() {
            output.externals.push((
                node_key(&node.path),
                file.externals.iter().map(|path| node_key(path)).collect(),
            ));
        }
        for object in &file.objects {
            let kind = &file.types[object.type_index];
            let (start, length) = file.object_range(object);
            let object_ref = || ObjectRef {
                node: node_ref.clone(),
                path_id: object.path_id,
            };
            if kind.class_id == TEXT_ASSET_CLASS {
                output.stats.text_assets_peeked += 1;
                let name = peek_string(&mut fs, &node, start, length, 0, big_endian)?;
                if name.contains('.') {
                    output.text_assets.push(Located {
                        name,
                        object: object_ref(),
                        size: object.byte_size,
                    });
                }
            } else if moc_types.contains(&object.type_index) {
                let name = match mono_name_offset(kind) {
                    Some(at) => peek_string(&mut fs, &node, start, length, at, big_endian)?,
                    None => {
                        let data = fs.read_node(&node, start, length)?;
                        let value = read_object(&kind.nodes, &data, big_endian)?;
                        value
                            .get("m_Name")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned()
                    }
                };
                output.stats.mocs += 1;
                output.mocs.push(Located {
                    name,
                    object: object_ref(),
                    size: object.byte_size,
                });
            }
        }
        if !renderer_types.is_empty() {
            output.stats.prefab_files += 1;
            let prefabs = read_prefabs(&mut fs, &node, &file, &renderer_types, &node_ref)
                .with_context(|| tr!("预制体 {}", "Prefab {}", node.path))?;
            output.stats.prefabs += prefabs.len();
            output.prefabs.extend(prefabs);
        }
    }
    Ok(())
}

/// MonoBehaviour types that declare `field` at the top level.
fn matching_types(file: &SerializedFile, field: &str, metadata: &[u8]) -> HashSet<usize> {
    if !contains(metadata, field.as_bytes()) {
        return HashSet::new();
    }
    file.types
        .iter()
        .enumerate()
        .filter(|(_, kind)| {
            kind.class_id == MONO_BEHAVIOUR_CLASS
                && kind
                    .nodes
                    .iter()
                    .any(|node| node.level == 1 && node.name == field)
        })
        .map(|(index, _)| index)
        .collect()
}

struct TransformInfo {
    path_id: i64,
    game_object: i64,
    father: PPtr,
    children: Vec<PPtr>,
}

fn pptr_list(value: Option<&Value>, element: Option<&str>) -> Vec<PPtr> {
    value
        .and_then(Value::as_array)
        .unwrap_or_default()
        .iter()
        .map(|item| match element {
            Some(name) => PPtr::from_value(item.get(name)),
            None => PPtr::from_value(Some(item)),
        })
        .collect()
}

fn read_prefabs<R: Read + Seek>(
    fs: &mut UnityFs<R>,
    node: &Node,
    file: &SerializedFile,
    renderer_types: &HashSet<usize>,
    node_ref: &NodeRef,
) -> Result<Vec<Prefab>> {
    let big_endian = file.header.big_endian;
    let mut names: HashMap<i64, String> = HashMap::new();
    let mut transforms: Vec<TransformInfo> = Vec::new();
    // (renderer path ID, game object path ID, texture PPtr)
    let mut renderers: Vec<(i64, i64, PPtr)> = Vec::new();
    for object in &file.objects {
        let kind = &file.types[object.type_index];
        let wanted = matches!(
            kind.class_id,
            GAME_OBJECT_CLASS | TRANSFORM_CLASS | RECT_TRANSFORM_CLASS
        ) || renderer_types.contains(&object.type_index);
        if !wanted {
            continue;
        }
        let (start, length) = file.object_range(object);
        let data = fs.read_node(node, start, length)?;
        let value = read_object(&kind.nodes, &data, big_endian)
            .with_context(|| tr!("对象 {}", "Object {}", object.path_id))?;
        match kind.class_id {
            GAME_OBJECT_CLASS => {
                let name = value.get("m_Name").and_then(Value::as_str);
                names.insert(object.path_id, name.unwrap_or_default().to_owned());
            }
            TRANSFORM_CLASS | RECT_TRANSFORM_CLASS => transforms.push(TransformInfo {
                path_id: object.path_id,
                game_object: PPtr::from_value(value.get("m_GameObject")).path_id,
                father: PPtr::from_value(value.get("m_Father")),
                children: pptr_list(value.get("m_Children"), None),
            }),
            _ => renderers.push((
                object.path_id,
                PPtr::from_value(value.get("m_GameObject")).path_id,
                PPtr::from_value(value.get(TEXTURE_FIELD)),
            )),
        }
    }
    renderers.sort_by_key(|(id, _, _)| *id);
    let by_path: HashMap<i64, &TransformInfo> = transforms.iter().map(|t| (t.path_id, t)).collect();
    let mut prefabs = Vec::new();
    for transform in &transforms {
        if !transform.father.is_null() {
            continue;
        }
        let mut game_objects = HashSet::new();
        let mut todo = vec![transform];
        while let Some(current) = todo.pop() {
            if !game_objects.insert(current.game_object) {
                continue;
            }
            for child in &current.children {
                if child.file_id == 0
                    && let Some(next) = by_path.get(&child.path_id)
                {
                    todo.push(next);
                }
            }
        }
        let mut textures: Vec<TextureTarget> = Vec::new();
        let (mut count, mut unresolved) = (0, 0);
        for (_, game_object, texture) in &renderers {
            if !game_objects.contains(game_object) {
                continue;
            }
            count += 1;
            let cab = match texture.file_id {
                _ if texture.is_null() => None,
                0 => Some(node_key(&node.path)),
                id => file
                    .externals
                    .get(id as usize - 1)
                    .map(|path| node_key(path)),
            };
            let Some(cab) = cab else {
                unresolved += 1;
                continue;
            };
            let target = TextureTarget {
                cab,
                path_id: texture.path_id,
            };
            if !textures.contains(&target) {
                textures.push(target);
            }
        }
        if count == 0 {
            continue;
        }
        prefabs.push(Prefab {
            name: names
                .get(&transform.game_object)
                .cloned()
                .unwrap_or_default(),
            root: ObjectRef {
                node: node_ref.clone(),
                path_id: transform.game_object,
            },
            descendants: game_objects.len(),
            renderers: count,
            textures,
            unresolved,
            files: std::iter::once(node_key(&node.path))
                .chain(file.externals.iter().map(|path| node_key(path)))
                .collect(),
        });
    }
    Ok(prefabs)
}

/// An opened SerializedFile node for reading several objects.
pub struct NodeReader {
    fs: UnityFs<File>,
    node: Node,
    file: SerializedFile,
}

impl NodeReader {
    pub fn open(bundle_root: &Path, node: &NodeRef) -> Result<Self> {
        let path = bundle_root.join(&node.container.bundle);
        let handle = File::open(&path)
            .with_context(|| tr!("无法打开 {}", "Cannot open {}", path.display()))?;
        let mut fs = UnityFs::open(handle, node.container.offset, node.container.size)?;
        let found = fs
            .nodes
            .iter()
            .find(|n| n.path == node.node)
            .cloned()
            .with_context(|| tr!("子包中没有 {}", "{} is not in the sub-bundle", node.node))?;
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
        let file = SerializedFile::parse(&metadata)?;
        Ok(Self {
            fs,
            node: found,
            file,
        })
    }

    fn raw(&mut self, path_id: i64) -> Result<(&SerializedType, Vec<u8>)> {
        let object = self.file.find_object(path_id).with_context(|| {
            tr!(
                "{} 中没有对象 {path_id}",
                "{} has no object {path_id}",
                self.node.path,
                path_id = path_id
            )
        })?;
        let (start, length) = self.file.object_range(object);
        let data = self.fs.read_node(&self.node, start, length)?;
        Ok((&self.file.types[object.type_index], data))
    }

    /// Raw `m_Script` bytes of a TextAsset (not re-encoded).
    pub fn text_asset(&mut self, path_id: i64) -> Result<Vec<u8>> {
        let big_endian = self.file.header.big_endian;
        let (kind, data) = self.raw(path_id)?;
        ensure!(
            kind.class_id == TEXT_ASSET_CLASS,
            tr!(
                "对象 {path_id} 不是 TextAsset",
                "Object {path_id} is not a TextAsset",
                path_id = path_id
            )
        );
        text_asset_script(kind, &data, big_endian)
    }

    /// `_bytes` of a CubismMoc MonoBehaviour.
    pub fn moc_bytes(&mut self, path_id: i64) -> Result<Vec<u8>> {
        let big_endian = self.file.header.big_endian;
        let (kind, data) = self.raw(path_id)?;
        let value = read_object(&kind.nodes, &data, big_endian)?;
        match value.get(MOC_FIELD) {
            Some(Value::Bytes(bytes)) => Ok(bytes.clone()),
            Some(Value::Array(items)) => items
                .iter()
                .map(|item| {
                    item.as_i64()
                        .and_then(|v| u8::try_from(v).ok())
                        .context(tr!("_bytes 元素无效", "Invalid _bytes element"))
                })
                .collect(),
            _ => anyhow::bail!(tr!(
                "对象 {path_id} 没有 {MOC_FIELD}",
                "Object {path_id} has no {MOC_FIELD}",
                path_id = path_id,
                MOC_FIELD = MOC_FIELD
            )),
        }
    }
}

/// Splits a TextAsset into its raw script bytes. The layout is checked
/// against the type tree: `m_Name` then `m_Script`, both strings.
pub fn text_asset_script(kind: &SerializedType, data: &[u8], big_endian: bool) -> Result<Vec<u8>> {
    let fields = top_fields(kind);
    ensure!(
        fields.len() >= 2 && fields[..2] == ["m_Name", "m_Script"],
        tr!(
            "TextAsset 字段布局不符：{fields:?}",
            "Unexpected TextAsset field layout: {fields:?}",
            fields = fields
        )
    );
    let mut reader = Reader::new(data, big_endian);
    let name = reader.count()?;
    reader.skip(name)?;
    reader.align(4)?;
    let size = reader.count()?;
    Ok(reader.bytes(size)?.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::unity::serialized::TypeTreeNode;

    fn kind(fields: &[(&str, &str)]) -> SerializedType {
        let mut nodes = vec![TypeTreeNode {
            version: 1,
            level: 0,
            type_flags: 0,
            type_name: "TextAsset".into(),
            name: "Base".into(),
            byte_size: -1,
            meta_flag: 0,
        }];
        for (type_name, name) in fields {
            nodes.push(TypeTreeNode {
                version: 1,
                level: 1,
                type_flags: 0,
                type_name: (*type_name).into(),
                name: (*name).into(),
                byte_size: -1,
                meta_flag: 0x8000,
            });
        }
        SerializedType {
            class_id: TEXT_ASSET_CLASS,
            nodes,
        }
    }

    #[test]
    fn text_asset_bytes_are_returned_unchanged() {
        let kind = kind(&[("string", "m_Name"), ("string", "m_Script")]);
        let script = b"{\"a\":1}\xff\xfe";
        let mut data = 6i32.to_le_bytes().to_vec();
        data.extend(b"x.exp3\0\0");
        data.extend((script.len() as i32).to_le_bytes());
        data.extend(script);
        assert_eq!(text_asset_script(&kind, &data, false).unwrap(), script);
        assert!(text_asset_script(&kind, &data[..data.len() - 1], false).is_err());
        let wrong = self::kind(&[("string", "m_Script"), ("string", "m_Name")]);
        assert!(text_asset_script(&wrong, &data, false).is_err());
    }

    #[test]
    fn manifest_names_are_case_insensitive() {
        assert!(is_manifest_name("cha_lenna_ssr_01.model3"));
        assert!(is_manifest_name("CHA_X.Model3"));
        assert!(!is_manifest_name("CHA_X.model3.json.bak"));
        assert!(!is_manifest_name("CHA_X.cdi3"));
    }

    use crate::unity::{
        bundle::tests::{build_bundle, encrypt},
        serialized::tests::{FileBuilder, NodeSpec},
    };

    fn pptr_nodes(level: u8, name: &'static str) -> Vec<NodeSpec> {
        vec![
            (level, "PPtr<Object>", name, 0, 0),
            (level + 1, "int", "m_FileID", 0, 0),
            (level + 1, "SInt64", "m_PathID", 0, 0),
        ]
    }

    fn string_nodes(level: u8, name: &'static str) -> Vec<NodeSpec> {
        vec![
            (level, "string", name, 0x8000, 0),
            (level + 1, "Array", "Array", 0x4000, 1),
            (level + 2, "int", "size", 0, 0),
            (level + 2, "char", "data", 0, 0),
        ]
    }

    fn mono_header() -> Vec<NodeSpec> {
        let mut nodes = vec![(0, "MonoBehaviour", "Base", 0, 0)];
        nodes.extend(pptr_nodes(1, "m_GameObject"));
        nodes.push((1, "UInt8", "m_Enabled", 0x4000, 0));
        nodes.extend(pptr_nodes(1, "m_Script"));
        nodes.extend(string_nodes(1, "m_Name"));
        nodes
    }

    fn pptr(file_id: i32, path_id: i64) -> Vec<u8> {
        let mut data = file_id.to_le_bytes().to_vec();
        data.extend(path_id.to_le_bytes());
        data
    }

    fn string(value: &[u8]) -> Vec<u8> {
        let mut data = (value.len() as i32).to_le_bytes().to_vec();
        data.extend(value);
        data.resize(data.len().div_ceil(4) * 4, 0);
        data
    }

    fn mono(game_object: i64, name: &str) -> Vec<u8> {
        let mut data = pptr(0, game_object);
        data.extend([1, 0, 0, 0]);
        data.extend(pptr(0, 0));
        data.extend(string(name.as_bytes()));
        data
    }

    fn transform(game_object: i64, father: i64, children: &[i64]) -> Vec<u8> {
        let mut data = pptr(0, game_object);
        data.extend(pptr(0, father));
        data.extend((children.len() as i32).to_le_bytes());
        for child in children {
            data.extend(pptr(0, *child));
        }
        data
    }

    /// One SerializedFile with a model prefab (root + child with a
    /// renderer pointing at an external texture), an unrelated root, a
    /// manifest TextAsset and an unnamed CubismMoc.
    fn model_file() -> Vec<u8> {
        let mut game_object = vec![(0, "GameObject", "Base", 0, 0)];
        game_object.extend(string_nodes(1, "m_Name"));
        let mut transform_nodes = vec![(0, "Transform", "Base", 0, 0)];
        transform_nodes.extend(pptr_nodes(1, "m_GameObject"));
        transform_nodes.extend(pptr_nodes(1, "m_Father"));
        transform_nodes.extend([
            (1, "vector", "m_Children", 0, 0),
            (2, "Array", "Array", 0x4000, 1),
            (3, "int", "size", 0, 0),
        ]);
        transform_nodes.extend(pptr_nodes(3, "data"));
        let mut renderer = mono_header();
        renderer.extend(pptr_nodes(1, TEXTURE_FIELD));
        let mut text = vec![(0, "TextAsset", "Base", 0, 0)];
        text.extend(string_nodes(1, "m_Name"));
        text.extend(string_nodes(1, "m_Script"));
        let mut moc = mono_header();
        moc.extend([
            (1, "vector", MOC_FIELD, 0, 0),
            (2, "Array", "Array", 0x4000, 1),
            (3, "int", "size", 0, 0),
            (3, "UInt8", "data", 0, 0),
        ]);
        let mut renderer_data = mono(11, "");
        renderer_data.extend(pptr(1, 77));
        let mut moc_data = mono(0, "");
        moc_data.extend(8i32.to_le_bytes());
        moc_data.extend(b"MOC3\x03\0\0\0");
        let mut text_data = string(b"cha_test_01.model3");
        text_data.extend(string(
            b"{\"FileReferences\":{\"Moc\":\"CHA_Test_01.moc3\"}}",
        ));
        FileBuilder {
            types: vec![
                (GAME_OBJECT_CLASS, game_object),
                (TRANSFORM_CLASS, transform_nodes),
                (MONO_BEHAVIOUR_CLASS, renderer),
                (TEXT_ASSET_CLASS, text),
                (MONO_BEHAVIOUR_CLASS, moc),
            ],
            objects: vec![
                (10, 0, string(b"CHA_Test_01")),
                (11, 0, string(b"Drawable")),
                (12, 0, string(b"Unrelated")),
                (20, 1, transform(10, 0, &[21])),
                (21, 1, transform(11, 20, &[])),
                (22, 1, transform(12, 0, &[])),
                (30, 2, renderer_data),
                (40, 3, text_data),
                (50, 4, moc_data),
            ],
            externals: vec!["archive:/CAB-tex/CAB-tex"],
        }
        .build()
    }

    #[test]
    fn collects_prefabs_text_assets_and_mocs() {
        let directory =
            std::env::temp_dir().join(format!("echo-archive-live2d-scan-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let bundle = encrypt(
            &build_bundle(&[("CAB-test", &model_file(), 4)], 64),
            b"0123456789abcdef",
        );
        std::fs::write(directory.join("a.bundle"), bundle).unwrap();
        let output = scan_bundles(
            &[directory.join("a.bundle")],
            &AtomicBool::new(false),
            &|_, _| {},
        );
        assert!(output.warnings.is_empty(), "{:?}", output.warnings);
        assert_eq!(output.stats.manifests, 1);
        assert_eq!(output.text_assets[0].name, "cha_test_01.model3");
        assert_eq!(output.mocs.len(), 1);
        assert_eq!(output.mocs[0].name, "");
        assert_eq!(output.externals["cab-test"], ["cab-tex"]);
        assert_eq!(output.prefabs.len(), 1);
        let prefab = &output.prefabs[0];
        assert_eq!(prefab.name, "CHA_Test_01");
        assert_eq!(prefab.root.path_id, 10);
        assert_eq!(
            (prefab.descendants, prefab.renderers, prefab.unresolved),
            (2, 1, 0)
        );
        assert_eq!(
            prefab.textures,
            [TextureTarget {
                cab: "cab-tex".into(),
                path_id: 77
            }]
        );
        assert_eq!(prefab.files, ["cab-test", "cab-tex"]);

        let mut reader = NodeReader::open(&directory, &output.mocs[0].object.node).unwrap();
        assert_eq!(reader.moc_bytes(50).unwrap(), b"MOC3\x03\0\0\0");
        assert!(
            reader
                .text_asset(40)
                .unwrap()
                .starts_with(b"{\"FileReferences\"")
        );
        assert!(reader.text_asset(50).is_err());
        std::fs::remove_dir_all(directory).unwrap();
    }
}
