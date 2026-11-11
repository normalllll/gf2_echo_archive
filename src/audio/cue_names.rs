//! Maps AWB wave ids to cue names by walking an ACB's tables.
//!
//! The naming rules are ported from vgmstream's `acb.c` (`load_acb_wave_info`),
//! with one deliberate difference: vgmstream cuts the joined name at 254 bytes,
//! which leaves later cues unmatchable, so all names are kept here.

use cridecoder::acb::{UtfTable, Value};
use std::{
    collections::{HashMap, HashSet},
    io::Cursor,
};

const MAX_DEPTH: u32 = 3;
/// `PortNo` of a waveform that plays from any AWB port.
const ANY_PORT: u16 = 0xFFFF;
const NO_EVENT: u16 = 0xFFFF;

#[derive(Default)]
pub(super) struct CueTables {
    pub cue_names: Vec<CueName>,
    pub cues: Vec<Cue>,
    pub block_sequences: Vec<BlockSequence>,
    pub blocks: Vec<TrackList>,
    pub sequences: Vec<TrackList>,
    pub tracks: Vec<u16>,
    /// `TrackEvent` or `Command` rows: each one is a TLV list.
    pub commands: Vec<Vec<u8>>,
    /// `ReferenceItems` of each synth.
    pub synths: Vec<Vec<u8>>,
    pub waveforms: Vec<Waveform>,
}

pub(super) struct CueName {
    pub cue_index: u16,
    pub name: Option<String>,
}

pub(super) struct Cue {
    pub reference_type: u8,
    pub reference_index: u16,
}

#[derive(Default)]
pub(super) struct TrackList {
    pub count: u16,
    pub indexes: Vec<u8>,
}

#[derive(Default)]
pub(super) struct BlockSequence {
    pub tracks: TrackList,
    pub blocks: TrackList,
}

pub(super) struct Waveform {
    pub id: u16,
    pub port: u16,
    pub streaming: u8,
}

#[derive(Default)]
struct WaveNames {
    rows: HashSet<usize>,
    names: Vec<String>,
}

struct Walker<'a> {
    tables: &'a CueTables,
    is_memory: bool,
    port: u16,
    synth_depth: u32,
    sequence_depth: u32,
    row: usize,
    name: Option<&'a str>,
    found: HashMap<u16, WaveNames>,
}

/// Cue names for each wave id, or `None` when the walk fails: no stream in\n/// the bank is named then.
pub(super) fn walk(tables: &CueTables, is_memory: bool, port: u16) -> Option<HashMap<u16, String>> {
    let mut walker = Walker {
        tables,
        is_memory,
        port,
        synth_depth: 0,
        sequence_depth: 0,
        row: 0,
        name: None,
        found: HashMap::new(),
    };
    for (row, cue_name) in tables.cue_names.iter().enumerate() {
        walker.row = row;
        walker.name = cue_name.name.as_deref();
        walker.cue(cue_name.cue_index)?;
    }
    Some(
        walker
            .found
            .into_iter()
            .map(|(id, wave)| (id, wave.names.join("; ")))
            .collect(),
    )
}

impl Walker<'_> {
    fn cue(&mut self, index: u16) -> Option<()> {
        let cue = self.tables.cues.get(index as usize)?;
        match cue.reference_type {
            1 => self.waveform(cue.reference_index),
            2 => self.synth(cue.reference_index),
            3 => self.sequence(cue.reference_index),
            8 => self.block_sequence(cue.reference_index),
            _ => Some(()),
        }
    }

    fn waveform(&mut self, index: u16) -> Option<()> {
        let wave = self.tables.waveforms.get(index as usize)?;
        if wave.port != ANY_PORT && wave.port != self.port {
            return Some(());
        }
        if (self.is_memory && wave.streaming == 1) || (!self.is_memory && wave.streaming == 0) {
            return Some(());
        }
        let Some(name) = self.name else {
            return Some(());
        };
        let entry = self.found.entry(wave.id).or_default();
        if entry.rows.insert(self.row) {
            entry.names.push(if wave.streaming == 2 && self.is_memory {
                format!("{name} [pre]")
            } else {
                name.to_owned()
            });
        }
        Some(())
    }

    fn synth(&mut self, index: u16) -> Option<()> {
        let items = self.tables.synths.get(index as usize)?;
        self.synth_depth += 1;
        if self.synth_depth > MAX_DEPTH {
            return None;
        }
        for [kind_high, kind_low, index_high, index_low] in items.as_chunks::<4>().0 {
            let item_index = u16::from_be_bytes([*index_high, *index_low]);
            match u16::from_be_bytes([*kind_high, *kind_low]) {
                1 => self.waveform(item_index)?,
                2 => self.synth(item_index)?,
                3 => self.sequence(item_index)?,
                _ => break,
            }
        }
        self.synth_depth -= 1;
        Some(())
    }

    fn sequence(&mut self, index: u16) -> Option<()> {
        let sequence = self.tables.sequences.get(index as usize)?;
        self.sequence_depth += 1;
        if self.sequence_depth > MAX_DEPTH {
            return None;
        }
        self.track_list(sequence)?;
        self.sequence_depth -= 1;
        Some(())
    }

    fn block_sequence(&mut self, index: u16) -> Option<()> {
        let sequence = self.tables.block_sequences.get(index as usize)?;
        self.track_list(&sequence.tracks)?;
        check_length(&sequence.blocks)?;
        for block in list_indexes(&sequence.blocks) {
            self.track_list(self.tables.blocks.get(block as usize)?)?;
        }
        Some(())
    }

    fn track_list(&mut self, list: &TrackList) -> Option<()> {
        check_length(list)?;
        for track in list_indexes(list) {
            self.track(track)?;
        }
        Some(())
    }

    fn track(&mut self, index: u16) -> Option<()> {
        let event = *self.tables.tracks.get(index as usize)?;
        if event == NO_EVENT {
            return Some(());
        }
        let command = self.tables.commands.get(event as usize)?;
        self.command(command)
    }

    fn command(&mut self, command: &[u8]) -> Option<()> {
        let mut position = 0;
        while let Some(head) = command.get(position..position + 3) {
            let code = u16::from_be_bytes([head[0], head[1]]);
            let size = head[2] as usize;
            position += 3;
            if matches!(code, 2000 | 2003)
                && size >= 4
                && let Some(reference) = command.get(position..position + 4)
            {
                let index = u16::from_be_bytes([reference[2], reference[3]]);
                match u16::from_be_bytes([reference[0], reference[1]]) {
                    2 => self.synth(index)?,
                    3 => self.sequence(index)?,
                    _ => break,
                }
            }
            position += size;
        }
        Some(())
    }
}

fn check_length(list: &TrackList) -> Option<()> {
    (list.count as usize * 2 <= list.indexes.len()).then_some(())
}

fn list_indexes(list: &TrackList) -> impl Iterator<Item = u16> + '_ {
    list.indexes
        .as_chunks::<2>()
        .0
        .iter()
        .take(list.count as usize)
        .map(|item| u16::from_be_bytes(*item))
}

/// Reads the walk's tables out of an ACB `Header` table. A missing or broken
/// table reads as empty, so it fails the walk only if something refers to it.
pub(super) fn read_tables(header: &UtfTable, is_memory: bool) -> CueTables {
    let table = |name| subtable(header, name);
    let mut tables = CueTables::default();
    if let Some(t) = table("CueNameTable") {
        tables.cue_names = rows(&t)
            .map(|row| CueName {
                cue_index: number(u16_at(&t, row, "CueIndex")),
                name: string_at(&t, row, "CueName"),
            })
            .collect();
    }
    if let Some(t) = table("CueTable") {
        tables.cues = rows(&t)
            .map(|row| Cue {
                reference_type: number(u8_at(&t, row, "ReferenceType")),
                reference_index: number(u16_at(&t, row, "ReferenceIndex")),
            })
            .collect();
    }
    if let Some(t) = table("BlockSequenceTable") {
        tables.block_sequences = rows(&t)
            .map(|row| BlockSequence {
                tracks: track_list(&t, row, "NumTracks", "TrackIndex"),
                blocks: track_list(&t, row, "NumBlocks", "BlockIndex"),
            })
            .collect();
    }
    if let Some(t) = table("BlockTable") {
        tables.blocks = rows(&t)
            .map(|row| track_list(&t, row, "NumTracks", "TrackIndex"))
            .collect();
    }
    if let Some(t) = table("SequenceTable") {
        tables.sequences = rows(&t)
            .map(|row| track_list(&t, row, "NumTracks", "TrackIndex"))
            .collect();
    }
    if let Some(t) = table("TrackTable") {
        tables.tracks = rows(&t)
            .map(|row| number(u16_at(&t, row, "EventIndex")))
            .collect();
    }
    // TrackEvent exists from ACB 1.28; older files have Command instead.
    if let Some(t) = table("TrackEventTable").or_else(|| table("CommandTable")) {
        tables.commands = rows(&t).map(|row| data_at(&t, row, "Command")).collect();
    }
    if let Some(t) = table("SynthTable") {
        tables.synths = rows(&t)
            .map(|row| data_at(&t, row, "ReferenceItems"))
            .collect();
    }
    if let Some(t) = table("WaveformTable") {
        tables.waveforms = rows(&t).map(|row| waveform(&t, row, is_memory)).collect();
    }
    tables
}

fn waveform(table: &UtfTable, row: usize, is_memory: bool) -> Waveform {
    let (id, port) = match u16_at(table, row, "Id") {
        Some(id) => (id, ANY_PORT),
        None if is_memory => (number(u16_at(table, row, "MemoryAwbId")), ANY_PORT),
        None => (
            number(u16_at(table, row, "StreamAwbId")),
            number(u16_at(table, row, "StreamAwbPortNo")),
        ),
    };
    Waveform {
        id,
        port,
        streaming: number(u8_at(table, row, "Streaming")),
    }
}

fn track_list(table: &UtfTable, row: usize, count: &str, indexes: &str) -> TrackList {
    TrackList {
        count: number(u16_at(table, row, count)),
        indexes: data_at(table, row, indexes),
    }
}

fn subtable(header: &UtfTable, name: &str) -> Option<UtfTable> {
    let bytes = header.rows.first()?.get(name)?.as_bytes()?;
    if bytes.is_empty() {
        return None;
    }
    UtfTable::new(Cursor::new(bytes)).ok()
}

fn rows(table: &UtfTable) -> std::ops::Range<usize> {
    0..table.rows.len()
}

/// An absent column reads as zero, like a zero-initialized row.
fn number<T: Default>(value: Option<T>) -> T {
    value.unwrap_or_default()
}

fn u8_at(table: &UtfTable, row: usize, key: &str) -> Option<u8> {
    match table.rows[row].get(key) {
        Some(Value::U8(value)) => Some(*value),
        _ => None,
    }
}

fn u16_at(table: &UtfTable, row: usize, key: &str) -> Option<u16> {
    match table.rows[row].get(key) {
        Some(Value::U16(value)) => Some(*value),
        _ => None,
    }
}

fn string_at(table: &UtfTable, row: usize, key: &str) -> Option<String> {
    table.rows[row].get(key)?.as_string().map(str::to_owned)
}

fn data_at(table: &UtfTable, row: usize, key: &str) -> Vec<u8> {
    table.rows[row]
        .get(key)
        .and_then(Value::as_bytes)
        .map(<[u8]>::to_vec)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name(cue_index: u16, name: &str) -> CueName {
        CueName {
            cue_index,
            name: Some(name.to_owned()),
        }
    }

    fn cue(reference_type: u8, reference_index: u16) -> Cue {
        Cue {
            reference_type,
            reference_index,
        }
    }

    fn wave(id: u16, port: u16, streaming: u8) -> Waveform {
        Waveform {
            id,
            port,
            streaming,
        }
    }

    fn direct(waveforms: Vec<Waveform>, names: &[&str]) -> CueTables {
        CueTables {
            cue_names: (0u16..).zip(names).map(|(i, n)| name(i, n)).collect(),
            cues: (0..names.len() as u16).map(|i| cue(1, i)).collect(),
            waveforms,
            ..Default::default()
        }
    }

    fn item(kind: u16, index: u16) -> Vec<u8> {
        [kind.to_be_bytes(), index.to_be_bytes()].concat()
    }

    #[test]
    fn joins_the_names_of_a_shared_wave() {
        let tables = direct(vec![wave(7, 0, 1), wave(7, 0, 1)], &["a", "b"]);
        assert_eq!(walk(&tables, false, 0).unwrap()[&7], "a; b");
    }

    #[test]
    fn marks_prefetch_names_in_memory_banks_only() {
        let tables = direct(vec![wave(1, ANY_PORT, 2)], &["a"]);
        assert_eq!(walk(&tables, true, 0).unwrap()[&1], "a [pre]");
        assert_eq!(walk(&tables, false, 0).unwrap()[&1], "a");
    }

    #[test]
    fn lists_one_cue_once_when_it_reaches_a_wave_twice() {
        let tables = CueTables {
            cue_names: vec![name(0, "a")],
            cues: vec![cue(2, 0)],
            synths: vec![[item(1, 0), item(1, 0)].concat()],
            waveforms: vec![wave(3, 0, 1)],
            ..Default::default()
        };
        assert_eq!(walk(&tables, false, 0).unwrap()[&3], "a");
    }

    #[test]
    fn repeated_name_rows_count_separately() {
        let tables = direct(vec![wave(1, 0, 1), wave(1, 0, 1)], &["a", "a"]);
        assert_eq!(walk(&tables, false, 0).unwrap()[&1], "a; a");
    }

    #[test]
    fn filters_by_port() {
        let tables = direct(
            vec![wave(1, 1, 1), wave(2, 0, 1), wave(3, ANY_PORT, 1)],
            &["other", "ours", "any"],
        );
        let names = walk(&tables, false, 0).unwrap();
        assert!(!names.contains_key(&1));
        assert_eq!((names[&2].as_str(), names[&3].as_str()), ("ours", "any"));
    }

    #[test]
    fn filters_by_streaming_kind() {
        let tables = direct(
            vec![
                wave(1, ANY_PORT, 0),
                wave(2, ANY_PORT, 1),
                wave(3, ANY_PORT, 2),
            ],
            &["memory", "stream", "both"],
        );
        let memory = walk(&tables, true, 0).unwrap();
        assert_eq!(memory.len(), 2);
        assert!(memory.contains_key(&1) && memory.contains_key(&3));
        let stream = walk(&tables, false, 0).unwrap();
        assert_eq!(stream.len(), 2);
        assert!(stream.contains_key(&2) && stream.contains_key(&3));
    }

    #[test]
    fn follows_sequence_track_command_synth_to_the_wave() {
        let mut command = vec![0, 33, 1, 9];
        command.extend(2000u16.to_be_bytes());
        command.push(4);
        command.extend(item(2, 0));
        let tables = CueTables {
            cue_names: vec![name(0, "a")],
            cues: vec![cue(3, 0)],
            sequences: vec![TrackList {
                count: 2,
                indexes: vec![0, 0, 0, 1],
            }],
            tracks: vec![NO_EVENT, 0],
            commands: vec![command],
            synths: vec![item(1, 0)],
            waveforms: vec![wave(5, 0, 1)],
            ..Default::default()
        };
        assert_eq!(walk(&tables, false, 0).unwrap()[&5], "a");
    }

    #[test]
    fn follows_block_sequences_through_tracks_and_blocks() {
        let mut command = 2003u16.to_be_bytes().to_vec();
        command.push(4);
        command.extend(item(2, 0));
        let tables = CueTables {
            cue_names: vec![name(0, "a")],
            cues: vec![cue(8, 0)],
            block_sequences: vec![BlockSequence {
                tracks: TrackList::default(),
                blocks: TrackList {
                    count: 1,
                    indexes: vec![0, 0],
                },
            }],
            blocks: vec![TrackList {
                count: 1,
                indexes: vec![0, 0],
            }],
            tracks: vec![0],
            commands: vec![command],
            synths: vec![item(1, 0)],
            waveforms: vec![wave(4, 0, 1)],
            ..Default::default()
        };
        assert_eq!(walk(&tables, false, 0).unwrap()[&4], "a");
    }

    #[test]
    fn synth_item_type_zero_or_unknown_ends_the_synth() {
        for stop in [0, 6] {
            let tables = CueTables {
                cue_names: vec![name(0, "a")],
                cues: vec![cue(2, 0)],
                synths: vec![[item(1, 0), item(stop, 0), item(1, 99)].concat()],
                waveforms: vec![wave(1, 0, 1)],
                ..Default::default()
            };
            assert_eq!(walk(&tables, false, 0).unwrap()[&1], "a");
        }
    }

    #[test]
    fn unknown_command_reference_ends_the_command() {
        let mut command = 2000u16.to_be_bytes().to_vec();
        command.push(4);
        command.extend(item(9, 0));
        command.extend(2000u16.to_be_bytes());
        command.push(4);
        command.extend(item(2, 99));
        let tables = CueTables {
            cue_names: vec![name(0, "a")],
            cues: vec![cue(3, 0)],
            sequences: vec![TrackList {
                count: 1,
                indexes: vec![0, 0],
            }],
            tracks: vec![0],
            commands: vec![command],
            ..Default::default()
        };
        assert!(walk(&tables, false, 0).unwrap().is_empty());
    }

    #[test]
    fn short_command_references_are_skipped() {
        let mut command = 2000u16.to_be_bytes().to_vec();
        command.extend([2, 0, 2]);
        command.extend(2000u16.to_be_bytes());
        command.push(4);
        command.extend(item(2, 0));
        let tables = CueTables {
            cue_names: vec![name(0, "a")],
            cues: vec![cue(3, 0)],
            sequences: vec![TrackList {
                count: 1,
                indexes: vec![0, 0],
            }],
            tracks: vec![0],
            commands: vec![command],
            synths: vec![item(1, 0)],
            waveforms: vec![wave(8, 0, 1)],
            ..Default::default()
        };
        assert_eq!(walk(&tables, false, 0).unwrap()[&8], "a");
    }

    #[test]
    fn ignores_other_cue_reference_types() {
        let tables = CueTables {
            cue_names: vec![name(0, "a")],
            cues: vec![cue(5, 99)],
            ..Default::default()
        };
        assert!(walk(&tables, false, 0).unwrap().is_empty());
    }

    #[test]
    fn synth_depth_over_three_fails_the_whole_bank() {
        let chain = |depth: u16| CueTables {
            cue_names: vec![name(0, "a")],
            cues: vec![cue(2, 0)],
            synths: (0..depth)
                .map(|i| {
                    if i + 1 == depth {
                        item(1, 0)
                    } else {
                        item(2, i + 1)
                    }
                })
                .collect(),
            waveforms: vec![wave(1, 0, 1)],
            ..Default::default()
        };
        assert!(walk(&chain(3), false, 0).is_some());
        assert!(walk(&chain(4), false, 0).is_none());
    }

    #[test]
    fn sequence_depth_over_three_fails_the_whole_bank() {
        // Sequence i plays track i, whose command starts sequence i + 1.
        let chain = |depth: u16| CueTables {
            cue_names: vec![name(0, "a")],
            cues: vec![cue(3, 0)],
            sequences: (0..depth)
                .map(|i| TrackList {
                    count: u16::from(i + 1 < depth),
                    indexes: i.to_be_bytes().to_vec(),
                })
                .collect(),
            tracks: (0..depth).collect(),
            commands: (0..depth)
                .map(|i| [vec![0x07, 0xD0, 4], item(3, i + 1)].concat())
                .collect(),
            ..Default::default()
        };
        assert!(walk(&chain(3), false, 0).is_some());
        assert!(walk(&chain(4), false, 0).is_none());
    }

    #[test]
    fn bad_indexes_fail_the_whole_bank() {
        let mut bad_cue = direct(vec![wave(1, 0, 1)], &["good"]);
        bad_cue.cue_names.push(name(5, "bad"));
        assert!(walk(&bad_cue, false, 0).is_none());

        let bad_wave = CueTables {
            cue_names: vec![name(0, "a")],
            cues: vec![cue(1, 3)],
            ..Default::default()
        };
        assert!(walk(&bad_wave, false, 0).is_none());
    }

    #[test]
    fn short_track_index_fails_the_whole_bank() {
        let tables = CueTables {
            cue_names: vec![name(0, "a")],
            cues: vec![cue(3, 0)],
            sequences: vec![TrackList {
                count: 2,
                indexes: vec![0, 0],
            }],
            tracks: vec![NO_EVENT],
            ..Default::default()
        };
        assert!(walk(&tables, false, 0).is_none());
    }

    #[test]
    fn unnamed_rows_add_nothing() {
        let tables = CueTables {
            cue_names: vec![CueName {
                cue_index: 0,
                name: None,
            }],
            cues: vec![cue(1, 0)],
            waveforms: vec![wave(1, 0, 1)],
            ..Default::default()
        };
        assert!(walk(&tables, false, 0).unwrap().is_empty());
    }
}
