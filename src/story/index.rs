//! Full scan of the physical bundles for Utage `AdvImportBook` objects.

use crate::unity::{
    SubBundle, UnityFs, Value,
    serialized::{HEADER_PROBE, SerializedFile, parse_file_header},
    split_physical_bundle,
    typetree::read_object,
};
use anyhow::{Context, Result, ensure};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::{
    fs::File,
    io::{Read, Seek},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};

pub const GRID_FIELD: &str = "importGridList";
/// Field of the `Adv.setting` object holding the game's setting sheets.
pub const SETTING_FIELD: &str = "settingList";
/// Sheet-name ending of the character setting table.
const CHARACTER_SHEET: &str = ":Character";

/// One row of the character setting table: the key story lines use in
/// `Arg2`, with the name filled down from the group's first row.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CharacterFile {
    #[serde(rename = "n")]
    pub name: String,
    #[serde(rename = "f")]
    pub file: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GridRow {
    #[serde(rename = "i")]
    pub row_index: i64,
    #[serde(rename = "s")]
    pub strings: Vec<String>,
    #[serde(rename = "e", default, skip_serializing_if = "is_false")]
    pub is_empty: bool,
    #[serde(rename = "c", default, skip_serializing_if = "is_false")]
    pub is_comment_out: bool,
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// One `importGridList` entry with the identity of the object it came from.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Grid {
    pub book: String,
    pub sheet: String,
    pub header_row: i64,
    pub rows: Vec<GridRow>,
    pub source_bundle: String,
    pub source_offset: u64,
    pub source_path_id: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanStats {
    pub physical_bundles: usize,
    pub sub_bundles: usize,
    /// Sub-bundles whose SerializedFile metadata was read.
    pub metadata_read: usize,
    /// Sub-bundles whose type trees mention `importGridList`.
    pub story_bundles: usize,
    pub story_objects: usize,
    pub grids: usize,
}

impl ScanStats {
    fn add(&mut self, other: &Self) {
        self.physical_bundles += other.physical_bundles;
        self.sub_bundles += other.sub_bundles;
        self.metadata_read += other.metadata_read;
        self.story_bundles += other.story_bundles;
        self.story_objects += other.story_objects;
        self.grids += other.grids;
    }
}

pub struct ScanOutput {
    pub grids: Vec<Grid>,
    pub character_files: Vec<CharacterFile>,
    pub warnings: Vec<String>,
    pub stats: ScanStats,
    pub cancelled: bool,
}

/// Top-level `*.bundle` files, sorted by name like the reference scanner.
pub fn physical_bundles(bundle_root: &Path) -> Result<Vec<PathBuf>> {
    let mut result = Vec::new();
    for entry in std::fs::read_dir(bundle_root)
        .with_context(|| tr!("无法读取 {}", "Cannot read {}", bundle_root.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_file()
            && path
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("bundle"))
        {
            result.push(path);
        }
    }
    result.sort();
    Ok(result)
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
            let result = scan_physical(path);
            let finished = done.fetch_add(1, Ordering::Relaxed) + 1;
            progress(finished, total);
            Some(result)
        })
        .collect();
    let mut output = ScanOutput {
        grids: Vec::new(),
        character_files: Vec::new(),
        warnings: Vec::new(),
        stats: ScanStats::default(),
        cancelled: cancel.load(Ordering::Relaxed),
    };
    for part in parts.into_iter().flatten() {
        output.grids.extend(part.grids);
        output.character_files.extend(part.character_files);
        output.warnings.extend(part.warnings);
        output.stats.add(&part.stats);
    }
    output
}

pub fn scan_physical(path: &Path) -> ScanOutput {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut output = ScanOutput {
        grids: Vec::new(),
        character_files: Vec::new(),
        warnings: Vec::new(),
        stats: ScanStats {
            physical_bundles: 1,
            ..ScanStats::default()
        },
        cancelled: false,
    };
    let opened = File::open(path).and_then(|file| Ok((file.metadata()?.len(), file)));
    let (total, file) = match opened {
        Ok(value) => value,
        Err(error) => {
            output.warnings.push(format!("{name}: {error}"));
            return output;
        }
    };
    let mut file = file;
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

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

/// Reads one sub-bundle; only the SerializedFile header and metadata are
/// decompressed unless the type trees mention `importGridList` or
/// `settingList`.
pub fn scan_sub_bundle<R: Read + Seek>(
    source: &mut R,
    bundle_name: &str,
    part: SubBundle,
    output: &mut ScanOutput,
) -> Result<()> {
    let mut fs = UnityFs::open(&mut *source, part.offset, part.size)?;
    let mut counted = false;
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
        let has_grids = contains(&metadata, GRID_FIELD.as_bytes());
        if !has_grids && !contains(&metadata, SETTING_FIELD.as_bytes()) {
            continue;
        }
        if has_grids && !counted {
            output.stats.story_bundles += 1;
            counted = true;
        }
        let file = SerializedFile::parse(&metadata)?;
        for object in &file.objects {
            let kind = &file.types[object.type_index];
            if kind.class_id != 114 {
                continue;
            }
            if !kind.has_field(GRID_FIELD) {
                if kind.has_field(SETTING_FIELD) {
                    let (start, length) = file.object_range(object);
                    let data = fs.read_node(&node, start, length)?;
                    let value = read_object(&kind.nodes, &data, file.header.big_endian)
                        .with_context(|| tr!("对象 {}", "Object {}", object.path_id))?;
                    output.character_files.extend(character_files(&value)?);
                }
                continue;
            }
            let (start, length) = file.object_range(object);
            let data = fs.read_node(&node, start, length)?;
            let value = read_object(&kind.nodes, &data, file.header.big_endian)
                .with_context(|| tr!("对象 {}", "Object {}", object.path_id))?;
            output.stats.story_objects += 1;
            let grids = grids_from_book(&value, bundle_name, part.offset, object.path_id)?;
            output.stats.grids += grids.len();
            output.grids.extend(grids);
        }
    }
    Ok(())
}

pub fn grids_from_book(
    value: &Value,
    bundle_name: &str,
    offset: u64,
    path_id: i64,
) -> Result<Vec<Grid>> {
    let book = value.get("m_Name").and_then(Value::as_str).context(tr!(
        "AdvImportBook 缺少 m_Name",
        "AdvImportBook has no m_Name"
    ))?;
    let mut result = Vec::new();
    for grid in value
        .get(GRID_FIELD)
        .and_then(Value::as_array)
        .context(tr!(
            "importGridList 不是数组",
            "importGridList is not an array"
        ))?
    {
        result.push(Grid {
            book: book.to_owned(),
            sheet: grid
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            header_row: grid.get("headerRow").and_then(Value::as_i64).unwrap_or(0),
            rows: grid_rows(grid)?,
            source_bundle: bundle_name.to_owned(),
            source_offset: offset,
            source_path_id: path_id,
        });
    }
    Ok(result)
}

fn grid_rows(grid: &Value) -> Result<Vec<GridRow>> {
    let mut rows = Vec::new();
    for row in grid
        .get("rows")
        .and_then(Value::as_array)
        .context(tr!("表格缺少 rows", "The sheet has no rows"))?
    {
        rows.push(GridRow {
            row_index: row
                .get("rowIndex")
                .and_then(Value::as_i64)
                .context(tr!("行缺少 rowIndex", "A row has no rowIndex"))?,
            strings: row
                .get("strings")
                .and_then(Value::as_array)
                .context(tr!("行缺少 strings", "A row has no strings"))?
                .iter()
                .map(|cell| {
                    cell.as_str()
                        .map(str::to_owned)
                        .context(tr!("单元格不是字符串", "A cell is not a string"))
                })
                .collect::<Result<_>>()?,
            is_empty: row.get("isEmpty").and_then(Value::as_bool).unwrap_or(false),
            is_comment_out: row
                .get("isCommentOut")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        });
    }
    Ok(rows)
}

/// Rows of the `Character` sheet in a `settingList` object; other setting
/// sheets are ignored.
fn character_files(value: &Value) -> Result<Vec<CharacterFile>> {
    let mut result = Vec::new();
    let Some(grids) = value.get(SETTING_FIELD).and_then(Value::as_array) else {
        return Ok(result);
    };
    for grid in grids {
        let is_character_sheet = grid
            .get("name")
            .and_then(Value::as_str)
            .is_some_and(|name| name.ends_with(CHARACTER_SHEET));
        if is_character_sheet {
            let header_row = grid.get("headerRow").and_then(Value::as_i64).unwrap_or(0);
            result.extend(character_rows(header_row, &grid_rows(grid)?));
        }
    }
    Ok(result)
}

/// `CharacterName` is written only on a group's first row; later rows of
/// the group leave it empty. Columns are located through the header row.
fn character_rows(header_row: i64, rows: &[GridRow]) -> Vec<CharacterFile> {
    fn cell(row: &GridRow, column: usize) -> &str {
        row.strings.get(column).map_or("", String::as_str)
    }
    let Some(header) = usize::try_from(header_row)
        .ok()
        .and_then(|index| rows.get(index))
    else {
        return Vec::new();
    };
    let column = |name: &str| header.strings.iter().position(|cell| cell == name);
    let (Some(name_column), Some(file_column)) = (column("CharacterName"), column("FileName"))
    else {
        return Vec::new();
    };
    let mut result = Vec::new();
    let mut current = "";
    for row in rows {
        if row.row_index <= header.row_index || row.is_empty || row.is_comment_out {
            continue;
        }
        if !cell(row, name_column).is_empty() {
            current = cell(row, name_column);
        }
        let file = cell(row, file_column);
        if !current.is_empty() && !file.is_empty() {
            result.push(CharacterFile {
                name: current.to_owned(),
                file: file.to_owned(),
            });
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn row(row_index: i64, strings: &[&str]) -> GridRow {
        GridRow {
            row_index,
            strings: strings.iter().map(|s| (*s).to_owned()).collect(),
            ..GridRow::default()
        }
    }

    #[test]
    fn character_rows_fill_names_down_and_find_columns_by_header() {
        let mut comment = row(5, &["x", "Commented", "ignored.prefab"]);
        comment.is_comment_out = true;
        let rows = [
            row(0, &["note"]),
            row(1, &["Pattern", "CharacterName", "FileName"]),
            row(2, &["", "兰汀", "CHA_Lenten_01.prefab"]),
            row(3, &["Sad", "", "CHA_Lenten_02.prefab"]),
            row(4, &["", "没有文件", ""]),
            comment,
            row(6, &["Idle", "", "SoundOnly.prefab"]),
        ];
        let result = character_rows(1, &rows);
        let pairs: Vec<(&str, &str)> = result
            .iter()
            .map(|r| (r.name.as_str(), r.file.as_str()))
            .collect();
        assert_eq!(
            pairs,
            [
                ("兰汀", "CHA_Lenten_01.prefab"),
                ("兰汀", "CHA_Lenten_02.prefab"),
                ("没有文件", "SoundOnly.prefab"),
            ]
        );
        // A sheet without the expected columns yields nothing.
        assert!(character_rows(0, &rows).is_empty());
    }

    fn sample_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".validation/story/chapter01-bundles")
    }

    /// Uses the decrypted local samples when present; never committed.
    #[test]
    fn reads_import_grids_from_samples() {
        let directory = sample_dir();
        if !directory.is_dir() {
            eprintln!("skipped: no local samples in {}", directory.display());
            return;
        }
        let mut found = Vec::new();
        for path in physical_bundles(&directory).unwrap() {
            let data = std::fs::read(&path).unwrap();
            let mut output = ScanOutput {
                grids: Vec::new(),
                character_files: Vec::new(),
                warnings: Vec::new(),
                stats: ScanStats::default(),
                cancelled: false,
            };
            let part = SubBundle {
                offset: 0,
                size: data.len() as u64,
            };
            scan_sub_bundle(&mut Cursor::new(data), "sample", part, &mut output).unwrap();
            found.extend(output.grids);
        }
        let grid = found
            .iter()
            .find(|grid| grid.book == "CN_M01001.book")
            .expect("CN_M01001.book");
        assert_eq!(
            grid.sheet,
            "Assets/Utage/BuildResources/MainStory/CN_Chapter01/CN_M01001.xls:CN_M01001"
        );
        let header = &grid.rows[grid.header_row as usize];
        assert_eq!(header.strings.first().map(String::as_str), Some("Command"));
        assert!(
            grid.rows
                .iter()
                .any(|row| row.strings.iter().any(|s| s == "101001004"))
        );
    }
}
