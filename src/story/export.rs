//! Writes `story.json` / `dialogue.csv` and optionally links story voices to
//! WAV files produced by the existing audio extractor.

use super::{
    Selection, StoryIndex,
    index::ScanStats,
    link::{ControlRow, Line, Linked, Unresolved, Voice},
};
use crate::{
    catalog::{Category, Language, SourceInfo},
    discovery::validate_directories,
    extraction::{ExtractOptions, existing_output, extract_source},
    roster::speakers::{COMMANDER_SPEAKERS, PLAYER_PLACEHOLDERS, is_player_placeholder},
};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

/// One progress line of a story export.
pub struct LogLine {
    pub text: String,
    /// A pack was missing or failed; the export goes on without it.
    pub problem: bool,
}

impl LogLine {
    fn step(text: String) -> Self {
        Self {
            text,
            problem: false,
        }
    }

    fn problem(text: String) -> Self {
        Self {
            text,
            problem: true,
        }
    }
}

pub struct ExportOptions {
    pub output: PathBuf,
    pub with_audio: bool,
    pub by_speaker: bool,
    pub cancel: Arc<AtomicBool>,
}

#[derive(Serialize)]
struct TableCounts {
    texts: usize,
    content: usize,
    audio: usize,
}

#[derive(Serialize)]
struct MissingAudio<'a> {
    script: &'a str,
    row: i64,
    #[serde(flatten)]
    voice: &'a Voice,
}

#[derive(Serialize)]
struct SelectionInfo {
    kind: &'static str,
    value: String,
}

#[derive(Serialize)]
struct StoryFile<'a> {
    format_version: u32,
    text_locale: &'static str,
    order: &'static str,
    /// The most common player-name placeholder; kept for existing readers.
    player_name_placeholder: &'static str,
    /// Every player-name placeholder found in speakers and text.
    player_name_placeholders: &'static [&'static str],
    lines: &'a [Line],
    control_rows: &'a [ControlRow],
    unresolved: &'a [Unresolved],
    table_counts: TableCounts,
    #[serde(skip_serializing_if = "Option::is_none")]
    missing_audio: Option<Vec<MissingAudio<'a>>>,
    scan: &'a ScanStats,
    scan_warnings: usize,
    source_data_root: String,
    audio_links_checked: bool,
    selection: SelectionInfo,
    generator: String,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct ExportSummary {
    pub selection: String,
    pub directory: PathBuf,
    pub scripts: usize,
    pub lines: usize,
    pub voice_links: usize,
    pub unresolved: usize,
    /// `None` when audio was not checked.
    pub missing_audio: Option<usize>,
    pub linked_wavs: usize,
    pub distinct_wavs: usize,
    pub speaker_copies: usize,
    pub audio_failures: Vec<String>,
}

/// Folder for one selection's files: `<output>/Story/<selection>`.
pub fn selection_directory(output: &Path, selection: &Selection) -> PathBuf {
    output.join("Story").join(safe_name(&selection.label()))
}

pub fn safe_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .take(80)
        .map(|c| {
            if c.is_control() || "<>:\"/\\|?*".contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect();
    let cleaned = cleaned.trim_matches([' ', '.']);
    if cleaned.is_empty() {
        "_".into()
    } else {
        cleaned.into()
    }
}

pub fn export(
    index: &StoryIndex,
    selection: &Selection,
    options: &ExportOptions,
    sources: Option<&[SourceInfo]>,
    log: &dyn Fn(LogLine),
) -> Result<(ExportSummary, Linked)> {
    export_filtered(index, selection, None, options, sources, log)
}

/// Keeps one linked line in a filtered export.
pub type LineFilter<'a> = &'a (dyn Fn(&Line) -> bool + Sync);

/// Like [`export`], but with `keep` only the matching text lines are
/// written (and only their voices are linked and copied); control rows and
/// unresolved references are kept only for the rows of those lines.
/// Without `keep` the output is exactly that of [`export`].
pub fn export_filtered(
    index: &StoryIndex,
    selection: &Selection,
    keep: Option<LineFilter>,
    options: &ExportOptions,
    sources: Option<&[SourceInfo]>,
    log: &dyn Fn(LogLine),
) -> Result<(ExportSummary, Linked)> {
    validate_directories(&index.game_root, &options.output)?;
    let label = selection.label();
    ensure!(
        !["cn", "jp", "common"].contains(&label.to_ascii_lowercase().as_str()),
        tr!(
            "选择名不能是 CN、JP 或 Common，以免与音频目录冲突",
            "The selection cannot be named CN, JP or Common, which would clash with the audio folders"
        )
    );
    let mut linked = index.link(selection);
    if let Some(keep) = keep {
        retain_lines(&mut linked, keep);
    }
    ensure!(
        !linked.lines.is_empty(),
        tr!("所选范围没有 Text 行", "The selection has no Text rows")
    );
    let directory = selection_directory(&options.output, selection);
    fs::create_dir_all(&directory)?;
    let mut summary = ExportSummary {
        selection: label.clone(),
        directory: directory.clone(),
        scripts: linked
            .lines
            .iter()
            .map(|line| &line.identity.script)
            .collect::<BTreeSet<_>>()
            .len(),
        lines: linked.lines.len(),
        voice_links: linked.lines.iter().map(|line| line.voices.len()).sum(),
        unresolved: linked.unresolved.len(),
        ..ExportSummary::default()
    };
    if options.with_audio {
        let sources = sources.context(tr!(
            "缺少音频资源扫描结果",
            "The audio scan results are missing"
        ))?;
        let character = matches!(selection, Selection::Character { .. });
        // Character exports decode into a temporary folder removed on every
        // exit path; only the WAVs of the kept lines are copied out.
        let temporary = character
            .then(|| TemporaryDirectory::create(&options.output))
            .transpose()?;
        summary.audio_failures = attach_audio(
            &mut linked,
            sources,
            options,
            temporary.as_ref().map(|directory| directory.0.as_path()),
            log,
        )?;
        if options.cancel.load(Ordering::Relaxed) {
            bail!(tr!("导出已停止", "The export was stopped"));
        }
        if character {
            copy_character_voices(&mut linked, &directory)?;
        }
        let wavs: Vec<&str> = linked
            .lines
            .iter()
            .flat_map(|line| &line.voices)
            .filter_map(|voice| voice.wav.as_deref())
            .collect();
        summary.linked_wavs = wavs.len();
        summary.distinct_wavs = wavs.iter().collect::<BTreeSet<_>>().len();
        if options.by_speaker && !character {
            summary.speaker_copies = copy_by_speaker(&linked, &options.output)?;
        }
    }
    let missing: Option<Vec<MissingAudio>> = options.with_audio.then(|| {
        linked
            .lines
            .iter()
            .flat_map(|line| {
                line.voices
                    .iter()
                    .filter(|voice| voice.wav.is_none())
                    .map(|voice| MissingAudio {
                        script: &line.identity.script,
                        row: line.identity.row,
                        voice,
                    })
            })
            .collect()
    });
    summary.missing_audio = missing.as_ref().map(Vec::len);
    let story = StoryFile {
        format_version: 1,
        text_locale: "CN",
        order: "source sheet row order; branches not evaluated",
        player_name_placeholder: PLAYER_PLACEHOLDERS[0],
        player_name_placeholders: &PLAYER_PLACEHOLDERS,
        lines: &linked.lines,
        control_rows: &linked.control_rows,
        unresolved: &linked.unresolved,
        table_counts: TableCounts {
            texts: index.tables.texts.len(),
            content: index.tables.content.len(),
            audio: index.tables.audio.len(),
        },
        missing_audio: missing,
        scan: &index.scan,
        scan_warnings: index.summary.scan_warnings,
        source_data_root: index.data_root.display().to_string(),
        audio_links_checked: options.with_audio,
        selection: SelectionInfo {
            kind: match selection {
                Selection::Script(_) => "script",
                Selection::Chapter(_) => "chapter",
                Selection::Character { .. } => "character",
            },
            value: match selection {
                Selection::Script(book) | Selection::Chapter(book) => book.clone(),
                Selection::Character { name, .. } => name.clone(),
            },
        },
        generator: format!("GF2 Echo Archive {}", env!("CARGO_PKG_VERSION")),
    };
    write_atomic(
        &directory.join("story.json"),
        &serde_json::to_vec_pretty(&story)?,
    )?;
    write_atomic(
        &directory.join("dialogue.csv"),
        &dialogue_csv(&linked.lines),
    )?;
    drop(story);
    Ok((summary, linked))
}

/// Drops text lines `keep` rejects, and the control rows and unresolved
/// references that do not sit on a kept line's row.
fn retain_lines(linked: &mut Linked, keep: LineFilter) {
    linked.lines.retain(|line| keep(line));
    let rows: BTreeSet<(&str, i64)> = linked
        .lines
        .iter()
        .map(|line| (line.identity.script.as_str(), line.identity.row))
        .collect();
    let on_kept_row =
        |identity: &super::link::Identity| rows.contains(&(identity.script.as_str(), identity.row));
    linked.control_rows.retain(|row| on_kept_row(&row.identity));
    linked
        .unresolved
        .retain(|entry| on_kept_row(&entry.identity));
}

/// Decode folder of a character export; removed when dropped.
struct TemporaryDirectory(PathBuf);

impl TemporaryDirectory {
    /// Created inside the output folder, on the same volume as the result.
    fn create(output: &Path) -> Result<Self> {
        let nanos = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let path = output.join(format!(".echo-voice-{}-{nanos}", std::process::id()));
        fs::create_dir_all(&path).with_context(|| {
            tr!(
                "无法创建临时目录 {}",
                "Cannot create the temporary folder {}",
                path.display()
            )
        })?;
        Ok(Self(path))
    }
}

impl Drop for TemporaryDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn write_atomic(path: &Path, data: &[u8]) -> Result<()> {
    let temporary = path.with_extension("partial");
    fs::write(&temporary, data)?;
    fs::rename(&temporary, path)?;
    Ok(())
}

fn csv_field(output: &mut String, value: &str) {
    if value.contains([',', '"', '\r', '\n']) {
        output.push('"');
        output.push_str(&value.replace('"', "\"\""));
        output.push('"');
    } else {
        output.push_str(value);
    }
}

/// UTF-8 with BOM and CRLF rows, matching Python's `csv.writer`.
pub fn dialogue_csv(lines: &[Line]) -> Vec<u8> {
    let mut output = String::from("\u{feff}");
    output.push_str(
        "script,row,speaker,text,speaker_alt,text_alt,CN_default,CN_alternate,JP_default,JP_alternate\r\n",
    );
    for line in lines {
        let mut references: HashMap<(&str, &str), &str> = HashMap::new();
        for voice in &line.voices {
            if let Some(value) = voice.wav.as_deref().or(voice.cue.as_deref()) {
                references.insert((voice.language, voice.variant), value);
            } else {
                references.remove(&(voice.language, voice.variant));
            }
        }
        let row = line.identity.row.to_string();
        let mut fields: Vec<&str> = vec![
            &line.identity.script,
            &row,
            line.speaker.as_deref().unwrap_or(""),
            line.text.as_deref().unwrap_or(""),
            line.speaker_alt.as_deref().unwrap_or(""),
            line.text_alt.as_deref().unwrap_or(""),
        ];
        for language in ["CN", "JP"] {
            for variant in ["default", "alternate"] {
                fields.push(references.get(&(language, variant)).copied().unwrap_or(""));
            }
        }
        for (index, field) in fields.iter().enumerate() {
            if index > 0 {
                output.push(',');
            }
            csv_field(&mut output, field);
        }
        output.push_str("\r\n");
    }
    output.into_bytes()
}

#[derive(Deserialize)]
struct AudioIndex {
    files: Vec<AudioIndexFile>,
}

#[derive(Deserialize)]
struct AudioIndexFile {
    name: String,
    bytes: u64,
    cue_name: String,
    sample_rate: u32,
    channels: u32,
    samples: u64,
    decoder_warning: bool,
}

fn language_of(name: &str) -> Option<Language> {
    match name {
        "CN" => Some(Language::Cn),
        "JP" => Some(Language::Jp),
        _ => None,
    }
}

/// Exports the required Story banks in-process and links WAVs by full cue
/// name. A cue matching several files is left unlinked. With `decode_into`,
/// banks already complete under the output folder are reused and the others
/// are decoded there instead of under the output folder.
fn attach_audio(
    linked: &mut Linked,
    sources: &[SourceInfo],
    options: &ExportOptions,
    decode_into: Option<&Path>,
    log: &dyn Fn(LogLine),
) -> Result<Vec<String>> {
    let required: BTreeSet<(String, &'static str)> = linked
        .lines
        .iter()
        .flat_map(|line| &line.voices)
        .filter_map(|voice| voice.bank.clone().map(|bank| (bank, voice.language)))
        .collect();
    let mut failures = Vec::new();
    type Match = (PathBuf, u64, AudioIndexFile);
    let mut lookup: HashMap<(String, &'static str, String), Vec<Match>> = HashMap::new();
    for (position, (bank, language)) in required.iter().enumerate() {
        if options.cancel.load(Ordering::Relaxed) {
            break;
        }
        let wanted = language_of(language).expect("CN or JP");
        let matches: Vec<SourceInfo> = sources
            .iter()
            .filter(|source| {
                source.category == Category::Story
                    && source.language == wanted
                    && source.key.eq_ignore_ascii_case(bank)
            })
            .cloned()
            .collect();
        if matches.is_empty() {
            let message = tr!(
                "[剧情语音] 没有已下载的音频包 {} / {}",
                "[Story audio] No downloaded audio pack {} / {}",
                bank,
                language
            );
            log(LogLine::problem(message.clone()));
            failures.push(message);
            continue;
        }
        let extract = ExtractOptions {
            output_directory: options.output.clone(),
            sources: matches,
            character_name: None,
            overwrite: false,
            cancel: options.cancel.clone(),
        };
        for source in &extract.sources {
            log(LogLine::step(tr!(
                "[剧情语音 {}/{}] {} / {} / {}",
                "[Story audio {}/{}] {} / {} / {}",
                position + 1,
                required.len(),
                bank,
                language,
                source.kind.label()
            )));
            let reusable = decode_into.and_then(|_| existing_output(source, &extract));
            let decoded = match (reusable, decode_into) {
                (Some(result), _) => Ok(result),
                (None, Some(directory)) => extract_source(
                    source,
                    &ExtractOptions {
                        output_directory: directory.to_path_buf(),
                        sources: Vec::new(),
                        character_name: None,
                        overwrite: false,
                        cancel: options.cancel.clone(),
                    },
                ),
                (None, None) => extract_source(source, &extract),
            };
            let result = match decoded {
                Ok(result) => result,
                Err(error) => {
                    let message = tr!(
                        "[剧情语音失败] {}：{:#}",
                        "[Story audio failed] {}: {:#}",
                        source.path.display(),
                        error
                    );
                    log(LogLine::problem(message.clone()));
                    failures.push(message);
                    continue;
                }
            };
            let index_path = result.directory.join("audio-index.json");
            let index: AudioIndex =
                serde_json::from_slice(&fs::read(&index_path).with_context(|| {
                    tr!("无法读取 {}", "Cannot read {}", index_path.display())
                })?)?;
            for item in index.files {
                let path = result.directory.join(&item.name);
                for cue in item.cue_name.split(';') {
                    lookup
                        .entry((bank.clone(), language, cue.trim().to_owned()))
                        .or_default()
                        .push((path.clone(), item.bytes, clone_item(&item)));
                }
            }
        }
    }
    for line in &mut linked.lines {
        for voice in &mut line.voices {
            let (Some(bank), Some(cue)) = (&voice.bank, &voice.cue) else {
                continue;
            };
            let key = (bank.clone(), voice.language, cue.clone());
            let Some([(path, bytes, item)]) = lookup.get(&key).map(Vec::as_slice) else {
                continue;
            };
            if fs::metadata(path).is_ok_and(|m| m.is_file() && m.len() == *bytes) {
                voice.wav = Some(std::path::absolute(path)?.display().to_string());
                voice.decoder_warning = Some(item.decoder_warning);
                voice.sample_rate = Some(item.sample_rate);
                voice.channels = Some(item.channels);
                voice.samples = Some(item.samples);
            }
        }
    }
    Ok(failures)
}

fn clone_item(item: &AudioIndexFile) -> AudioIndexFile {
    AudioIndexFile {
        name: item.name.clone(),
        bytes: item.bytes,
        cue_name: item.cue_name.clone(),
        sample_rate: item.sample_rate,
        channels: item.channels,
        samples: item.samples,
        decoder_warning: item.decoder_warning,
    }
}

/// `voice/<language>/<script>_<row>_<cue>.wav` under a character's folder.
fn character_voice_path(
    directory: &Path,
    script: &str,
    row: i64,
    language: &str,
    cue: &str,
) -> PathBuf {
    let script = script.strip_suffix(".book").unwrap_or(script);
    directory.join("voice").join(language).join(format!(
        "{}_{row}_{}.wav",
        safe_name(script),
        safe_name(cue)
    ))
}

/// Copies the WAVs of the kept lines into the character's `voice` folder,
/// repoints the lines at the copies and removes WAVs left there by earlier
/// exports of a different scope.
fn copy_character_voices(linked: &mut Linked, directory: &Path) -> Result<()> {
    let mut kept: BTreeSet<PathBuf> = BTreeSet::new();
    for line in &mut linked.lines {
        let (script, row) = (line.identity.script.clone(), line.identity.row);
        for voice in &mut line.voices {
            let (Some(wav), Some(cue)) = (&voice.wav, &voice.cue) else {
                continue;
            };
            let target = character_voice_path(directory, &script, row, voice.language, cue);
            if kept.insert(target.clone()) {
                if let Some(parent) = target.parent() {
                    fs::create_dir_all(parent)?;
                }
                fs::copy(wav, &target)
                    .with_context(|| tr!("无法复制到 {}", "Cannot copy to {}", target.display()))?;
            }
            voice.wav = Some(std::path::absolute(&target)?.display().to_string());
        }
    }
    for language in ["CN", "JP"] {
        let Ok(entries) = fs::read_dir(directory.join("voice").join(language)) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "wav") && !kept.contains(&path) {
                fs::remove_file(&path)?;
            }
        }
    }
    Ok(())
}

/// Folder name for one speaker under `by-speaker/`. The player-name placeholder
/// becomes the commander, as in the UI; the data files keep the placeholder.
fn speaker_folder(speaker: Option<&str>) -> String {
    match speaker {
        Some(name) if is_player_placeholder(name) => COMMANDER_SPEAKERS[0].into(),
        Some(name) => safe_name(name),
        None => "未知说话人".into(),
    }
}

/// Copies linked WAVs to `<output>/Story/<script>/by-speaker/<speaker>/`.
/// The language is part of the file name because CN and JP share cue names.
fn copy_by_speaker(linked: &Linked, output: &Path) -> Result<usize> {
    let mut targets: BTreeMap<PathBuf, PathBuf> = BTreeMap::new();
    for line in &linked.lines {
        let script = line
            .identity
            .script
            .strip_suffix(".book")
            .unwrap_or(&line.identity.script);
        let speaker = speaker_folder(line.speaker.as_deref());
        for voice in &line.voices {
            let (Some(wav), Some(cue)) = (&voice.wav, &voice.cue) else {
                continue;
            };
            let target = output
                .join("Story")
                .join(safe_name(script))
                .join("by-speaker")
                .join(&speaker)
                .join(format!(
                    "{:05}_{}_{}.wav",
                    line.identity.row,
                    safe_name(cue),
                    voice.language
                ));
            targets.insert(target, PathBuf::from(wav));
        }
    }
    for (target, source) in &targets {
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        let up_to_date = matches!(
            (fs::metadata(target), fs::metadata(source)),
            (Ok(a), Ok(b)) if a.len() == b.len()
        );
        if !up_to_date {
            fs::copy(source, target)
                .with_context(|| tr!("无法复制到 {}", "Cannot copy to {}", target.display()))?;
        }
    }
    Ok(targets.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::story::link::{Cells, Identity};

    #[test]
    fn csv_matches_python_quoting() {
        let line = Line {
            identity: Identity {
                script: "CN_X.book".into(),
                sheet: String::new(),
                row: 17,
                source_bundle: String::new(),
                source_offset: 0,
                source_path_id: 0,
            },
            sequence: 1,
            speaker_id: Some("165".into()),
            speaker: Some("未知对象".into()),
            speaker_alt: None,
            content_id: "1".into(),
            text: Some("他说：\"走,吧\"".into()),
            text_alt: Some("a\nb".into()),
            voices: vec![Voice {
                language: "JP",
                variant: "default",
                column: "Voice_JP",
                audio_id: "1".into(),
                bank: Some("B".into()),
                cue: Some("C".into()),
                wav: None,
                decoder_warning: None,
                sample_rate: None,
                channels: None,
                samples: None,
            }],
            raw_cells: Cells::default(),
        };
        let csv = String::from_utf8(dialogue_csv(&[line])).unwrap();
        assert!(csv.starts_with('\u{feff}'));
        assert!(csv.ends_with("CN_X.book,17,未知对象,\"他说：\"\"走,吧\"\"\",,\"a\nb\",,,C,\r\n"));
    }

    #[test]
    fn character_voices_are_named_by_script_row_and_cue() {
        assert_eq!(
            character_voice_path(Path::new("out"), "CN_M01013.book", 101, "JP", "VO/Main_01"),
            Path::new("out")
                .join("voice")
                .join("JP")
                .join("CN_M01013_101_VO_Main_01.wav")
        );
    }

    #[test]
    fn temporary_directory_is_removed_when_dropped() {
        let output = std::env::temp_dir().join(format!("echo-voice-test-{}", std::process::id()));
        fs::create_dir_all(&output).unwrap();
        let path = {
            let temporary = TemporaryDirectory::create(&output).unwrap();
            fs::write(temporary.0.join("a.wav"), b"x").unwrap();
            temporary.0.clone()
        };
        assert!(!path.exists());
        fs::remove_dir_all(output).unwrap();
    }

    #[test]
    fn safe_names_replace_reserved_characters() {
        assert_eq!(safe_name("a/b:c"), "a_b_c");
        assert_eq!(safe_name(" . "), "_");
        assert_eq!(safe_name("###"), "###");
    }

    #[test]
    fn player_placeholders_get_the_commander_folder() {
        assert_eq!(speaker_folder(Some("{}")), "指挥官");
        assert_eq!(speaker_folder(Some("###")), "指挥官");
        assert_eq!(speaker_folder(Some("可露凯")), "可露凯");
        assert_eq!(speaker_folder(Some("a/b")), "a_b");
        assert_eq!(speaker_folder(None), "未知说话人");
    }
}
