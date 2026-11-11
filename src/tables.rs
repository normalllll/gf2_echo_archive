//! Readers for the game's `.bytes` data tables.
//!
//! Layout: a little-endian `u32` giving the byte length of a sparse seek
//! index, the index itself (skipped), then a protobuf-style stream in which
//! every row is a length-delimited field 1 holding the row's own fields.

use anyhow::{Context, Result, bail, ensure};
use std::{collections::HashMap, path::Path};

pub const LANGUAGE_TABLE: &str = "LangPackageTableCnData.bytes";
pub const CONTENT_TABLE: &str = "AvgContentData.bytes";
pub const AUDIO_TABLE: &str = "AudioData.bytes";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FieldValue<'a> {
    Varint(u64),
    /// Length-delimited and fixed-width values; the reference keeps both as bytes.
    Bytes(&'a [u8]),
}

impl<'a> FieldValue<'a> {
    pub fn as_varint(&self) -> Option<u64> {
        match self {
            Self::Varint(value) => Some(*value),
            Self::Bytes(_) => None,
        }
    }
    pub fn as_bytes(&self) -> Option<&'a [u8]> {
        match self {
            Self::Bytes(value) => Some(value),
            Self::Varint(_) => None,
        }
    }
}

pub fn read_varint(data: &[u8], offset: &mut usize) -> Result<u64> {
    let mut value = 0u64;
    for shift in (0..70).step_by(7) {
        let Some(&byte) = data.get(*offset) else {
            bail!(tr!("protobuf varint 被截断", "Truncated protobuf varint"));
        };
        *offset += 1;
        if shift < 64 {
            value |= u64::from(byte & 127) << shift;
        }
        if byte < 128 {
            return Ok(value);
        }
    }
    bail!(tr!(
        "protobuf varint 过长",
        "The protobuf varint is too long"
    ))
}

/// Iterates `(field number, value)` pairs of one protobuf message.
pub struct Fields<'a> {
    data: &'a [u8],
    offset: usize,
    failed: bool,
}

pub fn fields(data: &[u8]) -> Fields<'_> {
    Fields {
        data,
        offset: 0,
        failed: false,
    }
}

impl<'a> Iterator for Fields<'a> {
    type Item = Result<(u64, FieldValue<'a>)>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.failed || self.offset >= self.data.len() {
            return None;
        }
        let result = self.read_one();
        self.failed = result.is_err();
        Some(result)
    }
}

impl<'a> Fields<'a> {
    fn read_one(&mut self) -> Result<(u64, FieldValue<'a>)> {
        let tag = read_varint(self.data, &mut self.offset)?;
        let (number, wire) = (tag >> 3, tag & 7);
        ensure!(
            number != 0,
            tr!("protobuf 字段号无效", "Invalid protobuf field number")
        );
        let value = match wire {
            0 => FieldValue::Varint(read_varint(self.data, &mut self.offset)?),
            1 | 2 | 5 => {
                let length = match wire {
                    2 => usize::try_from(read_varint(self.data, &mut self.offset)?).context(
                        tr!("protobuf 字段长度过大", "The protobuf field is too long"),
                    )?,
                    1 => 8,
                    _ => 4,
                };
                let end = self
                    .offset
                    .checked_add(length)
                    .filter(|end| *end <= self.data.len())
                    .context(tr!(
                        "protobuf 字段超出消息范围",
                        "The protobuf field is outside the message"
                    ))?;
                let value = &self.data[self.offset..end];
                self.offset = end;
                FieldValue::Bytes(value)
            }
            other => bail!(tr!(
                "不支持的 protobuf wire type {other}",
                "Unsupported protobuf wire type {other}",
                other = other
            )),
        };
        Ok((number, value))
    }
}

/// One table row; later duplicates of a field number replace earlier ones.
pub type Row<'a> = HashMap<u64, FieldValue<'a>>;

pub fn parse_row(data: &[u8]) -> Result<Row<'_>> {
    let mut row = HashMap::new();
    for field in fields(data) {
        let (number, value) = field?;
        row.insert(number, value);
    }
    Ok(row)
}

/// Splits a whole table file into row messages.
pub fn table_rows(data: &[u8]) -> Result<Vec<&[u8]>> {
    ensure!(
        data.len() >= 4,
        tr!("表头缺失", "The table header is missing")
    );
    let index_size = u32::from_le_bytes(data[..4].try_into().expect("4 bytes")) as usize;
    let start = 4usize
        .checked_add(index_size)
        .filter(|start| *start <= data.len())
        .context(tr!(
            "表索引超出文件范围",
            "The table index is outside the file"
        ))?;
    let mut rows = Vec::new();
    for field in fields(&data[start..]) {
        match field? {
            (1, FieldValue::Bytes(row)) => rows.push(row),
            _ => bail!(tr!("不支持的表行封装", "Unsupported table row wrapper")),
        }
    }
    Ok(rows)
}

fn read_table(directory: &Path, name: &str) -> Result<Vec<u8>> {
    std::fs::read(directory.join(name))
        .with_context(|| tr!("无法读取表 {name}", "Cannot read table {name}", name = name))
}

fn utf8(value: Option<&FieldValue>) -> Result<String> {
    match value {
        None => Ok(String::new()),
        Some(FieldValue::Bytes(bytes)) => Ok(std::str::from_utf8(bytes)
            .context(tr!(
                "表文本不是有效 UTF-8",
                "The table text is not valid UTF-8"
            ))?
            .to_owned()),
        Some(FieldValue::Varint(_)) => bail!(tr!(
            "表文本字段类型错误",
            "The table text field has the wrong type"
        )),
    }
}

fn key(row: &Row) -> Result<u64> {
    row.get(&1)
        .and_then(FieldValue::as_varint)
        .context(tr!("表行缺少数字 ID", "A table row has no numeric ID"))
}

/// A reference message `{1: language id}` stored in an AvgContent field.
pub fn text_reference(value: Option<&[u8]>, texts: &HashMap<u64, String>) -> Option<String> {
    let reference = parse_row(value?).ok()?;
    texts.get(&reference.get(&1)?.as_varint()?).cloned()
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ContentEntry {
    /// Field 2: language reference of the default text version.
    pub primary: Option<Vec<u8>>,
    /// Field 3: language reference of the alternate text version.
    pub alternate: Option<Vec<u8>>,
}

#[derive(Default)]
pub struct StoryTables {
    pub texts: HashMap<u64, String>,
    pub content: HashMap<u64, ContentEntry>,
    pub audio: HashMap<u64, String>,
}

impl StoryTables {
    pub fn load(table_directory: &Path) -> Result<Self> {
        let texts = parse_language(&read_table(table_directory, LANGUAGE_TABLE)?)
            .context(LANGUAGE_TABLE)?;
        let content =
            parse_content(&read_table(table_directory, CONTENT_TABLE)?).context(CONTENT_TABLE)?;
        let audio = parse_audio(&read_table(table_directory, AUDIO_TABLE)?).context(AUDIO_TABLE)?;
        Ok(Self {
            texts,
            content,
            audio,
        })
    }
}

pub fn parse_language(data: &[u8]) -> Result<HashMap<u64, String>> {
    let mut result = HashMap::new();
    for row in table_rows(data)? {
        let row = parse_row(row)?;
        result.insert(key(&row)?, utf8(row.get(&2))?);
    }
    Ok(result)
}

pub fn parse_content(data: &[u8]) -> Result<HashMap<u64, ContentEntry>> {
    let mut result = HashMap::new();
    for row in table_rows(data)? {
        let row = parse_row(row)?;
        let bytes = |number| {
            row.get(&number)
                .and_then(FieldValue::as_bytes)
                .map(<[u8]>::to_vec)
        };
        result.insert(
            key(&row)?,
            ContentEntry {
                primary: bytes(2),
                alternate: bytes(3),
            },
        );
    }
    Ok(result)
}

pub fn parse_audio(data: &[u8]) -> Result<HashMap<u64, String>> {
    let mut result = HashMap::new();
    for row in table_rows(data)? {
        let row = parse_row(row)?;
        result.insert(key(&row)?, utf8(row.get(&3))?);
    }
    Ok(result)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub fn varint(mut value: u64) -> Vec<u8> {
        let mut out = Vec::new();
        loop {
            let byte = (value & 127) as u8;
            value >>= 7;
            if value == 0 {
                out.push(byte);
                return out;
            }
            out.push(byte | 128);
        }
    }
    pub fn bytes_field(number: u64, value: &[u8]) -> Vec<u8> {
        let mut out = varint(number << 3 | 2);
        out.extend(varint(value.len() as u64));
        out.extend(value);
        out
    }
    pub fn varint_field(number: u64, value: u64) -> Vec<u8> {
        let mut out = varint(number << 3);
        out.extend(varint(value));
        out
    }
    pub fn table(index: &[u8], rows: &[Vec<u8>]) -> Vec<u8> {
        let mut out = (index.len() as u32).to_le_bytes().to_vec();
        out.extend(index);
        for row in rows {
            out.extend(bytes_field(1, row));
        }
        out
    }

    #[test]
    fn varint_round_trip_and_errors() {
        for value in [0, 1, 127, 128, 300, 8_010_001, u64::MAX] {
            let encoded = varint(value);
            let mut offset = 0;
            assert_eq!(read_varint(&encoded, &mut offset).unwrap(), value);
            assert_eq!(offset, encoded.len());
        }
        let mut offset = 0;
        assert!(read_varint(&[0x80, 0x80], &mut offset).is_err());
        let mut offset = 0;
        assert!(read_varint(&[0xff; 11], &mut offset).is_err());
    }

    #[test]
    fn skips_sparse_index_and_reads_rows() {
        let mut row = varint_field(1, 7);
        row.extend(bytes_field(2, "你要离开？".as_bytes()));
        let other = [varint_field(1, 8), bytes_field(2, b"x")].concat();
        let data = table(&[9, 9, 9, 9, 9, 9], &[row, other]);
        let texts = parse_language(&data).unwrap();
        assert_eq!(texts.len(), 2);
        assert_eq!(texts[&7], "你要离开？");
        assert_eq!(texts[&8], "x");
    }

    #[test]
    fn handles_fixed_width_fields_and_duplicates() {
        let mut row = varint_field(1, 1);
        row.extend(varint(2 << 3 | 5));
        row.extend([1, 2, 3, 4]);
        row.extend(varint(2 << 3 | 1));
        row.extend([0; 8]);
        let parsed = parse_row(&row).unwrap();
        assert_eq!(parsed[&2], FieldValue::Bytes(&[0; 8]));
    }

    #[test]
    fn rejects_malformed_tables() {
        assert!(table_rows(&[1, 0]).is_err());
        assert!(table_rows(&[10, 0, 0, 0, 1]).is_err());
        // Field 2 is not a row envelope.
        let mut data = 0u32.to_le_bytes().to_vec();
        data.extend(bytes_field(2, b"abc"));
        assert!(table_rows(&data).is_err());
        // Row length beyond end of file.
        let mut data = 0u32.to_le_bytes().to_vec();
        data.extend([0x0a, 0x10, 1]);
        assert!(table_rows(&data).is_err());
        // Field number zero and unsupported wire types.
        assert!(parse_row(&[0x02, 0x00]).is_err());
        assert!(parse_row(&[0x0b]).is_err());
        // Missing numeric key.
        let data = table(&[], &[bytes_field(2, b"abc")]);
        assert!(parse_language(&data).is_err());
    }

    #[test]
    fn resolves_text_reference_and_missing_language_entry() {
        let texts = HashMap::from([(5, "未知对象".to_owned())]);
        let reference = varint_field(1, 5);
        assert_eq!(
            text_reference(Some(&reference), &texts).as_deref(),
            Some("未知对象")
        );
        assert_eq!(text_reference(Some(&varint_field(1, 6)), &texts), None);
        assert_eq!(text_reference(None, &texts), None);
        assert_eq!(text_reference(Some(&[0xff]), &texts), None);
    }

    #[test]
    #[ignore = "needs GF2_GAME_DIR; the counts match one game version"]
    fn local_table_counts() {
        let Some(game) = crate::paths::test_game_dir() else {
            return;
        };
        let directory = crate::story::resolve_data_root(&game)
            .unwrap()
            .join("Table");
        let tables = StoryTables::load(&directory).unwrap();
        assert_eq!(tables.texts.len(), 526_885);
        assert_eq!(tables.content.len(), 178_624);
        assert_eq!(tables.audio.len(), 86_435);
    }
}
