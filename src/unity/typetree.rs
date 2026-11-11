//! Reads object data into a generic value tree using its type tree.

use super::{reader::Reader, serialized::TypeTreeNode};
use anyhow::{Result, bail, ensure};

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Bool(bool),
    Int(i64),
    UInt(u64),
    Float(f64),
    String(String),
    Bytes(Vec<u8>),
    Array(Vec<Value>),
    Object(Vec<(String, Value)>),
}

impl Value {
    pub fn get(&self, name: &str) -> Option<&Value> {
        match self {
            Self::Object(fields) => fields.iter().find(|(key, _)| key == name).map(|(_, v)| v),
            _ => None,
        }
    }
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(value) => Some(value),
            _ => None,
        }
    }
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Self::Int(value) => Some(*value),
            Self::UInt(value) => i64::try_from(*value).ok(),
            Self::Bool(value) => Some(i64::from(*value)),
            _ => None,
        }
    }
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(value) => Some(*value),
            Self::Int(value) => Some(*value != 0),
            Self::UInt(value) => Some(*value != 0),
            _ => None,
        }
    }
    pub fn as_array(&self) -> Option<&[Value]> {
        match self {
            Self::Array(values) => Some(values),
            _ => None,
        }
    }
}

struct Tree<'a> {
    nodes: &'a [TypeTreeNode],
    children: Vec<Vec<usize>>,
}

impl<'a> Tree<'a> {
    fn new(nodes: &'a [TypeTreeNode]) -> Result<Self> {
        let mut children = vec![Vec::new(); nodes.len()];
        let mut stack: Vec<usize> = Vec::new();
        for (index, node) in nodes.iter().enumerate() {
            while stack
                .last()
                .is_some_and(|parent| nodes[*parent].level >= node.level)
            {
                stack.pop();
            }
            match stack.last() {
                Some(parent) => {
                    ensure!(
                        nodes[*parent].level + 1 == node.level,
                        tr!(
                            "类型树层级不连续",
                            "The type tree levels are not continuous"
                        )
                    );
                    children[*parent].push(index);
                }
                None => ensure!(
                    index == 0,
                    tr!("类型树有多个根节点", "The type tree has several root nodes")
                ),
            }
            stack.push(index);
        }
        Ok(Self { nodes, children })
    }
}

/// Upper bound on array lengths, guarding against corrupt data.
const MAX_ELEMENTS: usize = 64 * 1024 * 1024;

fn read_array(tree: &Tree, array: usize, reader: &mut Reader) -> Result<Value> {
    let size = reader.count()?;
    ensure!(
        size <= MAX_ELEMENTS,
        tr!(
            "数组长度异常：{size}",
            "Abnormal array length: {size}",
            size = size
        )
    );
    let element = *tree.children[array].get(1).ok_or_else(|| {
        anyhow::anyhow!(tr!(
            "数组节点缺少元素类型",
            "An array node has no element type"
        ))
    })?;
    let element_node = &tree.nodes[element];
    if tree.children[element].is_empty()
        && matches!(element_node.type_name.as_str(), "UInt8" | "SInt8" | "char")
    {
        ensure!(
            size <= reader.data.len() - reader.position,
            tr!("字节数组被截断", "The byte array is truncated")
        );
        let bytes = reader.bytes(size)?.to_vec();
        if element_node.aligns() {
            reader.align(4)?;
        }
        return Ok(Value::Bytes(bytes));
    }
    let mut values = Vec::with_capacity(size.min(4096));
    for _ in 0..size {
        values.push(read_node(tree, element, reader)?);
    }
    Ok(Value::Array(values))
}

fn read_node(tree: &Tree, index: usize, reader: &mut Reader) -> Result<Value> {
    let node = &tree.nodes[index];
    let children = &tree.children[index];
    let mut align = node.aligns();
    let value = match node.type_name.as_str() {
        "bool" => Value::Bool(reader.u8()? != 0),
        "SInt8" => Value::Int(i64::from(reader.u8()? as i8)),
        "UInt8" | "char" => Value::UInt(u64::from(reader.u8()?)),
        "SInt16" | "short" => Value::Int(i64::from(reader.i16()?)),
        "UInt16" | "unsigned short" => Value::UInt(u64::from(reader.u16()?)),
        "SInt32" | "int" => Value::Int(i64::from(reader.i32()?)),
        "UInt32" | "unsigned int" | "Type*" => Value::UInt(u64::from(reader.u32()?)),
        "SInt64" | "long long" => Value::Int(reader.i64()?),
        "UInt64" | "unsigned long long" | "FileSize" => Value::UInt(reader.u64()?),
        "float" => Value::Float(f64::from(reader.f32()?)),
        "double" => Value::Float(reader.f64()?),
        "string" => {
            let length = reader.count()?;
            let bytes = reader.bytes(length)?;
            if let Some(first) = children.first() {
                align |= tree.nodes[*first].aligns();
            }
            Value::String(String::from_utf8_lossy(bytes).into_owned())
        }
        "TypelessData" => {
            let length = reader.count()?;
            Value::Bytes(reader.bytes(length)?.to_vec())
        }
        "ManagedReferencesRegistry" | "ReferencedObject" => {
            bail!(tr!(
                "不支持 SerializeReference 数据",
                "SerializeReference data is not supported"
            ))
        }
        _ if node.is_array() && !children.is_empty() => read_array(tree, index, reader)?,
        _ if children
            .first()
            .is_some_and(|first| tree.nodes[*first].is_array()) =>
        {
            let first = children[0];
            align |= tree.nodes[first].aligns();
            read_array(tree, first, reader)?
        }
        _ if !children.is_empty() => {
            let mut fields = Vec::with_capacity(children.len());
            for child in children {
                fields.push((
                    tree.nodes[*child].name.clone(),
                    read_node(tree, *child, reader)?,
                ));
            }
            Value::Object(fields)
        }
        other => bail!(tr!(
            "未知的类型树基础类型 {other}",
            "Unknown type tree base type {other}",
            other = other
        )),
    };
    if align {
        reader.align(4)?;
    }
    Ok(value)
}

/// Reads one object's bytes. Object data starts 8-byte aligned in the file,
/// so alignment relative to the object start is equivalent.
pub fn read_object(nodes: &[TypeTreeNode], data: &[u8], big_endian: bool) -> Result<Value> {
    ensure!(
        !nodes.is_empty(),
        tr!("对象没有类型树", "The object has no type tree")
    );
    let tree = Tree::new(nodes)?;
    let mut reader = Reader::new(data, big_endian);
    read_node(&tree, 0, &mut reader)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(
        level: u8,
        type_name: &str,
        name: &str,
        meta_flag: u32,
        type_flags: u8,
    ) -> TypeTreeNode {
        TypeTreeNode {
            version: 1,
            level,
            type_flags,
            type_name: type_name.into(),
            name: name.into(),
            byte_size: -1,
            meta_flag,
        }
    }

    fn string_nodes(level: u8, name: &str) -> Vec<TypeTreeNode> {
        vec![
            node(level, "string", name, 0x8000, 0),
            node(level + 1, "Array", "Array", 0x4000, 1),
            node(level + 2, "int", "size", 0, 0),
            node(level + 2, "char", "data", 0, 0),
        ]
    }

    #[test]
    fn reads_nested_arrays_strings_and_alignment() {
        // MonoBehaviour { string m_Name; vector<Row> rows; bool flag; PPtr p }
        let mut nodes = vec![node(0, "MonoBehaviour", "Base", 0, 0)];
        nodes.extend(string_nodes(1, "m_Name"));
        nodes.extend([
            node(1, "vector", "rows", 0, 0),
            node(2, "Array", "Array", 0x4000, 1),
            node(3, "int", "size", 0, 0),
            node(3, "Row", "data", 0, 0),
            node(4, "int", "rowIndex", 0, 0),
            node(4, "vector", "strings", 0, 0),
            node(5, "Array", "Array", 0x4000, 1),
            node(6, "int", "size", 0, 0),
        ]);
        nodes.extend(string_nodes(6, "data"));
        nodes.extend([
            node(4, "bool", "isEmpty", 0x4000, 0),
            node(1, "PPtr<Object>", "p", 0, 0),
            node(2, "int", "m_FileID", 0, 0),
            node(2, "SInt64", "m_PathID", 0, 0),
        ]);
        let mut data = Vec::new();
        data.extend(5i32.to_le_bytes());
        data.extend(b"hello\0\0\0");
        data.extend(1i32.to_le_bytes()); // one row
        data.extend(7i32.to_le_bytes()); // rowIndex
        data.extend(2i32.to_le_bytes()); // two strings
        data.extend(1i32.to_le_bytes());
        data.extend(b"a\0\0\0");
        data.extend(3i32.to_le_bytes());
        data.extend("文".as_bytes());
        data.push(0); // string alignment
        data.push(1); // isEmpty, then aligned
        data.extend([0, 0, 0]);
        data.extend(0i32.to_le_bytes());
        data.extend((-9i64).to_le_bytes());
        let value = read_object(&nodes, &data, false).unwrap();
        assert_eq!(value.get("m_Name").unwrap().as_str(), Some("hello"));
        let rows = value.get("rows").unwrap().as_array().unwrap();
        assert_eq!(rows[0].get("rowIndex").unwrap().as_i64(), Some(7));
        let strings = rows[0].get("strings").unwrap().as_array().unwrap();
        assert_eq!(strings[1].as_str(), Some("文"));
        assert_eq!(rows[0].get("isEmpty").unwrap().as_bool(), Some(true));
        assert_eq!(
            value.get("p").unwrap().get("m_PathID").unwrap().as_i64(),
            Some(-9)
        );
        assert!(read_object(&nodes, &data[..data.len() - 2], false).is_err());
    }

    #[test]
    fn rejects_broken_hierarchy() {
        let nodes = vec![node(0, "A", "Base", 0, 0), node(2, "int", "x", 0, 0)];
        assert!(read_object(&nodes, &[0; 4], false).is_err());
        let nodes = vec![node(0, "A", "Base", 0, 0), node(0, "B", "Base", 0, 0)];
        assert!(read_object(&nodes, &[0; 4], false).is_err());
    }
}
