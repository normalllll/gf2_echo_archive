use crate::catalog::{Category, Language, SourceInfo, SourceKind};
use anyhow::{Context, Result, bail, ensure};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, hash_map::DefaultHasher},
    fs,
    hash::Hasher,
    io::Read,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::Sender,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub struct ExtractOptions {
    pub output_directory: PathBuf,
    pub sources: Vec<SourceInfo>,
    pub character_name: Option<String>,
    pub overwrite: bool,
    pub cancel: Arc<AtomicBool>,
}

pub enum WorkerMessage {
    Log(String),
    Progress {
        done: usize,
        total: usize,
    },
    Scanned(crate::catalog::ScanReport),
    /// One pack was written, or an existing output was reused.
    Decoded {
        reused: bool,
        wav_count: usize,
        directory: PathBuf,
    },
    /// Tracks of the current pack that need checking; the files are kept.
    DecoderWarning {
        tracks: usize,
    },
    DecodeFailed {
        path: PathBuf,
        error: String,
    },
    Finished(ExportTally),
}

/// Counts of one audio export; turned into text only for display.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ExportTally {
    pub stopped: bool,
    pub succeeded: usize,
    pub reused: usize,
    pub failed: usize,
    pub warning_tracks: usize,
    pub wavs: usize,
    pub by_category: Vec<(Category, Language, usize)>,
}

impl ExportTally {
    /// Adds up the batches of one task; the per-category counts stay in order.
    pub fn merge(parts: &[Self]) -> Self {
        let mut merged = Self::default();
        for part in parts {
            merged.stopped |= part.stopped;
            merged.succeeded += part.succeeded;
            merged.reused += part.reused;
            merged.failed += part.failed;
            merged.warning_tracks += part.warning_tracks;
            merged.wavs += part.wavs;
            merged.by_category.extend_from_slice(&part.by_category);
        }
        merged
    }

    /// The summary recorded in the log.
    pub fn log_text(&self) -> String {
        let heading = if self.stopped {
            tr!("已停止", "Stopped")
        } else {
            tr!("执行摘要", "Summary")
        };
        let mut text = tr!(
            "[{}] 成功={}；复用={}；失败={}；密钥警告音轨={}；共 {} 个 WAV",
            "[{}] Succeeded={}; Reused={}; Failed={}; Key-warning tracks={}; {} WAV in total",
            heading,
            self.succeeded,
            self.reused,
            self.failed,
            self.warning_tracks,
            self.wavs
        );
        for (category, language, count) in &self.by_category {
            text.push_str(&tr!(
                "\n  {} / {}：{} 个 WAV",
                "\n  {} / {}: {} WAV",
                category.label(),
                language.folder(),
                count
            ));
        }
        text
    }

    /// The one-line result shown on the task card.
    pub fn card_text(&self) -> String {
        use crate::ui::fmt_count;
        tr!(
            "成功 {} · 复用 {} · 失败 {} · 警告音轨 {} · {} 个 WAV",
            "Succeeded {} · Reused {} · Failed {} · Warning tracks {} · {} WAV",
            fmt_count(self.succeeded),
            fmt_count(self.reused),
            fmt_count(self.failed),
            fmt_count(self.warning_tracks),
            fmt_count(self.wavs)
        )
    }
}

pub fn run_extraction(options: ExtractOptions, sender: Sender<WorkerMessage>) {
    let emit = |message| {
        let _ = sender.send(message);
    };
    emit(WorkerMessage::Log(tr!(
        "[任务启动] 已选择 {} 个音频包；输出：{}",
        "[Started] {} audio packs selected; output: {}",
        options.sources.len(),
        options.output_directory.display()
    )));
    let mut succeeded = 0;
    let mut skipped = 0;
    let mut failed = 0;
    let mut total_wav = 0;
    let mut decoder_warnings = 0;
    let mut by_category = BTreeMap::new();
    for (index, source) in options.sources.iter().enumerate() {
        // Stop between complete banks; cancellation never publishes a partial bank.
        if options.cancel.load(Ordering::Relaxed) {
            break;
        }
        emit(WorkerMessage::Log(tr!(
            "[解码 {}/{}] {} / {} / {} / {}",
            "[Decoding {}/{}] {} / {} / {} / {}",
            index + 1,
            options.sources.len(),
            source.category.label(),
            source.language.folder(),
            source.kind.label(),
            source.key
        )));
        match extract_source(source, &options) {
            Ok(result) => {
                if result.reused {
                    skipped += 1;
                } else {
                    succeeded += 1;
                }
                total_wav += result.wav_count;
                decoder_warnings += result.decoder_warning_count;
                if result.decoder_warning_count > 0 {
                    emit(WorkerMessage::DecoderWarning {
                        tracks: result.decoder_warning_count,
                    });
                }
                *by_category
                    .entry((source.category, source.language))
                    .or_insert(0usize) += result.wav_count;
                emit(WorkerMessage::Decoded {
                    reused: result.reused,
                    wav_count: result.wav_count,
                    directory: result.directory,
                });
            }
            Err(error) => {
                failed += 1;
                emit(WorkerMessage::DecodeFailed {
                    path: source.path.clone(),
                    error: format!("{error:#}"),
                });
            }
        }
        emit(WorkerMessage::Progress {
            done: index + 1,
            total: options.sources.len(),
        });
    }
    emit(WorkerMessage::Finished(ExportTally {
        stopped: options.cancel.load(Ordering::Relaxed),
        succeeded,
        reused: skipped,
        failed,
        warning_tracks: decoder_warnings,
        wavs: total_wav,
        by_category: by_category
            .into_iter()
            .map(|((category, language), count)| (category, language, count))
            .collect(),
    }));
}

pub struct ExtractResult {
    pub directory: PathBuf,
    pub wav_count: usize,
    pub reused: bool,
    pub decoder_warning_count: usize,
}

#[derive(Serialize, Deserialize, PartialEq, Debug)]
struct SourceStamp {
    path: PathBuf,
    bytes: u64,
    modified_ns: u128,
    fingerprint: u64,
    cue_fingerprint: Option<u64>,
}
impl SourceStamp {
    fn read(source: &SourceInfo) -> Result<Self> {
        let metadata = source.path.metadata()?;
        Ok(Self {
            path: source.path.canonicalize()?,
            bytes: metadata.len(),
            modified_ns: metadata.modified()?.duration_since(UNIX_EPOCH)?.as_nanos(),
            // Downloaded game files use fixed historical timestamps. Cache
            // identity must include bytes, including the AWB's naming sidecar.
            fingerprint: fingerprint(&source.path)?,
            cue_fingerprint: if source.kind == SourceKind::Awb
                && source.path.with_extension("acb").is_file()
            {
                Some(fingerprint(&source.path.with_extension("acb"))?)
            } else {
                None
            },
        })
    }
}

fn fingerprint(path: &Path) -> Result<u64> {
    let mut file = fs::File::open(path)?;
    let mut hash = DefaultHasher::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hash.write(&buffer[..read]);
    }
    Ok(hash.finish())
}
#[derive(Serialize, Deserialize)]
struct OutputFile {
    name: String,
    bytes: u64,
    stream_index: usize,
    cue_name: String,
    sample_rate: u32,
    channels: u32,
    samples: u64,
    decoder_warning: bool,
}
#[derive(Serialize, Deserialize)]
struct Completion {
    version: u32,
    source: SourceStamp,
    files: Vec<OutputFile>,
}
const COMPLETION_FILE: &str = ".extract-complete.json";
const STAGE_PREFIX: &str = ".gf2-partial-";
const STAGE_ABANDONED_AFTER: Duration = Duration::from_secs(600);

fn complete_output(directory: &Path, stamp: &SourceStamp) -> Option<(usize, usize)> {
    let completed: Completion =
        serde_json::from_slice(&fs::read(directory.join(COMPLETION_FILE)).ok()?).ok()?;
    if completed.version != 4 || completed.source != *stamp || completed.files.is_empty() {
        return None;
    }
    for file in &completed.files {
        // Manifests are local data, not trusted paths.
        if Path::new(&file.name).file_name()?.to_str()? != file.name || file.bytes <= 44 {
            return None;
        }
        if fs::metadata(directory.join(&file.name)).ok()?.len() != file.bytes {
            return None;
        }
    }
    Some((
        completed.files.len(),
        completed
            .files
            .iter()
            .filter(|file| file.decoder_warning)
            .count(),
    ))
}

/// The finished, current output of `source` under `options`, if there is one.
pub fn existing_output(source: &SourceInfo, options: &ExtractOptions) -> Option<ExtractResult> {
    let stamp = SourceStamp::read(source).ok()?;
    let directory = output_directory(source, options);
    let (wav_count, decoder_warning_count) = complete_output(&directory, &stamp)?;
    Some(ExtractResult {
        directory,
        wav_count,
        reused: true,
        decoder_warning_count,
    })
}

pub fn extract_source(source: &SourceInfo, options: &ExtractOptions) -> Result<ExtractResult> {
    let stamp = SourceStamp::read(source)?;
    let directory = output_directory(source, options);
    if !options.overwrite
        && let Some((wav_count, decoder_warning_count)) = complete_output(&directory, &stamp)
    {
        return Ok(ExtractResult {
            directory,
            wav_count,
            reused: true,
            decoder_warning_count,
        });
    }
    fs::create_dir_all(&directory)?;
    if !options.overwrite
        && (directory.join(COMPLETION_FILE).exists() || !wav_files(&directory)?.is_empty())
    {
        bail!(tr!(
            "已有输出不完整或源文件已更新；请在导出面板选择“全部覆盖”后重试",
            "The existing output is incomplete or the source has changed; choose \"Overwrite all\" in the export panel and retry"
        ));
    }
    let pack = crate::audio::read_pack(&source.path)?;
    let expected = pack.streams.len();
    ensure!(
        expected > 0,
        tr!("音频包没有音轨", "The audio pack has no tracks")
    );
    remove_abandoned_stages(&options.output_directory);
    // Keep staged paths short even when the final bank is nested deeply.
    let stage = options.output_directory.join(format!(
        "{STAGE_PREFIX}{}-{}",
        std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
    ));
    fs::create_dir(&stage)?;
    let result = (|| {
        let outputs: Vec<OutputFile> = (0..expected)
            .into_par_iter()
            .map(|position| decode_stream(&pack, position, &stage))
            .collect::<Result<_>>()?;
        let warning_count = outputs.iter().filter(|file| file.decoder_warning).count();
        let files = wav_files(&stage)?;
        ensure!(
            files.len() == expected,
            tr!(
                "音轨数不完整：期望 {expected}，实际 {}",
                "Incomplete tracks: expected {expected}, got {}",
                files.len(),
                expected = expected
            )
        );
        ensure!(
            SourceStamp::read(source)? == stamp,
            tr!(
                "解码期间游戏资源发生变化，请重试",
                "The game resources changed during decoding; please retry"
            )
        );
        // Invalidate completion before replacing files, so a failed publish
        // cannot leave a successful marker on a partial replacement.
        let marker = directory.join(COMPLETION_FILE);
        if marker.exists() {
            fs::remove_file(&marker)?;
        }
        if options.overwrite {
            for file in wav_files(&directory)? {
                fs::remove_file(file)?;
            }
        }
        for file in files {
            fs::rename(
                &file,
                directory.join(
                    file.file_name()
                        .context(tr!("WAV 文件名无效", "Invalid WAV file name"))?,
                ),
            )?;
        }
        let manifest = serde_json::to_vec_pretty(&Completion {
            version: 4,
            source: stamp,
            files: outputs,
        })?;
        fs::write(directory.join("audio-index.json"), &manifest)?;
        fs::write(marker, manifest)?;
        Ok(ExtractResult {
            directory: directory.clone(),
            wav_count: expected,
            reused: false,
            decoder_warning_count: warning_count,
        })
    })();
    let cleanup = fs::remove_dir_all(&stage);
    match (result, cleanup) {
        (Ok(result), Ok(())) => Ok(result),
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error).context(tr!(
            "无法清理临时导出目录",
            "Cannot clean up the temporary export folder"
        )),
    }
}

/// Staging folders left by a process that was killed. Another running export
/// keeps writing to its own folder, so a recently changed one is left alone.
fn remove_abandoned_stages(output_directory: &Path) {
    let Ok(entries) = fs::read_dir(output_directory) else {
        return;
    };
    for entry in entries.flatten() {
        let age = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .ok()
            .and_then(|modified| modified.elapsed().ok());
        if let (Some(age), Some(name)) = (age, entry.file_name().to_str().map(str::to_owned))
            && is_abandoned_stage(&name, std::process::id(), age)
        {
            let _ = fs::remove_dir_all(entry.path());
        }
    }
}

fn is_abandoned_stage(name: &str, own_process: u32, age: Duration) -> bool {
    let process = name
        .strip_prefix(STAGE_PREFIX)
        .and_then(|rest| rest.split('-').next())
        .and_then(|process| process.parse::<u32>().ok());
    process.is_some_and(|process| process != own_process) && age >= STAGE_ABANDONED_AFTER
}

fn wav_files(directory: &Path) -> Result<Vec<PathBuf>> {
    let mut result = Vec::new();
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        if entry.file_type()?.is_file()
            && entry
                .path()
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("wav"))
        {
            result.push(entry.path());
        }
    }
    result.sort();
    Ok(result)
}

fn safe_folder(name: &str) -> String {
    let result: String = name
        .chars()
        .take(100)
        .map(|c| {
            if c.is_control() || "<>:\"/\\|?*".contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect();
    let result = result.trim_matches([' ', '.']);
    if result.is_empty() {
        "_".to_owned()
    } else {
        format!("_{result}")
    }
}

fn output_directory(source: &SourceInfo, options: &ExtractOptions) -> PathBuf {
    let key = source.key.strip_prefix("VO_").unwrap_or(&source.key);
    // Preserve existing character paths for resource names from the game.
    let mut path = if source.category == Category::Character || options.character_name.is_some() {
        let character = options
            .character_name
            .as_ref()
            .or(source.character.as_ref())
            .map(String::as_str)
            .unwrap_or("Unknown");
        let group = if character
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
            && !character.is_empty()
        {
            character.to_owned()
        } else {
            safe_folder(character)
        };
        options.output_directory.join(group)
    } else {
        options.output_directory.join(source.category.folder())
    };
    path = path.join(source.language.folder()).join(key);
    if source.kind == SourceKind::Acb {
        path.push("memory");
    } else if source.kind == SourceKind::Hca {
        path.push("hca");
    }
    path
}

/// Decodes one stream into `stage` under its final name.
fn decode_stream(pack: &crate::audio::Pack, position: usize, stage: &Path) -> Result<OutputFile> {
    let stream = &pack.streams[position];
    let decoded = pack.decode(position)?;
    let cue_name = stream.cue_name.clone().unwrap_or_default();
    let name = wav_name(stream.index, &cue_name);
    let bytes = decoded.wav.len() as u64;
    ensure!(
        bytes > 44,
        tr!(
            "WAV 文件为空：{name}",
            "The WAV file is empty: {name}",
            name = name
        )
    );
    fs::write(stage.join(&name), &decoded.wav)?;
    Ok(OutputFile {
        name,
        bytes,
        stream_index: stream.index,
        cue_name,
        sample_rate: decoded.sample_rate,
        channels: decoded.channels,
        samples: decoded.samples,
        decoder_warning: !decoded.key_confirmed || decoded.failed_frames > 0,
    })
}

fn wav_name(index: usize, cue_name: &str) -> String {
    let primary_name = cue_name.split(';').next().unwrap_or_default().trim();
    let label: String = primary_name
        .chars()
        .take(70)
        .map(|c| {
            if c.is_control() || "<>:\"/\\|?*".contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect();
    if label.is_empty() {
        format!("{index:05}.wav")
    } else {
        format!("{index:05}_{label}.wav")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_old_staging_folders_of_other_processes_are_abandoned() {
        let old = STAGE_ABANDONED_AFTER;
        let recent = Duration::from_secs(5);
        assert!(is_abandoned_stage(".gf2-partial-12-999", 7, old));
        assert!(!is_abandoned_stage(".gf2-partial-12-999", 7, recent));
        assert!(!is_abandoned_stage(".gf2-partial-7-999", 7, old));
        assert!(!is_abandoned_stage("Story", 7, old));
        assert!(!is_abandoned_stage(".gf2-partial-x-999", 7, old));
    }
    #[test]
    fn names_wavs_after_the_first_cue() {
        assert_eq!(wav_name(3, ""), "00003.wav");
        assert_eq!(wav_name(3, "first; second"), "00003_first.wav");
        assert_eq!(wav_name(12, "a/b:c"), "00012_a_b_c.wav");
        assert_eq!(
            wav_name(1, &"x".repeat(80)),
            format!("00001_{}.wav", "x".repeat(70))
        );
    }
    #[test]
    fn keeps_character_layout_and_separates_memory_banks() {
        let mut source = SourceInfo::from_path(Path::new("VO_Bedroom_Tololo_CN.awb"), 0).unwrap();
        let options = ExtractOptions {
            output_directory: PathBuf::from("out"),
            sources: vec![],
            character_name: Some("Tololo".to_owned()),
            overwrite: false,
            cancel: Arc::new(AtomicBool::new(false)),
        };
        assert_eq!(
            output_directory(&source, &options),
            PathBuf::from("out/Tololo/CN/Bedroom_Tololo")
        );
        source.kind = SourceKind::Acb;
        assert!(output_directory(&source, &options).ends_with("Bedroom_Tololo/memory"));
    }
    #[test]
    fn completion_requires_every_output_and_current_source() {
        let root = std::env::temp_dir().join(format!(
            "gf2-completion-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&root).unwrap();
        let stamp = SourceStamp {
            path: PathBuf::from("bank.awb"),
            bytes: 1,
            modified_ns: 1,
            fingerprint: 1,
            cue_fingerprint: None,
        };
        fs::write(root.join("one.wav"), [0; 60]).unwrap();
        let complete = Completion {
            version: 4,
            source: stamp,
            files: vec![
                OutputFile {
                    name: "one.wav".to_owned(),
                    bytes: 60,
                    stream_index: 1,
                    cue_name: String::new(),
                    sample_rate: 44100,
                    channels: 2,
                    samples: 4,
                    decoder_warning: false,
                },
                OutputFile {
                    name: "two.wav".to_owned(),
                    bytes: 70,
                    stream_index: 2,
                    cue_name: String::new(),
                    sample_rate: 44100,
                    channels: 2,
                    samples: 6,
                    decoder_warning: false,
                },
            ],
        };
        fs::write(
            root.join(COMPLETION_FILE),
            serde_json::to_vec(&complete).unwrap(),
        )
        .unwrap();
        assert_eq!(complete_output(&root, &complete.source), None);
        fs::write(root.join("two.wav"), [0; 70]).unwrap();
        assert_eq!(complete_output(&root, &complete.source), Some((2, 0)));
        let changed = SourceStamp {
            bytes: 2,
            ..complete.source
        };
        assert_eq!(complete_output(&root, &changed), None);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn detects_same_size_changes_with_fixed_game_timestamps() {
        let root = std::env::temp_dir().join(format!(
            "gf2-fingerprint-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&root).unwrap();
        let path = root.join("VO_Tololo_CN.awb");
        fs::write(&path, b"original").unwrap();
        fs::write(path.with_extension("acb"), b"cue-one").unwrap();
        let source = SourceInfo::from_path(&path, 8).unwrap();
        let before = SourceStamp::read(&source).unwrap();
        let modified = path.metadata().unwrap().modified().unwrap();
        fs::write(&path, b"modified").unwrap();
        fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(modified)
            .unwrap();
        let after = SourceStamp::read(&source).unwrap();
        assert_eq!(
            (before.bytes, before.modified_ns),
            (after.bytes, after.modified_ns)
        );
        assert_ne!(before, after);
        fs::write(path.with_extension("acb"), b"cue-two").unwrap();
        assert_ne!(after, SourceStamp::read(&source).unwrap());
        fs::remove_dir_all(root).unwrap();
    }
}
