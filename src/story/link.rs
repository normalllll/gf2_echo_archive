//! Row linking, identical in rules to the reference `build_story`.

use super::index::Grid;
use crate::tables::{StoryTables, text_reference};
use rayon::prelude::*;
use serde::{Serialize, Serializer, ser::SerializeMap};

/// Ordered cell map with Python `dict(zip(columns, values))` semantics.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Cells(pub Vec<(String, String)>);

impl Cells {
    pub fn from_row(columns: &[String], values: &[String]) -> Self {
        let mut cells: Vec<(String, String)> = Vec::new();
        for (column, value) in columns.iter().zip(values) {
            match cells.iter_mut().find(|(key, _)| key == column) {
                Some(existing) => existing.1 = value.clone(),
                None => cells.push((column.clone(), value.clone())),
            }
        }
        Self(cells)
    }
    pub fn get(&self, key: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.as_str())
    }
    /// Python `cells.get(key)` followed by a truthiness test.
    fn non_empty(&self, key: &str) -> Option<&str> {
        self.get(key).filter(|value| !value.is_empty())
    }
}

impl Serialize for Cells {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for (key, value) in &self.0 {
            map.serialize_entry(key, value)?;
        }
        map.end()
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Voice {
    pub language: &'static str,
    pub variant: &'static str,
    pub column: &'static str,
    pub audio_id: String,
    pub bank: Option<String>,
    pub cue: Option<String>,
    pub wav: Option<String>,
    pub decoder_warning: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sample_rate: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channels: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub samples: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Identity {
    pub script: String,
    pub sheet: String,
    pub row: i64,
    pub source_bundle: String,
    pub source_offset: u64,
    pub source_path_id: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Line {
    #[serde(flatten)]
    pub identity: Identity,
    pub sequence: usize,
    pub speaker_id: Option<String>,
    pub speaker: Option<String>,
    pub speaker_alt: Option<String>,
    pub content_id: String,
    pub text: Option<String>,
    pub text_alt: Option<String>,
    pub voices: Vec<Voice>,
    pub raw_cells: Cells,
}

impl Line {
    /// The speaking character's key in the character setting table.
    pub fn character_key(&self) -> Option<String> {
        self.raw_cells
            .get("Arg2")
            .filter(|key| !key.is_empty())
            .map(str::to_owned)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ControlRow {
    #[serde(flatten)]
    pub identity: Identity,
    pub cells: Cells,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Unresolved {
    #[serde(flatten)]
    pub identity: Identity,
    pub kind: &'static str,
    pub id: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Linked {
    pub lines: Vec<Line>,
    pub control_rows: Vec<ControlRow>,
    pub unresolved: Vec<Unresolved>,
}

fn digits(value: &str) -> Option<Option<u64>> {
    // `str.isdigit()` true → Some(parsed id, None when it overflows u64).
    (!value.is_empty() && value.bytes().all(|b| b.is_ascii_digit())).then(|| value.parse().ok())
}

fn is_digit(value: &str) -> bool {
    digits(value).is_some()
}

/// Resolves a content id through AvgContent to its two text versions.
fn resolve_content(value: &str, tables: &StoryTables) -> (Option<String>, Option<String>) {
    let entry = digits(value)
        .flatten()
        .and_then(|id| tables.content.get(&id));
    match entry {
        Some(entry) => (
            text_reference(entry.primary.as_deref(), &tables.texts),
            text_reference(entry.alternate.as_deref(), &tables.texts),
        ),
        None => {
            let literal = if is_digit(value) || value.is_empty() {
                None
            } else {
                Some(value.to_owned())
            };
            (literal.clone(), literal)
        }
    }
}

const VOICE_SLOTS: [(&str, &str, &str, &str); 4] = [
    ("CN", "default", "Voice", "Voice"),
    ("CN", "alternate", "Voice1", "Voice1"),
    ("JP", "default", "Voice_JP", "Voice"),
    ("JP", "alternate", "Voice1_JP", "Voice1"),
];

/// Links the given grids. Grids are ordered by book name, keeping scan order
/// for equal names, exactly like the reference.
pub fn link(grids: &[&Grid], tables: &StoryTables) -> Linked {
    let mut ordered: Vec<&Grid> = grids.to_vec();
    ordered.sort_by(|a, b| a.book.cmp(&b.book));
    let parts: Vec<Linked> = ordered
        .par_iter()
        .map(|grid| link_grid(grid, tables))
        .collect();
    let mut result = Linked::default();
    for part in parts {
        result.lines.extend(part.lines);
        result.control_rows.extend(part.control_rows);
        result.unresolved.extend(part.unresolved);
    }
    for (index, line) in result.lines.iter_mut().enumerate() {
        line.sequence = index + 1;
    }
    result
}

/// One grid's rows. `Line::sequence` is numbered by [`link`].
fn link_grid(grid: &Grid, tables: &StoryTables) -> Linked {
    let mut result = Linked::default();
    let Some(header) = usize::try_from(grid.header_row)
        .ok()
        .and_then(|index| grid.rows.get(index))
    else {
        return result;
    };
    let columns = &header.strings;
    for row in &grid.rows {
        if row.row_index <= header.row_index || row.is_empty || row.is_comment_out {
            continue;
        }
        let cells = Cells::from_row(columns, &row.strings);
        let identity = Identity {
            script: grid.book.clone(),
            sheet: grid.sheet.clone(),
            row: row.row_index,
            source_bundle: grid.source_bundle.clone(),
            source_offset: grid.source_offset,
            source_path_id: grid.source_path_id,
        };
        if cells.get("Command").unwrap_or("") != "Text" {
            result.control_rows.push(ControlRow { identity, cells });
            continue;
        }
        let text_id = cells.get("Text").unwrap_or("").to_owned();
        let speaker_id = cells.get("Arg1").unwrap_or("").to_owned();
        let (text, text_alt) = resolve_content(&text_id, tables);
        let (speaker, speaker_alt) = resolve_content(&speaker_id, tables);
        let mut voices = Vec::new();
        for (language, variant, column, fallback) in VOICE_SLOTS {
            let value = cells
                .non_empty(column)
                .or_else(|| cells.non_empty(fallback))
                .or_else(|| cells.get("Voice"))
                .unwrap_or("");
            if value.is_empty() {
                continue;
            }
            let resource = digits(value).flatten().and_then(|id| tables.audio.get(&id));
            let split = resource.and_then(|resource| resource.split_once('/'));
            let (bank, cue) = match (resource, split) {
                (_, Some((bank, cue))) => (bank.to_owned(), cue.to_owned()),
                (Some(resource), None) => (resource.clone(), String::new()),
                (None, None) => (String::new(), String::new()),
            };
            if split.is_none() {
                result.unresolved.push(Unresolved {
                    identity: identity.clone(),
                    kind: "audio",
                    id: value.to_owned(),
                });
            }
            voices.push(Voice {
                language,
                variant,
                column,
                audio_id: value.to_owned(),
                bank: (!bank.is_empty()).then_some(bank),
                cue: (!cue.is_empty()).then_some(cue),
                wav: None,
                decoder_warning: None,
                sample_rate: None,
                channels: None,
                samples: None,
            });
        }
        if !text_id.is_empty() && (text.is_none() || text_alt.is_none()) {
            result.unresolved.push(Unresolved {
                identity: identity.clone(),
                kind: "text",
                id: text_id.clone(),
            });
        }
        if !speaker_id.is_empty() && speaker.is_none() {
            result.unresolved.push(Unresolved {
                identity: identity.clone(),
                kind: "speaker",
                id: speaker_id.clone(),
            });
        }
        result.lines.push(Line {
            identity,
            sequence: 0,
            speaker_id: (!speaker_id.is_empty()).then_some(speaker_id),
            speaker,
            speaker_alt,
            content_id: text_id,
            text,
            text_alt,
            voices,
            raw_cells: cells,
        });
    }
    result
}

/// Script name used for selection: `CN_M01001` and `CN_M01001.book` match.
pub fn book_name(script: &str) -> String {
    format!("{}.book", script.strip_suffix(".book").unwrap_or(script))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        story::index::GridRow,
        tables::{ContentEntry, tests::varint_field},
    };

    fn row(index: i64, strings: &[&str]) -> GridRow {
        GridRow {
            row_index: index,
            strings: strings.iter().map(|s| s.to_string()).collect(),
            is_empty: false,
            is_comment_out: false,
        }
    }

    fn tables() -> StoryTables {
        let mut tables = StoryTables::default();
        tables.texts.insert(1, "你要离开？".into());
        tables.texts.insert(2, "你要离开吗？".into());
        tables.texts.insert(3, "未知对象".into());
        tables.content.insert(
            101,
            ContentEntry {
                primary: Some(varint_field(1, 1)),
                alternate: Some(varint_field(1, 2)),
            },
        );
        tables.content.insert(
            165,
            ContentEntry {
                primary: Some(varint_field(1, 3)),
                alternate: None,
            },
        );
        tables
            .audio
            .insert(8010004, "VO_Chapter_01_EP0/VO_Main_01_0004".into());
        tables
            .audio
            .insert(8011004, "VO_Chapter_01_EP0/VO_Main_01_0004_Female".into());
        tables.audio.insert(9, "NoSlash".into());
        tables
    }

    #[test]
    fn lines_follow_book_order_and_are_numbered_across_grids() {
        let grid = |book: &str, text: &str| Grid {
            book: book.into(),
            header_row: 0,
            rows: vec![
                row(0, &["Command", "Arg1", "Text"]),
                row(1, &["Text", "", text]),
                row(2, &["Text", "", text]),
            ],
            ..Grid::default()
        };
        let (b, a, also_a) = (grid("B", "b"), grid("A", "a1"), grid("A", "a2"));
        let linked = link(&[&b, &a, &also_a], &tables());
        let lines: Vec<_> = linked
            .lines
            .iter()
            .map(|line| (line.sequence, line.text.as_deref().unwrap()))
            .collect();
        assert_eq!(
            lines,
            [
                (1, "a1"),
                (2, "a1"),
                (3, "a2"),
                (4, "a2"),
                (5, "b"),
                (6, "b"),
            ]
        );
    }

    #[test]
    fn links_text_speaker_voices_and_controls() {
        let columns = [
            "Command",
            "Arg1",
            "Text",
            "Voice",
            "Voice1",
            "Voice_JP",
            "Voice1_JP",
        ];
        let mut empty = row(5, &["Text", "165", "101", "8010004", "", "", ""]);
        empty.is_empty = true;
        let grid = Grid {
            book: "CN_X.book".into(),
            sheet: "Assets/Utage/BuildResources/X.xls:X".into(),
            header_row: 0,
            rows: vec![
                row(0, &columns),
                row(1, &["Wait", "1"]),
                row(2, &["Text", "165", "101", "8010004", "8011004", "", ""]),
                row(3, &["Text", "旁白", "字面", "", "", "", ""]),
                row(4, &["Text", "777", "999", "9", "", "", ""]),
                empty,
            ],
            ..Grid::default()
        };
        let linked = link(&[&grid], &tables());
        assert_eq!(linked.control_rows.len(), 1);
        assert_eq!(linked.control_rows[0].cells.get("Arg1"), Some("1"));
        assert_eq!(linked.lines.len(), 3);
        let first = &linked.lines[0];
        assert_eq!(first.speaker.as_deref(), Some("未知对象"));
        assert_eq!(first.speaker_alt, None);
        assert_eq!(first.text.as_deref(), Some("你要离开？"));
        assert_eq!(first.text_alt.as_deref(), Some("你要离开吗？"));
        let cues: Vec<_> = first
            .voices
            .iter()
            .map(|v| (v.language, v.variant, v.column, v.cue.as_deref().unwrap()))
            .collect();
        assert_eq!(
            cues,
            [
                ("CN", "default", "Voice", "VO_Main_01_0004"),
                ("CN", "alternate", "Voice1", "VO_Main_01_0004_Female"),
                ("JP", "default", "Voice_JP", "VO_Main_01_0004"),
                ("JP", "alternate", "Voice1_JP", "VO_Main_01_0004_Female"),
            ]
        );
        let literal = &linked.lines[1];
        assert_eq!(literal.speaker.as_deref(), Some("旁白"));
        assert_eq!(literal.text_alt.as_deref(), Some("字面"));
        assert!(literal.voices.is_empty());
        let kinds: Vec<_> = linked
            .unresolved
            .iter()
            .map(|u| (u.kind, u.id.as_str()))
            .collect();
        assert_eq!(
            kinds,
            [
                ("audio", "9"),
                ("audio", "9"),
                ("audio", "9"),
                ("audio", "9"),
                ("text", "999"),
                ("speaker", "777")
            ]
        );
        assert_eq!(linked.lines[2].voices[0].bank.as_deref(), Some("NoSlash"));
        assert_eq!(linked.lines[2].voices[0].cue, None);
    }

    #[test]
    fn cells_follow_dict_zip_semantics() {
        let columns: Vec<String> = ["A", "B", "A"].iter().map(|s| s.to_string()).collect();
        let values: Vec<String> = ["1", "2"].iter().map(|s| s.to_string()).collect();
        let cells = Cells::from_row(&columns, &values);
        assert_eq!(
            cells.0,
            [("A".into(), "1".into()), ("B".into(), "2".into())]
        );
        let values: Vec<String> = ["1", "2", "3", "4"].iter().map(|s| s.to_string()).collect();
        let cells = Cells::from_row(&columns, &values);
        assert_eq!(
            cells.0,
            [("A".into(), "3".into()), ("B".into(), "2".into())]
        );
        assert_eq!(
            serde_json::to_string(&cells).unwrap(),
            r#"{"A":"3","B":"2"}"#
        );
    }

    #[test]
    fn book_names_accept_suffix() {
        assert_eq!(book_name("CN_M01001"), "CN_M01001.book");
        assert_eq!(book_name("CN_M01001.book"), "CN_M01001.book");
    }
}
