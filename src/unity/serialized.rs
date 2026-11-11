//! SerializedFile metadata (formats 17–22, which covers Unity 2019.4) with
//! embedded type trees and the object table.

use super::reader::Reader;
use anyhow::{Context, Result, bail, ensure};
use std::sync::OnceLock;

/// Unity's built-in type tree string table; offsets with the high bit set
/// index into this buffer.
const COMMON_STRINGS: &[&str] = &[
    "AABB",
    "AnimationClip",
    "AnimationCurve",
    "AnimationState",
    "Array",
    "Base",
    "BitField",
    "bitset",
    "bool",
    "char",
    "ColorRGBA",
    "Component",
    "data",
    "deque",
    "double",
    "dynamic_array",
    "FastPropertyName",
    "first",
    "float",
    "Font",
    "GameObject",
    "Generic Mono",
    "GradientNEW",
    "GUID",
    "GUIStyle",
    "int",
    "list",
    "long long",
    "map",
    "Matrix4x4f",
    "MdFour",
    "MonoBehaviour",
    "MonoScript",
    "m_ByteSize",
    "m_Curve",
    "m_EditorClassIdentifier",
    "m_EditorHideFlags",
    "m_Enabled",
    "m_ExtensionPtr",
    "m_GameObject",
    "m_Index",
    "m_IsArray",
    "m_IsStatic",
    "m_MetaFlag",
    "m_Name",
    "m_ObjectHideFlags",
    "m_PrefabInternal",
    "m_PrefabParentObject",
    "m_Script",
    "m_StaticEditorFlags",
    "m_Type",
    "m_Version",
    "Object",
    "pair",
    "PPtr<Component>",
    "PPtr<GameObject>",
    "PPtr<Material>",
    "PPtr<MonoBehaviour>",
    "PPtr<MonoScript>",
    "PPtr<Object>",
    "PPtr<Prefab>",
    "PPtr<Sprite>",
    "PPtr<TextAsset>",
    "PPtr<Texture>",
    "PPtr<Texture2D>",
    "PPtr<Transform>",
    "Prefab",
    "Quaternionf",
    "Rectf",
    "RectInt",
    "RectOffset",
    "second",
    "set",
    "short",
    "size",
    "SInt16",
    "SInt32",
    "SInt64",
    "SInt8",
    "staticvector",
    "string",
    "TextAsset",
    "TextMesh",
    "Texture",
    "Texture2D",
    "Transform",
    "TypelessData",
    "UInt16",
    "UInt32",
    "UInt64",
    "UInt8",
    "unsigned int",
    "unsigned long long",
    "unsigned short",
    "vector",
    "Vector2f",
    "Vector3f",
    "Vector4f",
    "m_ScriptingClassIdentifier",
    "Gradient",
    "Type*",
    "int2_storage",
    "int3_storage",
    "BoundsInt",
    "m_CorrespondingSourceObject",
    "m_PrefabInstance",
    "m_PrefabAsset",
    "FileSize",
    "Hash128",
];

fn common_buffer() -> &'static [u8] {
    static BUFFER: OnceLock<Vec<u8>> = OnceLock::new();
    BUFFER.get_or_init(|| {
        let mut buffer = Vec::new();
        for value in COMMON_STRINGS {
            buffer.extend(value.as_bytes());
            buffer.push(0);
        }
        buffer
    })
}

fn string_at(buffer: &[u8], offset: usize) -> Result<String> {
    let rest = buffer.get(offset..).context(tr!(
        "类型树字符串偏移无效",
        "Invalid type tree string offset"
    ))?;
    let end = rest.iter().position(|b| *b == 0).unwrap_or(rest.len());
    Ok(String::from_utf8_lossy(&rest[..end]).into_owned())
}

fn tree_string(local: &[u8], offset: u32) -> Result<String> {
    if offset & 0x8000_0000 != 0 {
        string_at(common_buffer(), (offset & 0x7fff_ffff) as usize)
    } else {
        string_at(local, offset as usize)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TypeTreeNode {
    pub version: u16,
    pub level: u8,
    pub type_flags: u8,
    pub type_name: String,
    pub name: String,
    pub byte_size: i32,
    pub meta_flag: u32,
}

impl TypeTreeNode {
    pub fn is_array(&self) -> bool {
        self.type_flags & 1 != 0 || self.type_name == "Array"
    }
    pub fn aligns(&self) -> bool {
        self.meta_flag & 0x4000 != 0
    }
}

#[derive(Clone, Debug, Default)]
pub struct SerializedType {
    pub class_id: i32,
    pub nodes: Vec<TypeTreeNode>,
}

impl SerializedType {
    pub fn has_field(&self, name: &str) -> bool {
        self.nodes.iter().any(|node| node.name == name)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObjectInfo {
    pub path_id: i64,
    pub byte_start: u64,
    pub byte_size: u32,
    pub type_index: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileHeader {
    pub metadata_size: u64,
    pub file_size: u64,
    pub version: u32,
    pub data_offset: u64,
    pub big_endian: bool,
    pub header_size: usize,
}

/// Bytes needed to parse the fixed header of any supported format.
pub const HEADER_PROBE: usize = 48;

pub fn parse_file_header(data: &[u8]) -> Result<FileHeader> {
    let mut reader = Reader::new(data, true);
    let mut metadata_size = u64::from(reader.u32()?);
    let mut file_size = u64::from(reader.u32()?);
    let version = reader.u32()?;
    let mut data_offset = u64::from(reader.u32()?);
    ensure!(
        (17..=22).contains(&version),
        tr!(
            "不支持的 SerializedFile 版本 {version}",
            "Unsupported SerializedFile version {version}",
            version = version
        )
    );
    let big_endian = reader.u8()? != 0;
    reader.skip(3)?;
    if version >= 22 {
        metadata_size = u64::from(reader.u32()?);
        file_size = reader.u64()?;
        data_offset = reader.u64()?;
        reader.skip(8)?;
    }
    Ok(FileHeader {
        metadata_size,
        file_size,
        version,
        data_offset,
        big_endian,
        header_size: reader.position,
    })
}

pub struct SerializedFile {
    pub header: FileHeader,
    pub types: Vec<SerializedType>,
    pub objects: Vec<ObjectInfo>,
    /// External file paths; a PPtr `m_FileID` of n refers to entry n - 1.
    /// Empty when the table could not be read: not every caller needs it,
    /// so a problem there does not fail the parse.
    pub externals: Vec<String>,
}

impl SerializedFile {
    /// Parses the header and metadata. `data` must cover at least
    /// `header_size + metadata_size` bytes; object data is not needed.
    pub fn parse(data: &[u8]) -> Result<Self> {
        let header = parse_file_header(data)?;
        let end = header.header_size as u64 + header.metadata_size;
        ensure!(
            end <= data.len() as u64,
            tr!(
                "SerializedFile 元数据被截断",
                "The SerializedFile metadata is truncated"
            )
        );
        let mut reader = Reader::new(&data[..end as usize], header.big_endian);
        reader.position = header.header_size;
        let version = header.version;
        reader.cstring()?; // Unity version
        reader.i32()?; // Target platform
        let type_tree_enabled = reader.u8()? != 0;
        let mut types = Vec::new();
        for _ in 0..reader.count()? {
            types.push(read_type(&mut reader, version, type_tree_enabled, false)?);
        }
        let mut objects = Vec::new();
        for _ in 0..reader.count()? {
            reader.align(4)?;
            let path_id = reader.i64()?;
            let byte_start = if version >= 22 {
                reader.u64()?
            } else {
                u64::from(reader.u32()?)
            };
            let byte_size = reader.u32()?;
            let type_index = reader.i32()?;
            ensure!(
                type_index >= 0 && (type_index as usize) < types.len(),
                tr!("对象类型索引无效", "Invalid object type index")
            );
            objects.push(ObjectInfo {
                path_id,
                byte_start,
                byte_size,
                type_index: type_index as usize,
            });
        }
        let externals = read_externals(&mut reader).unwrap_or_default();
        Ok(Self {
            header,
            types,
            objects,
            externals,
        })
    }

    pub fn find_object(&self, path_id: i64) -> Option<&ObjectInfo> {
        self.objects.iter().find(|object| object.path_id == path_id)
    }

    pub fn object_range(&self, object: &ObjectInfo) -> (u64, usize) {
        (
            self.header.data_offset + object.byte_start,
            object.byte_size as usize,
        )
    }
}

/// Skips the script type table and reads the external file paths
/// (formats 17–22 share this layout).
fn read_externals(reader: &mut Reader) -> Result<Vec<String>> {
    for _ in 0..reader.count()? {
        reader.i32()?; // Local serialized file index.
        reader.align(4)?;
        reader.i64()?; // Local identifier in file.
    }
    let mut externals = Vec::new();
    for _ in 0..reader.count()? {
        reader.cstring()?; // Temporary path, usually empty.
        reader.skip(16)?; // GUID.
        reader.i32()?; // Type.
        externals.push(reader.cstring()?);
    }
    Ok(externals)
}

fn read_type(
    reader: &mut Reader,
    version: u32,
    type_tree_enabled: bool,
    is_ref_type: bool,
) -> Result<SerializedType> {
    let class_id = reader.i32()?;
    let _stripped = reader.u8()?;
    let script_type_index = reader.i16()?;
    if (is_ref_type && script_type_index >= 0) || class_id < 0 || class_id == 114 {
        reader.skip(16)?; // Script ID.
    }
    reader.skip(16)?; // Old type hash.
    let mut nodes = Vec::new();
    if type_tree_enabled {
        let node_count = reader.count()?;
        let string_size = reader.count()?;
        let node_size = if version >= 19 { 32 } else { 24 };
        let node_bytes = reader.bytes(
            node_count
                .checked_mul(node_size)
                .context(tr!("类型树节点数过大", "The type tree has too many nodes"))?,
        )?;
        let strings = reader.bytes(string_size)?;
        let mut nodes_reader = Reader::new(node_bytes, reader.big_endian);
        for _ in 0..node_count {
            let node_version = nodes_reader.u16()?;
            let level = nodes_reader.u8()?;
            let type_flags = nodes_reader.u8()?;
            let type_offset = nodes_reader.u32()?;
            let name_offset = nodes_reader.u32()?;
            let byte_size = nodes_reader.i32()?;
            let _index = nodes_reader.i32()?;
            let meta_flag = nodes_reader.u32()?;
            if version >= 19 {
                nodes_reader.skip(8)?;
            }
            nodes.push(TypeTreeNode {
                version: node_version,
                level,
                type_flags,
                type_name: tree_string(strings, type_offset)?,
                name: tree_string(strings, name_offset)?,
                byte_size,
                meta_flag,
            });
        }
        if version >= 21 {
            if is_ref_type {
                reader.cstring()?;
                reader.cstring()?;
                reader.cstring()?;
            } else {
                let dependencies = reader.count()?;
                reader.skip(
                    dependencies
                        .checked_mul(4)
                        .context(tr!("依赖数过大", "Too many dependencies"))?,
                )?;
            }
        }
    }
    if nodes.first().is_some_and(|node| node.level != 0) {
        bail!(tr!("类型树根节点层级无效", "Invalid type tree root level"));
    }
    Ok(SerializedType { class_id, nodes })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn common_string_offsets_match_unity() {
        let buffer = common_buffer();
        for (offset, value) in [
            (0, "AABB"),
            (49, "Array"),
            (106, "data"),
            (222, "int"),
            (263, "MonoBehaviour"),
            (427, "m_Name"),
            (490, "m_Script"),
            (616, "PPtr<MonoScript>"),
            (795, "size"),
            (814, "SInt64"),
            (840, "string"),
            (928, "UInt8"),
            (981, "vector"),
            (1093, "m_CorrespondingSourceObject"),
            (1161, "Hash128"),
        ] {
            assert_eq!(string_at(buffer, offset).unwrap(), value);
        }
    }

    /// Minimal builder for little-endian format-21 files used by tests.
    /// (level, type, name, meta flag, type flags)
    pub type NodeSpec = (u8, &'static str, &'static str, u32, u8);

    pub struct FileBuilder {
        pub types: Vec<(i32, Vec<NodeSpec>)>,
        pub objects: Vec<(i64, usize, Vec<u8>)>,
        pub externals: Vec<&'static str>,
    }

    impl FileBuilder {
        pub fn build(&self) -> Vec<u8> {
            let mut meta = b"2019.4.29f1\0".to_vec();
            meta.extend(19i32.to_le_bytes());
            meta.push(1);
            meta.extend((self.types.len() as i32).to_le_bytes());
            for (class_id, nodes) in &self.types {
                meta.extend(class_id.to_le_bytes());
                meta.push(0);
                meta.extend((-1i16).to_le_bytes());
                if *class_id == 114 {
                    meta.extend([0; 16]);
                }
                meta.extend([0; 16]);
                let mut strings = Vec::new();
                let mut offset = |value: &str| {
                    let at = strings.len() as u32;
                    strings.extend(value.as_bytes());
                    strings.push(0);
                    at
                };
                let mut encoded = Vec::new();
                for (level, type_name, name, meta_flag, type_flags) in nodes {
                    encoded.extend(1u16.to_le_bytes());
                    encoded.push(*level);
                    encoded.push(*type_flags);
                    encoded.extend(offset(type_name).to_le_bytes());
                    encoded.extend(offset(name).to_le_bytes());
                    encoded.extend((-1i32).to_le_bytes());
                    encoded.extend(0i32.to_le_bytes());
                    encoded.extend(meta_flag.to_le_bytes());
                    encoded.extend(0u64.to_le_bytes());
                }
                meta.extend((nodes.len() as i32).to_le_bytes());
                meta.extend((strings.len() as i32).to_le_bytes());
                meta.extend(encoded);
                meta.extend(strings);
                meta.extend(0i32.to_le_bytes());
            }
            meta.extend((self.objects.len() as i32).to_le_bytes());
            let mut data = Vec::new();
            let mut starts = Vec::new();
            for (_, _, bytes) in &self.objects {
                starts.push(data.len());
                data.extend(bytes);
                data.resize(data.len().div_ceil(8) * 8, 0);
            }
            for ((path_id, type_index, bytes), start) in self.objects.iter().zip(&starts) {
                while !(20 + meta.len()).is_multiple_of(4) {
                    meta.push(0);
                }
                meta.extend(path_id.to_le_bytes());
                meta.extend((*start as u32).to_le_bytes());
                meta.extend((bytes.len() as u32).to_le_bytes());
                meta.extend((*type_index as i32).to_le_bytes());
            }
            meta.extend(0i32.to_le_bytes()); // script types
            meta.extend((self.externals.len() as i32).to_le_bytes());
            for path in &self.externals {
                meta.push(0);
                meta.extend([0; 16]);
                meta.extend(0i32.to_le_bytes());
                meta.extend(path.as_bytes());
                meta.push(0);
            }
            meta.extend([0; 4]); // reference types
            let data_offset = (20 + meta.len()).div_ceil(16) * 16;
            let mut file = Vec::new();
            file.extend((meta.len() as u32).to_be_bytes());
            file.extend(((data_offset + data.len()) as u32).to_be_bytes());
            file.extend(21u32.to_be_bytes());
            file.extend((data_offset as u32).to_be_bytes());
            file.extend([0; 4]);
            file.extend(meta);
            file.resize(data_offset, 0);
            file.extend(data);
            file
        }
    }

    #[test]
    fn parses_types_and_objects() {
        let file = FileBuilder {
            types: vec![(
                114,
                vec![
                    (0, "MonoBehaviour", "Base", 0, 0),
                    (1, "string", "m_Name", 0x8000, 0),
                    (2, "Array", "Array", 0x4000, 1),
                    (3, "int", "size", 0, 0),
                    (3, "char", "data", 0, 0),
                ],
            )],
            objects: vec![(-5, 0, vec![1, 0, 0, 0, b'a', 0, 0, 0])],
            externals: vec!["archive:/CAB-x/CAB-x"],
        }
        .build();
        let parsed = SerializedFile::parse(&file).unwrap();
        assert_eq!(parsed.header.version, 21);
        assert_eq!(parsed.types[0].class_id, 114);
        assert_eq!(parsed.types[0].nodes[1].name, "m_Name");
        assert!(parsed.types[0].nodes[2].is_array());
        assert_eq!(parsed.objects[0].path_id, -5);
        assert_eq!(parsed.objects[0].byte_size, 8);
        assert_eq!(parsed.externals, ["archive:/CAB-x/CAB-x"]);
        assert!(parsed.find_object(-5).is_some());
        assert!(SerializedFile::parse(&file[..30]).is_err());
        let mut wrong = file.clone();
        wrong[11] = 9;
        assert!(SerializedFile::parse(&wrong).is_err());
    }
}
