//! UI-framework-independent application state.
//!
//! Everything here is plain Rust so the filtering, validation and worker-message
//! handling can be unit tested without a window.

use super::{live2d::Live2dData, portrait::PortraitData, story::StoryData};
use crate::{
    catalog::{Category, SourceInfo, scan_resources},
    discovery::{auto_detect_game_directory, validate_directories},
    extraction::{ExportTally, ExtractOptions, WorkerMessage, run_extraction},
    settings::Settings,
    story::{
        CachePolicy, IndexProgress, Selection,
        export::{ExportOptions, ExportSummary, LineFilter as LineKeep, LogLine, export_filtered},
        link::Linked,
        load_with_policy,
    },
};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver},
    },
    thread,
    time::Instant,
};

pub const DEFAULT_OUTPUT_FOLDER: &str = "gf2_echo_archive_output";

fn resolve_output(saved: Option<&std::path::Path>, base: &std::path::Path) -> PathBuf {
    saved
        .filter(|path| !path.as_os_str().is_empty())
        .map(std::path::Path::to_path_buf)
        .unwrap_or_else(|| base.join(DEFAULT_OUTPUT_FOLDER))
}

pub fn disclaimer() -> &'static str {
    tr!(
        "本工具仅供个人学习、技术研究及合法备份使用。请勿将本工具用于未经授权的传播、商业利用、规避版权保护或其他违法违规行为。因使用本工具产生的任何后果，由使用者自行承担。",
        "This tool is for personal study, technical research and lawful backups only. Do not use it for unauthorised distribution, commercial use, circumventing copyright protection or any other unlawful purpose. You are solely responsible for any consequences of using this tool."
    )
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LogLevel {
    /// Separator inserted by the UI at the start of each task.
    Heading,
    Info,
    Success,
    Warning,
    Error,
}

#[derive(Clone, Debug)]
pub struct LogEntry {
    pub text: String,
    pub level: LogLevel,
}

impl LogEntry {
    pub(super) fn normal(text: String) -> Self {
        Self {
            text,
            level: LogLevel::Info,
        }
    }

    pub(super) fn success(text: String) -> Self {
        Self {
            text,
            level: LogLevel::Success,
        }
    }

    pub(super) fn warning(text: String) -> Self {
        Self {
            text,
            level: LogLevel::Warning,
        }
    }

    pub(super) fn error(text: String) -> Self {
        Self {
            text,
            level: LogLevel::Error,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LogFilter {
    All,
    Warnings,
    Errors,
}

impl LogFilter {
    pub fn accepts(self, level: LogLevel) -> bool {
        match self {
            Self::All => true,
            Self::Warnings => matches!(level, LogLevel::Warning | LogLevel::Heading),
            Self::Errors => matches!(level, LogLevel::Error | LogLevel::Heading),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TaskKind {
    Scan,
    Export,
    StoryIndex,
    StoryExport,
    PortraitIndex,
    PortraitExport,
    Live2dIndex,
    Live2dExport,
    Roster,
}

/// Stages of the parse flow, in order.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ParseStage {
    Audio,
    Story,
    Portrait,
    Live2d,
    Roster,
}

impl ParseStage {
    pub const COUNT: usize = 5;

    pub fn label(self) -> &'static str {
        match self {
            Self::Audio => tr!("音频", "Audio"),
            Self::Story => tr!("剧情", "Story"),
            Self::Portrait => tr!("立绘", "Portraits"),
            Self::Live2d => "Live2D",
            Self::Roster => tr!("角色与头像", "Characters & avatars"),
        }
    }

    /// Position shown to the user, from 1.
    pub fn number(self) -> usize {
        self as usize + 1
    }

    pub fn next(self) -> Option<Self> {
        match self {
            Self::Audio => Some(Self::Story),
            Self::Story => Some(Self::Portrait),
            Self::Portrait => Some(Self::Live2d),
            Self::Live2d => Some(Self::Roster),
            Self::Roster => None,
        }
    }
}

/// Why the parse flow is running.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ParseFlow {
    /// The user asked to parse or update: unchanged indexes come from cache.
    Parse,
    /// The user asked for a full rebuild.
    Rebuild,
    /// Launch with unchanged game files.
    Restore,
    /// Launch with changed game files: the old results load as they are.
    RestoreStale,
}

impl ParseFlow {
    pub fn policy(self) -> CachePolicy {
        match self {
            Self::Parse | Self::Restore => CachePolicy::Current,
            Self::Rebuild => CachePolicy::Rebuild,
            Self::RestoreStale => CachePolicy::AcceptStale,
        }
    }

    /// Started by the launch rather than by the user.
    pub fn restores(self) -> bool {
        matches!(self, Self::Restore | Self::RestoreStale)
    }
}

/// State of the whole parse flow.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ParseStatus {
    NotParsed,
    /// The launch is deciding whether the last results can be reused.
    Checking,
    Running {
        stage: ParseStage,
        flow: ParseFlow,
    },
    Done {
        characters: usize,
        /// The game files have changed since these results were built.
        update_available: bool,
    },
    Failed,
}

/// Messages from the story index and story export workers.
pub enum StoryMessage {
    Log(LogLine),
    Stage(String),
    Progress { done: usize, total: usize },
    Indexed(Result<Option<Arc<StoryData>>, String>),
    Exported(Result<Box<(ExportSummary, Linked)>, String>),
}

/// Key of a linked WAV: (script, row, language, variant).
pub type WavKey = (String, i64, &'static str, &'static str);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TaskState {
    Running,
    Finished,
    Stopped,
    Failed,
}

#[derive(Clone, Debug)]
pub struct TaskRecord {
    pub kind: TaskKind,
    pub title: String,
    pub state: TaskState,
    pub done: usize,
    pub total: usize,
    pub current: Option<String>,
    pub detail: String,
    pub decoder_warnings: usize,
    pub failures: usize,
}

/// Per-row export status shown in the audio table (derived from worker messages).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RowStatus {
    Queued,
    Decoding,
    Done,
    Reused,
    Warning(usize),
    Failed,
    NotRun,
}

impl RowStatus {
    pub fn label(self) -> String {
        match self {
            Self::Queued => tr!("排队中", "Queued").to_owned(),
            Self::Decoding => tr!("解码中", "Decoding").to_owned(),
            Self::Done => tr!("已导出", "Exported").to_owned(),
            Self::Reused => tr!("已复用", "Reused").to_owned(),
            Self::Warning(count) => tr!(
                "{count} 条解码警告",
                "{count} decoder warnings",
                count = count
            ),
            Self::Failed => tr!("导出失败", "Export failed").to_owned(),
            Self::NotRun => tr!("未执行", "Not run").to_owned(),
        }
    }
}

#[derive(Default, Clone)]
pub struct ScanStats {
    pub metadata_only_acb: usize,
    pub superseded: usize,
}

pub struct AppState {
    pub game_directory: String,
    pub output_directory: String,
    pub disclaimer_accepted: bool,
    pub overwrite: bool,
    pub logs: Vec<LogEntry>,
    /// Bumped whenever `logs` changes.
    pub logs_version: usize,
    /// Bumped whenever `logs` is cleared (the log is otherwise append-only).
    pub logs_generation: usize,
    pub running: bool,
    pub parse: ParseStatus,
    /// The next parse stage is waiting to be started.
    pub parse_stage_pending: bool,
    /// When the running flow (or the startup check before it) began.
    parse_started: Option<Instant>,
    pub progress: f32,
    pub progress_label: String,
    pub sources: Vec<SourceInfo>,
    /// Bumped whenever a scan replaces `sources`.
    pub sources_version: usize,
    pub scanned_root: Option<PathBuf>,
    pub scan_stats: Option<ScanStats>,
    pub cancel: Arc<AtomicBool>,
    pub tasks: Vec<TaskRecord>,
    export_batch: Vec<PathBuf>,
    pub row_status: HashMap<PathBuf, RowStatus>,
    pub story: Option<Arc<StoryData>>,
    /// Game directory the loaded story index belongs to.
    pub story_root: Option<PathBuf>,
    pub story_wavs: HashMap<WavKey, String>,
    pub story_last_export: Option<ExportSummary>,
    pub portrait: Option<Arc<PortraitData>>,
    /// Game directory the loaded portrait index belongs to.
    pub portrait_root: Option<PathBuf>,
    pub portrait_last_export: Option<crate::portrait::ExportSummary>,
    pub live2d: Option<Arc<Live2dData>>,
    /// Game directory the loaded Live2D index belongs to.
    pub live2d_root: Option<PathBuf>,
    pub live2d_last_export: Option<crate::live2d::ExportSummary>,
    /// Latest Cubism Core results by model name.
    pub live2d_core: HashMap<String, crate::live2d::core::CoreReport>,
}

impl AppState {
    /// Startup state: the saved game directory when it still exists, else
    /// the detected one; the saved output directory; the saved notice.
    pub fn from_settings(settings: &Settings) -> Self {
        let base = crate::paths::exe_dir().unwrap_or_else(|| PathBuf::from("."));
        let output_directory = resolve_output(settings.output_directory.as_deref(), &base);
        let saved_game = settings.game_directory.clone().filter(|path| path.is_dir());
        let mut state = Self::with_environment(
            saved_game.or_else(auto_detect_game_directory),
            output_directory,
        );
        state.disclaimer_accepted = settings.notice_accepted_at.is_some();
        state
    }

    pub fn with_environment(
        detected_game_directory: Option<PathBuf>,
        output_directory: PathBuf,
    ) -> Self {
        let mut logs = Vec::new();
        match &detected_game_directory {
            Some(path) => logs.push(LogEntry::normal(tr!(
                "[环境检查] 已识别游戏目录：{}",
                "[Setup] Game directory detected: {}",
                path.display()
            ))),
            None => logs.push(LogEntry::normal(
                tr!(
                    "[配置提示] 未能自动识别游戏目录，请手动指定。",
                    "[Setup] Could not detect the game directory; please set it manually."
                )
                .to_owned(),
            )),
        }

        Self {
            game_directory: detected_game_directory
                .map(|path| path.to_string_lossy().into_owned())
                .unwrap_or_default(),
            output_directory: output_directory.to_string_lossy().into_owned(),
            disclaimer_accepted: false,
            overwrite: false,
            logs,
            logs_version: 0,
            logs_generation: 0,
            running: false,
            parse: ParseStatus::NotParsed,
            parse_stage_pending: false,
            parse_started: None,
            progress: 0.0,
            progress_label: String::new(),
            sources: Vec::new(),
            sources_version: 0,
            scanned_root: None,
            scan_stats: None,
            cancel: Arc::new(AtomicBool::new(false)),
            tasks: Vec::new(),
            export_batch: Vec::new(),
            row_status: HashMap::new(),
            story: None,
            story_root: None,
            story_wavs: HashMap::new(),
            story_last_export: None,
            portrait: None,
            portrait_root: None,
            portrait_last_export: None,
            live2d: None,
            live2d_root: None,
            live2d_last_export: None,
            live2d_core: HashMap::new(),
        }
    }

    pub(super) fn push_log(&mut self, entry: LogEntry) {
        self.logs.push(entry);
        self.logs_version += 1;
    }

    pub fn clear_logs(&mut self) {
        self.logs.clear();
        self.logs_version += 1;
        self.logs_generation += 1;
    }

    pub(super) fn push_heading(&mut self, title: &str) {
        self.push_log(LogEntry {
            text: format!("── {title} ──"),
            level: LogLevel::Heading,
        });
    }

    pub(super) fn append_error(&mut self, message: &str) {
        self.push_log(LogEntry::error(tr!(
            "[参数错误] {message}",
            "[Invalid input] {message}",
            message = message
        )));
    }

    pub fn is_scanned(&self) -> bool {
        self.scanned_root.as_ref() == Some(&PathBuf::from(self.game_directory.trim()))
    }

    pub fn is_scanning(&self) -> bool {
        self.running && self.current_task_kind() == Some(TaskKind::Scan)
    }

    pub fn current_task_kind(&self) -> Option<TaskKind> {
        self.tasks
            .last()
            .filter(|task| task.state == TaskState::Running)
            .map(|task| task.kind)
    }

    pub fn current_task(&self) -> Option<&TaskRecord> {
        self.tasks
            .last()
            .filter(|task| task.state == TaskState::Running)
    }

    pub fn stop_requested(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    pub fn request_stop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    /// Enters the startup check; the caller runs it.
    pub fn begin_startup_check(&mut self) {
        self.parse = ParseStatus::Checking;
        self.parse_started = Some(Instant::now());
    }

    /// Enters the first stage; the caller starts it. A restore keeps timing
    /// from the startup check, so the reported time is the whole launch.
    pub fn begin_parse(&mut self, flow: ParseFlow) {
        if !flow.restores() || self.parse_started.is_none() {
            self.parse_started = Some(Instant::now());
        }
        self.parse = ParseStatus::Running {
            stage: ParseStage::Audio,
            flow,
        };
        self.parse_stage_pending = true;
    }

    /// Ends the flow after its last stage and reports how long it took.
    pub fn complete_parse(&mut self, characters: usize) {
        let ParseStatus::Running { flow, .. } = self.parse else {
            return;
        };
        let seconds = self
            .parse_started
            .take()
            .map_or(0.0, |started| started.elapsed().as_secs_f64());
        let task = if flow.restores() {
            tr!("载入上次的解析结果", "Load the last parse results")
        } else {
            tr!("解析", "Parse")
        };
        self.push_log(LogEntry::normal(tr!(
            "[完成] {task}：{} 个角色；用时 {seconds:.1} 秒",
            "[Done] {task}: {} characters; took {seconds:.1} s",
            super::fmt_count(characters),
            task = task,
            seconds = seconds
        )));
        let update_available = flow == ParseFlow::RestoreStale;
        if update_available {
            self.push_log(LogEntry::normal(
                tr!("[资源更新] 游戏资源在上次解析后有变化，可以点击标题栏的“更新”。", "[Update] The game resources changed since the last parse; click \"Update\" in the title bar.").to_owned(),
            ));
        }
        self.parse = ParseStatus::Done {
            characters,
            update_available,
        };
    }

    /// The last results are loading: the startup check or a restore.
    pub fn restoring(&self) -> bool {
        match self.parse {
            ParseStatus::Checking => true,
            ParseStatus::Running { flow, .. } => flow.restores(),
            _ => false,
        }
    }

    pub fn update_available(&self) -> bool {
        matches!(
            self.parse,
            ParseStatus::Done {
                update_available: true,
                ..
            }
        )
    }

    /// When the running stage's worker has ended, decides what follows.
    /// Returns true when the next stage is ready to be started. The roster
    /// stage ends through its own completion instead.
    pub fn finish_parse_stage(&mut self) -> bool {
        let ParseStatus::Running { stage, flow } = self.parse else {
            return false;
        };
        if self.running || self.parse_stage_pending || stage == ParseStage::Roster {
            return false;
        }
        let outcome = self.tasks.last().map(|task| task.state);
        if self.stop_requested() || outcome == Some(TaskState::Stopped) {
            self.parse = ParseStatus::NotParsed;
            return false;
        }
        match (outcome, stage.next()) {
            (Some(TaskState::Finished), Some(next)) => {
                self.parse = ParseStatus::Running { stage: next, flow };
                self.parse_stage_pending = true;
                true
            }
            _ => {
                self.parse = ParseStatus::Failed;
                false
            }
        }
    }

    /// Share of the whole flow that is done. Every stage counts equally; a
    /// stage that reports no finer progress (a cache hit, the audio scan, the
    /// roster) stays at its starting share until it ends.
    pub fn parse_progress(&self) -> Option<f32> {
        let ParseStatus::Running { stage, .. } = self.parse else {
            return None;
        };
        let within_stage = self
            .current_task()
            .filter(|task| task.total > 0)
            .map_or(0., |_| self.progress.clamp(0., 1.));
        Some((stage as usize as f32 + within_stage) / ParseStage::COUNT as f32)
    }

    /// Title-bar status: not parsed, loading or parsing with the stage and share,
    /// parsed with the character count, update available, or failed.
    pub fn parse_text(&self) -> String {
        match self.parse {
            ParseStatus::NotParsed => tr!("未解析", "Not parsed").to_owned(),
            ParseStatus::Checking => tr!("载入中", "Loading").to_owned(),
            ParseStatus::Failed => tr!("解析失败", "Parse failed").to_owned(),
            ParseStatus::Done {
                update_available: true,
                ..
            } => tr!("发现资源更新", "Update available").to_owned(),
            ParseStatus::Done { characters, .. } => {
                tr!(
                    "已解析 · {} 个角色",
                    "Parsed · {} characters",
                    super::fmt_count(characters)
                )
            }
            ParseStatus::Running { .. } if self.stop_requested() => {
                tr!("正在停止…", "Stopping…").to_owned()
            }
            ParseStatus::Running { stage, flow } => {
                let position = format!(
                    "{} · {} {}/{}",
                    if flow.restores() {
                        tr!("载入中", "Loading")
                    } else {
                        tr!("解析中", "Parsing")
                    },
                    stage.label(),
                    stage.number(),
                    ParseStage::COUNT
                );
                // Rounded down, so 100% never shows while work remains.
                let percent = (self.parse_progress().unwrap_or(0.) * 100.) as u32;
                format!("{position} · {percent}%")
            }
        }
    }

    /// Drops everything derived from the game directory.
    pub fn clear_parsed(&mut self) {
        self.sources.clear();
        self.sources_version += 1;
        self.scanned_root = None;
        self.scan_stats = None;
        self.row_status.clear();
        self.story = None;
        self.story_root = None;
        self.story_wavs.clear();
        self.portrait = None;
        self.portrait_root = None;
        self.live2d = None;
        self.live2d_root = None;
        self.live2d_core.clear();
    }

    pub fn start_scan(&mut self) -> Option<Receiver<WorkerMessage>> {
        if self.running {
            return None;
        }
        let root = PathBuf::from(self.game_directory.trim());
        if !root.is_dir() {
            self.append_error(tr!(
                "请选择有效的游戏目录。",
                "Please choose a valid game directory."
            ));
            return None;
        }
        self.cancel = Arc::new(AtomicBool::new(false));
        let cancel = self.cancel.clone();
        let (sender, receiver) = mpsc::channel();
        self.scanned_root = Some(root.clone());
        self.row_status.clear();
        self.running = true;
        self.progress = 0.0;
        self.progress_label = tr!("正在扫描资源…", "Scanning resources…").to_owned();
        self.push_heading(tr!("扫描资源", "Scan resources"));
        self.push_log(LogEntry::normal(tr!(
            "[开始扫描] {}",
            "[Scan started] {}",
            root.display()
        )));
        self.tasks.push(TaskRecord {
            kind: TaskKind::Scan,
            title: tr!("扫描资源", "Scan resources").to_owned(),
            state: TaskState::Running,
            done: 0,
            total: 0,
            current: Some(root.display().to_string()),
            detail: String::new(),
            decoder_warnings: 0,
            failures: 0,
        });
        thread::spawn(move || {
            let report = scan_resources(&root, &cancel);
            let _ = sender.send(WorkerMessage::Scanned(report));
        });
        Some(receiver)
    }

    /// Exports the given audio packs. With `character`, Character-category
    /// packs are grouped under that resource name (as the character
    /// export did) and the other packs keep their category folders, so a
    /// mixed selection runs as two batches in one task.
    pub fn start_audio_export(
        &mut self,
        sources: Vec<SourceInfo>,
        character: Option<String>,
    ) -> Option<Receiver<WorkerMessage>> {
        if self.running {
            return None;
        }
        let game_directory = PathBuf::from(self.game_directory.trim());
        let output_directory = PathBuf::from(self.output_directory.trim());
        if !self.disclaimer_accepted {
            self.append_error(tr!(
                "请阅读并同意使用声明。",
                "Please read and accept the usage notice."
            ));
            return None;
        }
        if self.output_directory.trim().is_empty() {
            self.append_error(tr!(
                "请选择有效的输出目录。",
                "Please choose a valid output directory."
            ));
            return None;
        }
        if let Err(error) = validate_directories(&game_directory, &output_directory) {
            self.append_error(&format!("{error:#}"));
            return None;
        }
        if !self.is_scanned() {
            self.append_error(tr!(
                "游戏目录已变化，请先解析资源。",
                "The game directory has changed; please parse the resources first."
            ));
            return None;
        }
        if sources.is_empty() {
            self.append_error(tr!(
                "请先选择要导出的音频包。",
                "Please select the audio packs to export first."
            ));
            return None;
        }
        let batches: Vec<(Vec<SourceInfo>, Option<String>)> = match character {
            Some(name) => {
                let (grouped, rest): (Vec<_>, Vec<_>) = sources
                    .into_iter()
                    .partition(|source| source.category == Category::Character);
                [(grouped, Some(name)), (rest, None)]
                    .into_iter()
                    .filter(|(list, _)| !list.is_empty())
                    .collect()
            }
            None => vec![(sources, None)],
        };
        let count: usize = batches.iter().map(|(list, _)| list.len()).sum();
        let (sender, receiver) = mpsc::channel();
        let overwrite = self.overwrite;
        self.cancel = Arc::new(AtomicBool::new(false));
        let cancel = self.cancel.clone();

        let title = tr!(
            "导出音频 · {} 个音频包",
            "Export audio · {} audio packs",
            super::fmt_count(count)
        );
        self.push_heading(&title);
        self.push_log(LogEntry::normal(
            tr!(
                "[任务初始化] 正在准备资源扫描与解码流程…",
                "[Preparing] Getting the resource scan and decoding ready…"
            )
            .to_owned(),
        ));
        self.running = true;
        self.progress = 0.0;
        self.progress_label.clear();
        self.export_batch = batches
            .iter()
            .flat_map(|(list, _)| list.iter().map(|source| source.path.clone()))
            .collect();
        self.row_status = self
            .export_batch
            .iter()
            .map(|path| (path.clone(), RowStatus::Queued))
            .collect();
        if let Some(first) = self.export_batch.first() {
            self.row_status.insert(first.clone(), RowStatus::Decoding);
        }
        self.tasks.push(TaskRecord {
            kind: TaskKind::Export,
            title,
            state: TaskState::Running,
            done: 0,
            total: count,
            current: batches
                .first()
                .and_then(|(list, _)| list.first())
                .map(|source| source.key.clone()),
            detail: output_directory.display().to_string(),
            decoder_warnings: 0,
            failures: 0,
        });
        thread::spawn(move || {
            run_batches(batches, output_directory, overwrite, cancel, sender);
        });
        Some(receiver)
    }

    fn current_batch_path(&self) -> Option<&PathBuf> {
        let done = self.tasks.last().map(|task| task.done).unwrap_or(0);
        self.export_batch.get(done)
    }

    fn set_current_row(&mut self, status: RowStatus) {
        if let Some(path) = self.current_batch_path().cloned() {
            self.row_status.insert(path, status);
        }
    }

    pub fn worker_disconnected(&mut self) {
        if !self.running {
            return;
        }
        self.running = false;
        self.scanned_root = None;
        self.progress_label = tr!("任务异常结束", "The task ended abnormally").to_owned();
        self.push_log(LogEntry::error(
            tr!(
                "[后台任务错误] 工作线程已退出，请重新解析后重试。",
                "[Worker error] The worker thread exited; please parse again and retry."
            )
            .to_owned(),
        ));
        if let Some(task) = self.tasks.last_mut() {
            task.state = TaskState::Failed;
            task.current = None;
            task.detail = tr!("工作线程已退出", "The worker thread exited").to_owned();
        }
    }

    pub fn handle(&mut self, message: WorkerMessage) {
        match message {
            WorkerMessage::Log(line) => self.push_log(LogEntry::normal(line)),
            WorkerMessage::Decoded {
                reused,
                wav_count,
                directory,
            } => {
                if let Some(path) = self.current_batch_path().cloned()
                    && !matches!(self.row_status.get(&path), Some(RowStatus::Warning(_)))
                {
                    let status = if reused {
                        RowStatus::Reused
                    } else {
                        RowStatus::Done
                    };
                    self.row_status.insert(path, status);
                }
                let directory = directory.display();
                self.push_log(LogEntry::success(if reused {
                    tr!(
                        "[复用] {} 个 WAV → {}",
                        "[Reused] {} WAV → {}",
                        wav_count,
                        directory
                    )
                } else {
                    tr!(
                        "[完成] {} 个 WAV → {}",
                        "[Done] {} WAV → {}",
                        wav_count,
                        directory
                    )
                }));
            }
            WorkerMessage::DecoderWarning { tracks } => {
                self.set_current_row(RowStatus::Warning(tracks));
                if let Some(task) = self.tasks.last_mut() {
                    task.decoder_warnings += tracks;
                }
                self.push_log(LogEntry::warning(tr!(
                    "[解码器警告] {} 条音轨需要核查，通常是静音占位；文件已保留，并在 audio-index.json 中标注。",
                    "[Decoder warning] {} tracks need checking, usually silent placeholders; the files were kept and are marked in audio-index.json.",
                    tracks
                )));
            }
            WorkerMessage::DecodeFailed { path, error } => {
                self.set_current_row(RowStatus::Failed);
                if let Some(task) = self.tasks.last_mut() {
                    task.failures += 1;
                }
                self.push_log(LogEntry::error(tr!(
                    "[解码失败] {}：{}",
                    "[Decode failed] {}: {}",
                    path.display(),
                    error
                )));
            }
            WorkerMessage::Progress { done, total } => {
                self.progress = if total == 0 {
                    0.0
                } else {
                    done as f32 / total as f32
                };
                self.progress_label = format!("{done} / {total}");
                let next = self.export_batch.get(done).cloned();
                if let Some(task) = self.tasks.last_mut() {
                    task.done = done;
                    task.total = total;
                }
                if let Some(path) = next {
                    let key = self
                        .sources
                        .iter()
                        .find(|source| source.path == path)
                        .map(|source| source.key.clone());
                    if let Some(task) = self.tasks.last_mut() {
                        task.current = key;
                    }
                    if !self.stop_requested() {
                        self.row_status.insert(path, RowStatus::Decoding);
                    }
                }
            }
            WorkerMessage::Finished(summary) => {
                self.running = false;
                let stopped = self.stop_requested();
                self.progress_label = if stopped {
                    tr!("已停止", "Stopped")
                } else {
                    tr!("任务结束", "Task finished")
                }
                .to_owned();
                for status in self.row_status.values_mut() {
                    if matches!(status, RowStatus::Queued | RowStatus::Decoding) {
                        *status = RowStatus::NotRun;
                    }
                }
                if let Some(task) = self.tasks.last_mut() {
                    task.state = if stopped {
                        TaskState::Stopped
                    } else {
                        TaskState::Finished
                    };
                    task.current = None;
                    task.detail = summary.card_text();
                }
                let text = summary.log_text();
                self.push_log(if summary.stopped {
                    LogEntry::normal(text)
                } else {
                    LogEntry::success(text)
                });
            }
            WorkerMessage::Scanned(report) => {
                self.push_log(LogEntry::success(tr!(
                    "[扫描完成] {} 个音频包；{} 个纯索引 ACB；{} 个重复资源已优先使用缓存版本。",
                    "[Scan complete] {} audio packs; {} metadata-only ACB; {} duplicate resources resolved to the cached version.",
                    report.sources.len(),
                    report.metadata_only_acb,
                    report.superseded
                )));
                for category in Category::ALL {
                    let count = report
                        .sources
                        .iter()
                        .filter(|source| source.category == category)
                        .count();
                    self.push_log(LogEntry::normal(tr!(
                        "  {}：{count} 个包",
                        "  {}: {count} packs",
                        category.label(),
                        count = count
                    )));
                }
                for (extension, count) in &report.other_files {
                    self.push_log(LogEntry::normal(tr!(
                        "[其他资源] {extension}：{count} 个文件（尚未提供解包导出）",
                        "[Other resources] {extension}: {count} files (export not available yet)",
                        extension = extension,
                        count = count
                    )));
                }
                for warning in &report.warnings {
                    self.push_log(LogEntry::warning(warning.clone()));
                }
                let count = report.sources.len();
                self.scan_stats = Some(ScanStats {
                    metadata_only_acb: report.metadata_only_acb,
                    superseded: report.superseded,
                });
                self.sources = report.sources;
                self.sources_version += 1;
                if report.cancelled {
                    self.scanned_root = None;
                    self.push_log(LogEntry::normal(
                        tr!(
                            "[扫描已停止] 请重新解析后导出。",
                            "[Scan stopped] Please parse again before exporting."
                        )
                        .to_owned(),
                    ));
                }
                self.running = false;
                self.progress = if report.cancelled { 0.0 } else { 1.0 };
                self.progress_label = if report.cancelled {
                    tr!("扫描已停止", "Scan stopped")
                } else {
                    tr!("扫描完成，请勾选资源", "Scan complete; select resources")
                }
                .to_owned();
                if let Some(task) = self.tasks.last_mut() {
                    task.state = if report.cancelled {
                        TaskState::Stopped
                    } else {
                        TaskState::Finished
                    };
                    task.current = None;
                    task.done = usize::from(!report.cancelled);
                    task.total = 1;
                    task.detail = tr!("{} 个音频包", "{} audio packs", super::fmt_count(count));
                }
            }
        }
    }

    /// The loaded story index, if it belongs to the current game directory.
    pub fn story_data(&self) -> Option<&Arc<StoryData>> {
        let root = PathBuf::from(self.game_directory.trim());
        self.story
            .as_ref()
            .filter(|_| self.story_root.as_ref() == Some(&root))
    }

    /// Starts building (or loading the cached) story index in the background.
    pub fn start_story_index(&mut self, policy: CachePolicy) -> Option<Receiver<StoryMessage>> {
        if self.running {
            return None;
        }
        let root = PathBuf::from(self.game_directory.trim());
        if !root.is_dir() {
            self.append_error(tr!(
                "请选择有效的游戏目录。",
                "Please choose a valid game directory."
            ));
            return None;
        }
        self.cancel = Arc::new(AtomicBool::new(false));
        let cancel = self.cancel.clone();
        let (sender, receiver) = mpsc::channel();
        self.running = true;
        self.progress = 0.0;
        self.progress_label = tr!("正在建立剧情索引…", "Building the story index…").to_owned();
        let title = if policy == CachePolicy::Rebuild {
            tr!("重新建立剧情索引", "Rebuild the story index")
        } else {
            tr!("建立剧情索引", "Build the story index")
        };
        self.push_heading(title);
        self.push_log(LogEntry::normal(tr!(
            "[剧情索引] {}",
            "[Story index] {}",
            root.display()
        )));
        self.tasks.push(TaskRecord {
            kind: TaskKind::StoryIndex,
            title: title.to_owned(),
            state: TaskState::Running,
            done: 0,
            total: 0,
            current: Some(tr!("准备中", "Preparing").to_owned()),
            detail: String::new(),
            decoder_warnings: 0,
            failures: 0,
        });
        thread::spawn(move || {
            let progress = |event: IndexProgress| {
                let message = match event {
                    IndexProgress::Stage(stage) => StoryMessage::Stage(stage),
                    IndexProgress::Bundles { done, total } => {
                        // About 200 updates are plenty for the progress bar.
                        if done != total && done % (total / 200).max(1) != 0 {
                            return;
                        }
                        StoryMessage::Progress { done, total }
                    }
                };
                let _ = sender.send(message);
            };
            let result = load_with_policy(&root, policy, &cancel, &progress)
                .map(|index| index.map(|index| Arc::new(StoryData::new(index))))
                .map_err(|error| format!("{error:#}"));
            let _ = sender.send(StoryMessage::Indexed(result));
        });
        Some(receiver)
    }

    /// Exports one script or one chapter directory in the background.
    pub fn start_story_export(
        &mut self,
        selection: Selection,
        filter: Option<super::export::LineFilter>,
        with_audio: bool,
        by_speaker: bool,
    ) -> Option<Receiver<StoryMessage>> {
        if self.running {
            return None;
        }
        let Some(data) = self.story_data().cloned() else {
            self.append_error(tr!(
                "请先建立剧情索引。",
                "Please build the story index first."
            ));
            return None;
        };
        if !self.disclaimer_accepted {
            self.append_error(tr!(
                "请阅读并同意使用声明。",
                "Please read and accept the usage notice."
            ));
            return None;
        }
        let output = PathBuf::from(self.output_directory.trim());
        if self.output_directory.trim().is_empty() {
            self.append_error(tr!(
                "请选择有效的输出目录。",
                "Please choose a valid output directory."
            ));
            return None;
        }
        if let Err(error) = validate_directories(&data.index.game_root, &output) {
            self.append_error(&format!("{error:#}"));
            return None;
        }
        let sources = if with_audio {
            if !self.is_scanned() {
                self.append_error(tr!(
                    "同时导出语音需要先解析资源。",
                    "Exporting voices needs the resources to be parsed first."
                ));
                return None;
            }
            Some(self.sources.clone())
        } else {
            None
        };
        self.cancel = Arc::new(AtomicBool::new(false));
        let cancel = self.cancel.clone();
        let (sender, receiver) = mpsc::channel();
        let title = tr!(
            "导出剧情台词 · {}{}",
            "Export story lines · {}{}",
            selection.label(),
            if with_audio {
                tr!(" · 含语音", " · with voices")
            } else {
                ""
            }
        );
        self.push_heading(&title);
        self.running = true;
        self.progress = 0.0;
        self.progress_label.clear();
        self.tasks.push(TaskRecord {
            kind: TaskKind::StoryExport,
            title,
            state: TaskState::Running,
            done: 0,
            total: 0,
            current: Some(selection.label()),
            detail: output.display().to_string(),
            decoder_warnings: 0,
            failures: 0,
        });
        thread::spawn(move || {
            let options = ExportOptions {
                output,
                with_audio,
                by_speaker,
                cancel,
            };
            let log = |line: LogLine| {
                let _ = sender.send(StoryMessage::Log(line));
            };
            let keep = filter.as_deref().map(|keep| keep as LineKeep<'_>);
            let result = export_filtered(
                &data.index,
                &selection,
                keep,
                &options,
                sources.as_deref(),
                &log,
            )
            .map(Box::new)
            .map_err(|error| format!("{error:#}"));
            let _ = sender.send(StoryMessage::Exported(result));
        });
        Some(receiver)
    }

    pub fn handle_story(&mut self, message: StoryMessage) {
        match message {
            StoryMessage::Log(LogLine { text, problem }) => {
                if let Some(task) = self.tasks.last_mut() {
                    task.current = Some(text.clone());
                    if problem {
                        task.failures += 1;
                    }
                }
                self.push_log(if problem {
                    LogEntry::warning(text)
                } else {
                    LogEntry::normal(text)
                });
            }
            StoryMessage::Stage(stage) => {
                if let Some(task) = self.tasks.last_mut() {
                    task.current = Some(stage.clone());
                }
                self.push_log(LogEntry::normal(tr!(
                    "[剧情索引] {stage}",
                    "[Story index] {stage}",
                    stage = stage
                )));
            }
            StoryMessage::Progress { done, total } => {
                self.progress = if total == 0 {
                    0.0
                } else {
                    done as f32 / total as f32
                };
                self.progress_label = format!("{done} / {total}");
                if let Some(task) = self.tasks.last_mut() {
                    task.done = done;
                    task.total = total;
                }
            }
            StoryMessage::Indexed(result) => {
                self.running = false;
                let (state, detail) = match result {
                    Ok(Some(data)) => {
                        let summary = &data.index.summary;
                        let detail = tr!(
                            "{} 份脚本 · {} 条文本 · {} 条有配音 · 未解析 {} · {}",
                            "{} scripts · {} text lines · {} voiced · Unresolved {} · {}",
                            super::fmt_count(summary.scripts),
                            super::fmt_count(summary.text_lines),
                            super::fmt_count(summary.voiced_lines),
                            super::fmt_count(summary.unresolved),
                            if summary.from_cache {
                                tr!("命中缓存", "from cache")
                            } else {
                                tr!("已重建", "rebuilt")
                            }
                        );
                        self.push_log(LogEntry::success(tr!(
                            "[完成] 剧情索引：{detail}；用时 {:.1} 秒",
                            "[Done] Story index: {detail}; took {:.1} s",
                            summary.elapsed_ms as f64 / 1000.0,
                            detail = detail
                        )));
                        self.log_scan_warnings(
                            &data.index.scan_warnings,
                            summary.scan_warnings,
                            summary.from_cache,
                        );
                        self.story_root = Some(data.index.game_root.clone());
                        self.story = Some(data);
                        self.story_wavs.clear();
                        (TaskState::Finished, detail)
                    }
                    Ok(None) => {
                        self.push_log(LogEntry::normal(
                            tr!(
                                "[已停止] 剧情索引未完成。",
                                "[Stopped] The story index was not finished."
                            )
                            .to_owned(),
                        ));
                        (TaskState::Stopped, tr!("已停止", "Stopped").to_owned())
                    }
                    Err(error) => {
                        self.push_log(LogEntry::error(tr!(
                            "[剧情索引失败] {error}",
                            "[Story index failed] {error}",
                            error = error
                        )));
                        (TaskState::Failed, error)
                    }
                };
                self.progress_label = detail.clone();
                if let Some(task) = self.tasks.last_mut() {
                    task.state = state;
                    task.current = None;
                    task.detail = detail;
                }
            }
            StoryMessage::Exported(result) => {
                self.running = false;
                let (state, detail) = match result {
                    Ok(result) => {
                        let (summary, linked) = *result;
                        let detail = match summary.missing_audio {
                            Some(missing) => tr!(
                                "{} 条文本 · 关联 {} 个 WAV · 缺失 {}",
                                "{} text lines · {} linked WAV · Missing {}",
                                super::fmt_count(summary.lines),
                                super::fmt_count(summary.distinct_wavs),
                                super::fmt_count(missing)
                            ),
                            None => tr!(
                                "{} 条文本",
                                "{} text lines",
                                super::fmt_count(summary.lines)
                            ),
                        };
                        self.push_log(LogEntry::success(tr!(
                            "[完成] 剧情台词：{detail} → {}",
                            "[Done] Story text: {detail} → {}",
                            summary.directory.display(),
                            detail = detail
                        )));
                        for line in &linked.lines {
                            for voice in &line.voices {
                                if let Some(wav) = &voice.wav {
                                    self.story_wavs.insert(
                                        (
                                            line.identity.script.clone(),
                                            line.identity.row,
                                            voice.language,
                                            voice.variant,
                                        ),
                                        wav.clone(),
                                    );
                                }
                            }
                        }
                        self.story_last_export = Some(summary);
                        (TaskState::Finished, detail)
                    }
                    Err(error) => {
                        let stopped = self.stop_requested();
                        self.push_log(if stopped {
                            LogEntry::normal(tr!(
                                "[已停止] {error}",
                                "[Stopped] {error}",
                                error = error
                            ))
                        } else {
                            LogEntry::error(tr!(
                                "[剧情导出失败] {error}",
                                "[Story export failed] {error}",
                                error = error
                            ))
                        });
                        let state = if stopped {
                            TaskState::Stopped
                        } else {
                            TaskState::Failed
                        };
                        (state, error)
                    }
                };
                self.progress_label = detail.clone();
                if let Some(task) = self.tasks.last_mut() {
                    task.state = state;
                    task.current = None;
                    task.detail = detail;
                }
            }
        }
    }

    /// A fresh scan lists its warnings; a cached index only remembers how
    /// many there were, because their text depends on the language.
    pub(super) fn log_scan_warnings(&mut self, text: &[String], count: usize, from_cache: bool) {
        if from_cache {
            if count > 0 {
                self.push_log(LogEntry::warning(tr!(
                    "[缓存] 上次扫描有 {} 条警告；重新解析可查看详情。",
                    "[Cache] The last scan had {} warnings; rebuild the index to see them.",
                    count
                )));
            }
            return;
        }
        for warning in text.iter().take(50) {
            self.push_log(LogEntry::warning(warning.clone()));
        }
    }

    pub fn logs_text(&self, filter: LogFilter) -> String {
        self.logs
            .iter()
            .filter(|entry| filter.accepts(entry.level))
            .map(|entry| entry.text.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// Runs the extractor once per batch on this thread and forwards its
/// messages as one task: progress continues across batches and the
/// summaries are added up into one `Finished`.
fn run_batches(
    batches: Vec<(Vec<SourceInfo>, Option<String>)>,
    output_directory: PathBuf,
    overwrite: bool,
    cancel: Arc<AtomicBool>,
    sender: mpsc::Sender<WorkerMessage>,
) {
    let total: usize = batches.iter().map(|(list, _)| list.len()).sum();
    let mut offset = 0;
    let mut tallies = Vec::new();
    for (sources, character_name) in batches {
        if offset > 0 && cancel.load(Ordering::Relaxed) {
            break;
        }
        let count = sources.len();
        let (inner, messages) = mpsc::channel();
        let options = ExtractOptions {
            output_directory: output_directory.clone(),
            sources,
            character_name,
            overwrite,
            cancel: cancel.clone(),
        };
        let worker = thread::spawn(move || run_extraction(options, inner));
        for message in messages {
            let forwarded = match message {
                WorkerMessage::Progress { done, .. } => WorkerMessage::Progress {
                    done: offset + done,
                    total,
                },
                WorkerMessage::Finished(summary) => {
                    tallies.push(summary);
                    continue;
                }
                other => other,
            };
            if sender.send(forwarded).is_err() {
                return;
            }
        }
        let _ = worker.join();
        offset += count;
    }
    let _ = sender.send(WorkerMessage::Finished(ExportTally::merge(&tallies)));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::{Language, with_language};

    #[test]
    fn an_unset_output_uses_the_default_folder() {
        let base = std::path::Path::new(r"V:\App");
        let default = base.join(DEFAULT_OUTPUT_FOLDER);
        assert_eq!(resolve_output(None, base), default);
        assert_eq!(
            resolve_output(Some(std::path::Path::new("")), base),
            default
        );
        let chosen = std::path::Path::new(r"D:\Exports\audio");
        assert_eq!(resolve_output(Some(chosen), base), chosen);
    }
    use crate::catalog::ScanReport;

    fn parse_state() -> AppState {
        let mut state = AppState::with_environment(None, PathBuf::from("out"));
        state.begin_parse(ParseFlow::Parse);
        state.parse_stage_pending = false;
        state
    }

    fn finish_task(state: &mut AppState, outcome: TaskState) {
        state.tasks.push(TaskRecord {
            kind: TaskKind::Scan,
            title: String::new(),
            state: outcome,
            done: 0,
            total: 0,
            current: None,
            detail: String::new(),
            decoder_warnings: 0,
            failures: 0,
        });
    }

    #[test]
    fn parse_moves_through_the_stages_in_order() {
        let mut state = parse_state();
        let mut seen = Vec::new();
        loop {
            let ParseStatus::Running { stage, flow } = state.parse else {
                panic!("left the flow early");
            };
            assert_eq!(flow, ParseFlow::Parse);
            seen.push(stage);
            if stage == ParseStage::Roster {
                break;
            }
            finish_task(&mut state, TaskState::Finished);
            assert!(state.finish_parse_stage());
            // A second call before the stage starts must not skip one.
            assert!(!state.finish_parse_stage());
            state.parse_stage_pending = false;
        }
        assert_eq!(
            seen.iter().map(|stage| stage.number()).collect::<Vec<_>>(),
            [1, 2, 3, 4, 5]
        );
        // The roster stage ends through its own completion.
        assert!(!state.finish_parse_stage());
    }

    #[test]
    fn parse_waits_for_the_running_worker() {
        let mut state = parse_state();
        state.running = true;
        finish_task(&mut state, TaskState::Finished);
        assert!(!state.finish_parse_stage());
        assert!(matches!(state.parse, ParseStatus::Running { .. }));
    }

    #[test]
    fn a_stopped_stage_returns_to_not_parsed_and_a_failed_one_to_failed() {
        let mut state = parse_state();
        finish_task(&mut state, TaskState::Stopped);
        assert!(!state.finish_parse_stage());
        assert_eq!(state.parse, ParseStatus::NotParsed);

        let mut state = parse_state();
        finish_task(&mut state, TaskState::Failed);
        assert!(!state.finish_parse_stage());
        assert_eq!(state.parse, ParseStatus::Failed);

        // Stop wins even when the worker finished normally.
        let mut state = parse_state();
        finish_task(&mut state, TaskState::Finished);
        state.request_stop();
        assert!(!state.finish_parse_stage());
        assert_eq!(state.parse, ParseStatus::NotParsed);
    }

    #[test]
    fn parse_status_text_matches_the_title_bar_wording() {
        let mut state = AppState::with_environment(None, PathBuf::from("out"));
        assert_eq!(state.parse_text(), "未解析");
        state.parse = ParseStatus::Running {
            stage: ParseStage::Story,
            flow: ParseFlow::Parse,
        };
        assert_eq!(state.parse_text(), "解析中 · 剧情 2/5 · 20%");
        state.tasks.push(TaskRecord {
            kind: TaskKind::StoryIndex,
            title: String::new(),
            state: TaskState::Running,
            done: 38,
            total: 100,
            current: None,
            detail: String::new(),
            decoder_warnings: 0,
            failures: 0,
        });
        state.progress = 0.38;
        assert_eq!(state.parse_text(), "解析中 · 剧情 2/5 · 27%");
        state.parse = ParseStatus::Done {
            characters: 289,
            update_available: false,
        };
        assert_eq!(state.parse_text(), "已解析 · 289 个角色");
        state.parse = ParseStatus::Done {
            characters: 289,
            update_available: true,
        };
        assert_eq!(state.parse_text(), "发现资源更新");
        state.parse = ParseStatus::Failed;
        assert_eq!(state.parse_text(), "解析失败");
    }

    #[test]
    fn every_stage_shows_its_share_of_the_whole_parse() {
        let mut state = AppState::with_environment(None, PathBuf::from("out"));
        let stages = [
            (ParseStage::Audio, "音频 1/5 · 0%"),
            (ParseStage::Story, "剧情 2/5 · 20%"),
            (ParseStage::Portrait, "立绘 3/5 · 40%"),
            (ParseStage::Live2d, "Live2D 4/5 · 60%"),
            (ParseStage::Roster, "角色与头像 5/5 · 80%"),
        ];
        for (stage, text) in stages {
            state.parse = ParseStatus::Running {
                stage,
                flow: ParseFlow::Parse,
            };
            assert_eq!(state.parse_text(), format!("解析中 · {text}"));
        }
        state.tasks.push(TaskRecord {
            kind: TaskKind::Roster,
            title: String::new(),
            state: TaskState::Running,
            done: 999,
            total: 1000,
            current: None,
            detail: String::new(),
            decoder_warnings: 0,
            failures: 0,
        });
        state.progress = 0.999;
        assert_eq!(state.parse_text(), "解析中 · 角色与头像 5/5 · 99%");
    }

    #[test]
    fn launch_flows_use_the_restore_wording() {
        let mut state = AppState::with_environment(None, PathBuf::from("out"));
        state.begin_startup_check();
        assert_eq!(state.parse_text(), "载入中");
        assert!(state.restoring());
        for flow in [ParseFlow::Restore, ParseFlow::RestoreStale] {
            state.begin_parse(flow);
            state.parse = ParseStatus::Running {
                stage: ParseStage::Story,
                flow,
            };
            assert_eq!(state.parse_text(), "载入中 · 剧情 2/5 · 20%");
            assert!(state.restoring());
        }
        for flow in [ParseFlow::Parse, ParseFlow::Rebuild] {
            state.begin_parse(flow);
            assert!(!state.restoring());
        }
    }

    #[test]
    fn each_flow_uses_its_cache_policy() {
        assert_eq!(ParseFlow::Parse.policy(), CachePolicy::Current);
        assert_eq!(ParseFlow::Rebuild.policy(), CachePolicy::Rebuild);
        assert_eq!(ParseFlow::Restore.policy(), CachePolicy::Current);
        assert_eq!(ParseFlow::RestoreStale.policy(), CachePolicy::AcceptStale);
    }

    #[test]
    fn a_stale_restore_ends_with_an_update_prompt_and_nothing_else_does() {
        for (flow, prompted) in [
            (ParseFlow::Parse, false),
            (ParseFlow::Rebuild, false),
            (ParseFlow::Restore, false),
            (ParseFlow::RestoreStale, true),
        ] {
            let mut state = AppState::with_environment(None, PathBuf::from("out"));
            state.begin_parse(flow);
            state.complete_parse(113);
            assert_eq!(
                state.parse,
                ParseStatus::Done {
                    characters: 113,
                    update_available: prompted
                }
            );
            assert_eq!(state.update_available(), prompted);
            assert!(state.logs_text(LogFilter::All).contains("用时"));
        }
    }

    #[test]
    fn stages_keep_the_flow_they_started_with() {
        let mut state = parse_state();
        state.begin_parse(ParseFlow::RestoreStale);
        state.parse_stage_pending = false;
        finish_task(&mut state, TaskState::Finished);
        assert!(state.finish_parse_stage());
        assert_eq!(
            state.parse,
            ParseStatus::Running {
                stage: ParseStage::Story,
                flow: ParseFlow::RestoreStale
            }
        );
    }

    #[test]
    fn stopping_a_restore_returns_to_not_parsed() {
        let mut state = parse_state();
        state.begin_parse(ParseFlow::Restore);
        state.parse_stage_pending = false;
        finish_task(&mut state, TaskState::Stopped);
        assert!(!state.finish_parse_stage());
        assert_eq!(state.parse, ParseStatus::NotParsed);
    }

    fn tally(stopped: bool, counts: [usize; 5], rows: &[(Category, usize)]) -> ExportTally {
        ExportTally {
            stopped,
            succeeded: counts[0],
            reused: counts[1],
            failed: counts[2],
            warning_tracks: counts[3],
            wavs: counts[4],
            by_category: rows
                .iter()
                .map(|&(category, count)| (category, crate::catalog::Language::Common, count))
                .collect(),
        }
    }

    #[test]
    fn merges_batch_tallies() {
        let one = tally(false, [1, 0, 0, 0, 1], &[(Category::Music, 1)]);
        assert_eq!(ExportTally::merge(std::slice::from_ref(&one)), one);
        let merged = ExportTally::merge(&[
            tally(false, [2, 1, 0, 3, 40], &[(Category::Character, 40)]),
            tally(true, [1, 0, 1, 0, 2], &[(Category::Other, 2)]),
        ]);
        assert_eq!(
            merged,
            tally(
                true,
                [3, 1, 1, 3, 42],
                &[(Category::Character, 40), (Category::Other, 2)]
            )
        );
    }

    #[test]
    fn export_summaries_render_in_both_languages() {
        let finished = tally(false, [1, 0, 0, 0, 1], &[(Category::Music, 1)]);
        let stopped = tally(true, [7, 1, 0, 1200, 4600], &[]);
        with_language(Language::Chinese, || {
            assert_eq!(
                finished.log_text(),
                "[执行摘要] 成功=1；复用=0；失败=0；密钥警告音轨=0；共 1 个 WAV\n  背景音乐 / Common：1 个 WAV"
            );
            assert_eq!(
                finished.card_text(),
                "成功 1 · 复用 0 · 失败 0 · 警告音轨 0 · 1 个 WAV"
            );
            assert_eq!(
                stopped.card_text(),
                "成功 7 · 复用 1 · 失败 0 · 警告音轨 1,200 · 4,600 个 WAV"
            );
            assert!(stopped.log_text().starts_with("[已停止]"));
        });
        with_language(Language::English, || {
            assert_eq!(
                finished.log_text(),
                "[Summary] Succeeded=1; Reused=0; Failed=0; Key-warning tracks=0; 1 WAV in total\n  Music / Common: 1 WAV"
            );
            assert_eq!(
                finished.card_text(),
                "Succeeded 1 · Reused 0 · Failed 0 · Warning tracks 0 · 1 WAV"
            );
            assert_eq!(
                stopped.card_text(),
                "Succeeded 7 · Reused 1 · Failed 0 · Warning tracks 1,200 · 4,600 WAV"
            );
            assert!(stopped.log_text().starts_with("[Stopped]"));
        });
    }
    use std::path::Path;

    fn state_with(names: &[&str]) -> AppState {
        let mut state = AppState::with_environment(None, PathBuf::from("out"));
        state.game_directory = "game".to_owned();
        state.scanned_root = Some(PathBuf::from("game"));
        state.sources = names
            .iter()
            .map(|name| SourceInfo::from_path(Path::new(name), 100).unwrap())
            .collect();
        state
    }

    #[test]
    fn refuses_export_without_disclaimer() {
        let mut state = state_with(&["BGM_Agent_Battle.awb"]);
        let sources = state.sources.clone();
        assert!(state.start_audio_export(sources, None).is_none());
        assert!(!state.running);
        assert!(
            state
                .logs
                .last()
                .unwrap()
                .text
                .contains("请阅读并同意使用声明")
        );
    }

    #[test]
    fn tracks_scan_and_export_messages() {
        let mut state = state_with(&[]);
        state.running = true;
        state.tasks.push(TaskRecord {
            kind: TaskKind::Scan,
            title: String::new(),
            state: TaskState::Running,
            done: 0,
            total: 0,
            current: None,
            detail: String::new(),
            decoder_warnings: 0,
            failures: 0,
        });
        assert!(state.is_scanning());
        let sources: Vec<_> = ["BGM_Agent_Battle.awb", "BGM_Other.awb"]
            .iter()
            .map(|name| SourceInfo::from_path(Path::new(name), 100).unwrap())
            .collect();
        state.handle(WorkerMessage::Scanned(ScanReport {
            sources: sources.clone(),
            warnings: vec!["w".to_owned()],
            other_files: std::collections::BTreeMap::from([("usm".to_owned(), 3)]),
            metadata_only_acb: 4,
            superseded: 1,
            cancelled: false,
        }));
        assert!(!state.running);
        assert_eq!(state.tasks[0].state, TaskState::Finished);
        assert_eq!(state.scan_stats.as_ref().unwrap().metadata_only_acb, 4);
        assert_eq!(state.sources.len(), 2);

        state.running = true;
        state.export_batch = sources.iter().map(|source| source.path.clone()).collect();
        state.tasks.push(TaskRecord {
            kind: TaskKind::Export,
            title: String::new(),
            state: TaskState::Running,
            done: 0,
            total: 2,
            current: None,
            detail: String::new(),
            decoder_warnings: 0,
            failures: 0,
        });
        state.handle(WorkerMessage::DecoderWarning { tracks: 3 });
        state.handle(WorkerMessage::Decoded {
            reused: false,
            wav_count: 1,
            directory: PathBuf::from("x"),
        });
        state.handle(WorkerMessage::Progress { done: 1, total: 2 });
        state.handle(WorkerMessage::DecodeFailed {
            path: PathBuf::from("x"),
            error: "boom".to_owned(),
        });
        state.handle(WorkerMessage::Progress { done: 2, total: 2 });
        state.handle(WorkerMessage::Finished(ExportTally::default()));
        assert_eq!(state.row_status[&sources[0].path], RowStatus::Warning(3));
        assert_eq!(state.row_status[&sources[1].path], RowStatus::Failed);
        let task = state.tasks.last().unwrap();
        assert_eq!(task.state, TaskState::Finished);
        assert_eq!((task.decoder_warnings, task.failures), (3, 1));
        assert_eq!(state.logs_text(LogFilter::Warnings).lines().count(), 2);
        assert!(state.logs_text(LogFilter::Errors).contains("[解码失败]"));
        state.worker_disconnected();
        assert_eq!(state.tasks.last().unwrap().state, TaskState::Finished);
    }
}
