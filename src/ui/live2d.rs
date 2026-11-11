//! Live2D index and export tasks (with the optional Cubism Core check), the
//! texture preview workers and the exported-file tree used by the drawer.

use super::{
    EchoArchive, fmt_count,
    portrait::{Job, Thumb, ThumbQueue, ThumbResult, spawn_workers},
    state::{AppState, LogEntry, TaskKind, TaskState},
    theme::*,
};
use crate::{
    discovery::validate_directories,
    live2d::{
        self, CoreCheck, Entry, ExportOptions, ExportSummary, FileStatus, IndexProgress,
        Live2dIndex, ModelExport, ModelFile, core::CoreReport, manifest::Role,
    },
    portrait::FileOutcome,
    story::CachePolicy,
};
use gpui::{Context, Task};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        Arc,
        atomic::AtomicBool,
        mpsc::{self, Receiver},
    },
    thread,
};

// ---------------------------------------------------------------------------
// Loaded index and background tasks.

pub struct Live2dData {
    pub index: Live2dIndex,
}

pub enum Live2dMessage {
    Stage(String),
    Progress {
        done: usize,
        total: usize,
        current: Option<String>,
    },
    Indexed(Result<Option<Arc<Live2dData>>, String>),
    Exported(Result<ExportSummary, String>),
}

impl AppState {
    /// The loaded Live2D index, if it belongs to the current game directory.
    pub fn live2d_data(&self) -> Option<&Arc<Live2dData>> {
        let root = PathBuf::from(self.game_directory.trim());
        self.live2d
            .as_ref()
            .filter(|_| self.live2d_root.as_ref() == Some(&root))
    }

    /// Builds (or loads the cached) Live2D index in the background.
    pub fn start_live2d_index(&mut self, policy: CachePolicy) -> Option<Receiver<Live2dMessage>> {
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
        let title = if policy == CachePolicy::Rebuild {
            tr!("重新建立 Live2D 索引", "Rebuild the Live2D index")
        } else {
            tr!("建立 Live2D 索引", "Build the Live2D index")
        };
        self.begin_task(
            TaskKind::Live2dIndex,
            title.to_owned(),
            tr!("准备中", "Preparing").to_owned(),
            String::new(),
        );
        self.progress_label = tr!("正在建立 Live2D 索引…", "Building the Live2D index…").to_owned();
        self.push_log(LogEntry::normal(tr!(
            "[Live2D 索引] {}",
            "[Live2D index] {}",
            root.display()
        )));
        thread::spawn(move || {
            let progress = |event: IndexProgress| {
                let message = match event {
                    IndexProgress::Stage(stage) => Live2dMessage::Stage(stage),
                    IndexProgress::Bundles { done, total } => {
                        if done != total && done % (total / 200).max(1) != 0 {
                            return;
                        }
                        Live2dMessage::Progress {
                            done,
                            total,
                            current: None,
                        }
                    }
                };
                let _ = sender.send(message);
            };
            let result = live2d::load_with_policy(&root, policy, &cancel, &progress)
                .map(|index| index.map(|index| Arc::new(Live2dData { index })))
                .map_err(|error| format!("{error:#}"));
            let _ = sender.send(Live2dMessage::Indexed(result));
        });
        Some(receiver)
    }

    /// Exports the given models (indices into the loaded index).
    pub fn start_live2d_export(
        &mut self,
        indices: Vec<usize>,
        verify_core: bool,
    ) -> Option<Receiver<Live2dMessage>> {
        if self.running {
            return None;
        }
        let Some(data) = self.live2d_data().cloned() else {
            self.append_error(tr!(
                "请先建立 Live2D 索引。",
                "Please build the Live2D index first."
            ));
            return None;
        };
        if indices.is_empty() {
            self.append_error(tr!(
                "请先选择要导出的 Live2D 模型。",
                "Please select the Live2D models to export first."
            ));
            return None;
        }
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
        self.cancel = Arc::new(AtomicBool::new(false));
        let cancel = self.cancel.clone();
        let (sender, receiver) = mpsc::channel();
        let names: Vec<&str> = indices
            .iter()
            .filter_map(|index| data.index.entries.get(*index))
            .map(|entry| entry.model.name.as_str())
            .collect();
        let title = tr!(
            "导出 Live2D · {}{}",
            "Export Live2D · {}{}",
            if names.len() == 1 {
                names[0].to_owned()
            } else {
                tr!("{} 个模型", "{} models", fmt_count(names.len()))
            },
            if verify_core {
                tr!(" · Core 校验", " · Core check")
            } else {
                ""
            }
        );
        self.begin_task(
            TaskKind::Live2dExport,
            title,
            tr!("准备中", "Preparing").to_owned(),
            output.display().to_string(),
        );
        let options = ExportOptions {
            output,
            overwrite: self.overwrite,
            verify_core,
        };
        thread::spawn(move || {
            let entries: Vec<&Entry> = indices
                .iter()
                .filter_map(|index| data.index.entries.get(*index))
                .collect();
            let progress = |done: usize, total: usize, name: &str| {
                let _ = sender.send(Live2dMessage::Progress {
                    done,
                    total,
                    current: (!name.is_empty()).then(|| name.to_owned()),
                });
            };
            let result = live2d::export(&data.index, &entries, &options, &cancel, &progress)
                .map_err(|error| format!("{error:#}"));
            let _ = sender.send(Live2dMessage::Exported(result));
        });
        Some(receiver)
    }

    pub fn handle_live2d(&mut self, message: Live2dMessage) {
        match message {
            Live2dMessage::Stage(stage) => {
                if let Some(task) = self.tasks.last_mut() {
                    task.current = Some(stage.clone());
                }
                self.push_log(LogEntry::normal(tr!(
                    "[Live2D 索引] {stage}",
                    "[Live2D index] {stage}",
                    stage = stage
                )));
            }
            Live2dMessage::Progress {
                done,
                total,
                current,
            } => {
                self.progress = if total == 0 {
                    0.0
                } else {
                    done as f32 / total as f32
                };
                self.progress_label = format!("{done} / {total}");
                if let Some(task) = self.tasks.last_mut() {
                    task.done = done;
                    task.total = total;
                    if current.is_some() {
                        task.current = current;
                    }
                }
            }
            Live2dMessage::Indexed(result) => {
                self.running = false;
                let (state, detail) = match result {
                    Ok(Some(data)) => {
                        let detail = self.log_live2d_index(&data);
                        self.live2d_root = Some(data.index.game_root.clone());
                        self.live2d = Some(data);
                        (TaskState::Finished, detail)
                    }
                    Ok(None) => {
                        self.push_log(LogEntry::normal(
                            tr!(
                                "[已停止] Live2D 索引未完成。",
                                "[Stopped] The Live2D index was not finished."
                            )
                            .to_owned(),
                        ));
                        (TaskState::Stopped, tr!("已停止", "Stopped").to_owned())
                    }
                    Err(error) => {
                        self.push_log(LogEntry::error(tr!(
                            "[Live2D 索引失败] {error}",
                            "[Live2D index failed] {error}",
                            error = error
                        )));
                        (TaskState::Failed, error)
                    }
                };
                self.finish_task(state, detail);
            }
            Live2dMessage::Exported(result) => {
                self.running = false;
                let (state, detail) = match result {
                    Ok(summary) => {
                        for model in &summary.models {
                            self.log_model_export(model);
                            if let Some(CoreCheck::Passed { report, .. }) = &model.report.core {
                                self.live2d_core
                                    .insert(model.model.clone(), (**report).clone());
                            }
                        }
                        let detail = tr!(
                            "{} 个模型 · 写入 {} · 内容相同 {} · 未覆盖 {} · 失败 {} · 不完整 {}",
                            "{} models · Written {} · Identical {} · Not overwritten {} · Failed {} · Incomplete {}",
                            summary.models.len(),
                            summary.written,
                            summary.unchanged,
                            summary.conflicts,
                            summary.failed,
                            summary.incomplete
                        );
                        self.push_log(if summary.cancelled {
                            LogEntry::normal(tr!(
                                "[已停止] Live2D 导出：{}",
                                "[Stopped] Live2D export: {}",
                                detail
                            ))
                        } else {
                            LogEntry::success(tr!(
                                "[完成] Live2D 导出：{}",
                                "[Done] Live2D export: {}",
                                detail
                            ))
                        });
                        let state = if summary.cancelled {
                            TaskState::Stopped
                        } else if summary.failed > 0 || summary.core_failed > 0 {
                            TaskState::Failed
                        } else {
                            TaskState::Finished
                        };
                        if let Some(task) = self.tasks.last_mut() {
                            task.failures = summary.failed + summary.conflicts;
                        }
                        self.live2d_last_export = Some(summary);
                        (state, detail)
                    }
                    Err(error) => {
                        self.push_log(LogEntry::error(tr!(
                            "[Live2D 导出失败] {error}",
                            "[Live2D export failed] {error}",
                            error = error
                        )));
                        (TaskState::Failed, error)
                    }
                };
                self.finish_task(state, detail);
            }
        }
    }

    fn log_live2d_index(&mut self, data: &Live2dData) -> String {
        let summary = &data.index.summary;
        let detail = tr!(
            "{} 个模型 · 完整 {} · 不完整 {} · {}",
            "{} models · Complete {} · Incomplete {} · {}",
            fmt_count(summary.models),
            fmt_count(summary.complete),
            fmt_count(summary.incomplete),
            if summary.from_cache {
                tr!("命中缓存", "from cache")
            } else {
                tr!("已重建", "rebuilt")
            }
        );
        self.push_log(LogEntry::success(tr!(
            "[完成] Live2D 索引：{}；用时 {:.1} 秒",
            "[Done] Live2D index: {}; took {:.1} s",
            detail,
            summary.elapsed_ms as f64 / 1000.0
        )));
        for (problem, count) in &summary.incomplete_by_reason {
            self.push_log(LogEntry::warning(tr!(
                "[Live2D 不完整] {}：{} 个模型",
                "[Live2D incomplete] {}: {} models",
                problem.label(),
                fmt_count(*count)
            )));
            for sample in summary.samples.get(problem).into_iter().flatten() {
                self.push_log(LogEntry::warning(format!(
                    "    {} · {}{}",
                    sample.model,
                    sample.file,
                    if sample.detail.is_empty() {
                        String::new()
                    } else {
                        tr!("：{}", ": {}", sample.detail)
                    }
                )));
            }
        }
        self.log_scan_warnings(
            &data.index.scan_warnings,
            summary.scan_warnings,
            summary.from_cache,
        );
        detail
    }

    fn log_model_export(&mut self, model: &ModelExport) {
        let counts = tr!(
            "写入 {} · 内容相同 {} · 未覆盖 {} · 跳过 {} · 失败 {}",
            "Written {} · Identical {} · Not overwritten {} · Skipped {} · Failed {}",
            model.written,
            model.unchanged,
            model.conflicts,
            model.skipped,
            model.failed
        );
        self.push_log(LogEntry::normal(tr!(
            "[Live2D] {} → {}（{counts}）",
            "[Live2D] {} → {} ({counts})",
            model.model,
            model.directory.display(),
            counts = counts
        )));
        if model.conflicts > 0 {
            self.push_log(LogEntry::warning(tr!("[Live2D] {} 中有 {} 个文件已存在且内容不同；在导出面板选择“全部覆盖”后重新导出", "[Live2D] {} has {} existing files with different content; choose \"Overwrite all\" in the export panel and export again",
                model.directory.display(),
                model.conflicts)));
        }
        for file in &model.report.files {
            if file.status != FileStatus::Ok {
                self.push_log(LogEntry::warning(tr!(
                    "[Live2D 未导出] {} · {}{}",
                    "[Live2D not exported] {} · {}{}",
                    file.path,
                    file.status.label(),
                    file.detail
                        .as_ref()
                        .map(|detail| tr!("：{}", ": {}", detail))
                        .unwrap_or_default()
                )));
            } else if file.outcome == Some(FileOutcome::Failed)
                && let Some(detail) = &file.detail
            {
                self.push_log(LogEntry::error(tr!(
                    "[Live2D 导出失败] {}：{detail}",
                    "[Live2D export failed] {}: {detail}",
                    file.path,
                    detail = detail
                )));
            }
        }
        let integrity = &model.report.integrity;
        self.push_log(if model.complete {
            LogEntry::normal(tr!("[Live2D 完整性] {} 个引用齐全 · {} 个 JSON 可解析 · moc3 签名正确", "[Live2D integrity] All {} references present · {} JSON files parse · moc3 signature valid",
                integrity.references, integrity.json_files))
        } else {
            LogEntry::warning(tr!("[Live2D 完整性] 不完整：缺少 {} · JSON 无法解析 {} · moc3 签名{}", "[Live2D integrity] Incomplete: missing {} · unparsable JSON {} · moc3 signature {}",
                if integrity.missing_on_disk.is_empty() {
                    tr!("无", "none").to_owned()
                } else {
                    integrity.missing_on_disk.join(tr!("、", ", "))
                },
                integrity.json_invalid.len(),
                if integrity.moc_signature {
                    tr!("正确", "valid")
                } else {
                    tr!("无效", "invalid")
                }))
        });
        match &model.report.core {
            None => {}
            Some(CoreCheck::Passed {
                report, ids_match, ..
            }) => self.push_log(if *ids_match {
                LogEntry::normal(format!("[Cubism Core] {}", core_line(report)))
            } else {
                LogEntry::warning(tr!("[Cubism Core] {}；表情或动作中有模型不存在的参数/部件 ID，详见 export-report.json", "[Cubism Core] {}; expressions or motions use parameter or part IDs the model lacks, see export-report.json",
                    core_line(report)))
            }),
            Some(CoreCheck::Skipped { reason }) => {
                self.push_log(LogEntry::warning(tr!("[Cubism Core] 已跳过：{reason}", "[Cubism Core] Skipped: {reason}", reason = reason)))
            }
            Some(CoreCheck::Failed { error }) => {
                self.push_log(LogEntry::error(tr!("[Cubism Core] 校验失败：{error}", "[Cubism Core] Check failed: {error}", error = error)))
            }
        }
    }
}

pub(super) fn core_line(report: &CoreReport) -> String {
    tr!(
        "参数 {} · 部件 {} · Drawable {} · {}",
        "Parameters {} · Parts {} · Drawables {} · {}",
        report.parameter_count,
        report.part_count,
        report.drawable_count,
        match report.mouth_changed_vertices {
            Some(count) => tr!(
                "嘴型参数改变 {} 个顶点分量",
                "The mouth parameter changed {} vertex components",
                count
            ),
            None => tr!(
                "模型没有 {}",
                "The model has no {}",
                live2d::core::MOUTH_PARAMETER
            ),
        }
    )
}

// ---------------------------------------------------------------------------
// View state and thumbnail workers (same workers and cache as the portrait page).

pub struct Live2dWorkers {
    queue: Arc<ThumbQueue>,
    _listener: Task<()>,
}

/// Texture previews of the loaded Live2D index.
#[derive(Default)]
pub struct Live2dThumbs {
    pub generation: usize,
    /// (entry index, texture index) → preview.
    pub thumbs: HashMap<(usize, usize), Thumb>,
    pub workers: Option<Live2dWorkers>,
}

/// Packs (entry, texture) into the worker's single index.
const TEXTURE_SLOTS: usize = 64;

fn generation_of(data: &Arc<Live2dData>) -> usize {
    Arc::as_ptr(data) as usize
}

/// Directory part of a manifest path (`expresstions/a.json` → `expresstions`).
fn directory_of(path: &str) -> Option<&str> {
    path.rsplit_once(['/', '\\']).map(|(dir, _)| dir)
}

pub(super) fn file_name_of(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

/// Root files first, then each directory with its files, in manifest order.
pub(super) fn file_tree(files: &[ModelFile]) -> Vec<(Option<String>, Vec<&ModelFile>)> {
    let mut root = Vec::new();
    let mut directories: Vec<(String, Vec<&ModelFile>)> = Vec::new();
    for file in files {
        match directory_of(&file.path) {
            None => root.push(file),
            Some(directory) => match directories.iter_mut().find(|(d, _)| d == directory) {
                Some((_, list)) => list.push(file),
                None => directories.push((directory.to_owned(), vec![file])),
            },
        }
    }
    std::iter::once((None, root))
        .chain(directories.into_iter().map(|(d, list)| (Some(d), list)))
        .filter(|(_, list)| !list.is_empty())
        .collect()
}

pub(super) fn status_color(status: FileStatus) -> Color {
    match status {
        FileStatus::Ok => TEXT_FAINT,
        FileStatus::Invalid => DANGER,
        _ => WARNING,
    }
}

impl EchoArchive {
    pub(super) fn start_live2d_index(
        &mut self,
        policy: CachePolicy,
        cx: &mut Context<Self>,
    ) -> bool {
        let receiver = self.state.start_live2d_index(policy);
        let started = receiver.is_some();
        if let Some(receiver) = receiver {
            self.listen_with(receiver, AppState::handle_live2d, cx);
        }
        cx.notify();
        started
    }

    pub(super) fn start_live2d_export(
        &mut self,
        indices: Vec<usize>,
        verify_core: bool,
        cx: &mut Context<Self>,
    ) {
        if let Some(receiver) = self.state.start_live2d_export(indices, verify_core) {
            self.listen_with(receiver, AppState::handle_live2d, cx);
        }
        cx.notify();
    }

    /// Drops previews when a different index was loaded.
    pub(super) fn sync_live2d(&mut self) {
        let generation = self.state.live2d_data().map_or(0, generation_of);
        if self.live2d.generation != generation {
            self.live2d.generation = generation;
            self.live2d.thumbs.clear();
            if let Some(workers) = &self.live2d.workers {
                workers.queue.jobs.lock().expect("thumbnail queue").clear();
            }
        }
    }

    fn ensure_live2d_workers(&mut self, cx: &mut Context<Self>) {
        if self.live2d.workers.is_some() {
            return;
        }
        let queue = Arc::new(ThumbQueue::default());
        let (sender, receiver) = mpsc::channel::<ThumbResult>();
        spawn_workers(&queue, &sender);
        let executor = cx.background_executor().clone();
        let listener = cx.spawn(async move |this, cx| {
            let mut receiver = receiver;
            loop {
                let (first, returned) = executor
                    .spawn(async move {
                        let message = receiver.recv();
                        (message, receiver)
                    })
                    .await;
                receiver = returned;
                let Ok(first) = first else { break };
                let mut batch = vec![first];
                while let Ok(message) = receiver.try_recv() {
                    batch.push(message);
                }
                let updated = this.update(cx, |this, cx| {
                    for (generation, slot, result) in batch {
                        if generation == this.live2d.generation {
                            this.live2d.thumbs.insert(
                                (slot / TEXTURE_SLOTS, slot % TEXTURE_SLOTS),
                                match result {
                                    Ok(path) => Thumb::Ready(path),
                                    Err(error) => Thumb::Failed(error),
                                },
                            );
                        }
                    }
                    cx.notify();
                });
                if updated.is_err() {
                    break;
                }
            }
        });
        self.live2d.workers = Some(Live2dWorkers {
            queue,
            _listener: listener,
        });
    }

    /// The preview of one texture of a model, queued when not known yet.
    pub(super) fn live2d_thumb(
        &mut self,
        index: usize,
        texture: usize,
        cx: &mut Context<Self>,
    ) -> Option<Thumb> {
        let key = (index, texture);
        if let Some(thumb) = self.live2d.thumbs.get(&key) {
            return Some(thumb.clone());
        }
        let data = self.state.live2d_data().cloned()?;
        let entry = data.index.entries.get(index)?;
        let source = entry
            .model
            .files
            .iter()
            .filter(|file| file.role == Role::Texture)
            .nth(texture)?
            .texture
            .as_ref()?;
        if texture >= TEXTURE_SLOTS {
            return None;
        }
        self.ensure_live2d_workers(cx);
        self.live2d.thumbs.insert(key, Thumb::Pending);
        let workers = self.live2d.workers.as_ref().expect("workers started");
        workers
            .queue
            .jobs
            .lock()
            .expect("thumbnail queue")
            .push(Job {
                generation: self.live2d.generation,
                index: index * TEXTURE_SLOTS + texture,
                bundle_root: data.index.bundle_root.clone(),
                portrait: live2d::preview_portrait(&entry.model, source),
            });
        workers.queue.ready.notify_one();
        Some(Thumb::Pending)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str) -> ModelFile {
        ModelFile {
            role: Role::Other,
            path: path.into(),
            status: FileStatus::Ok,
            source: None,
            size: None,
            texture: None,
            candidates: 1,
            detail: None,
        }
    }

    #[test]
    fn tree_groups_root_files_then_directories_in_order() {
        let files = [
            file("M.model3.json"),
            file("M.moc3"),
            file("M.2048/texture_00.png"),
            file("expresstions/a.exp3.json"),
            file("M.cdi3.json"),
            file("expresstions/b.exp3.json"),
            file("motions/m.motion3.json"),
        ];
        let tree = file_tree(&files);
        let shape: Vec<(Option<&str>, Vec<&str>)> = tree
            .iter()
            .map(|(dir, list)| {
                (
                    dir.as_deref(),
                    list.iter().map(|f| file_name_of(&f.path)).collect(),
                )
            })
            .collect();
        assert_eq!(
            shape,
            [
                (None, vec!["M.model3.json", "M.moc3", "M.cdi3.json"]),
                (Some("M.2048"), vec!["texture_00.png"]),
                (Some("expresstions"), vec!["a.exp3.json", "b.exp3.json"]),
                (Some("motions"), vec!["m.motion3.json"]),
            ]
        );
    }
}
