//! Portrait index and export tasks, and the thumbnail workers shared by the
//! character page (portrait cards, header art) and Live2D texture previews.

use super::{
    EchoArchive, fmt_count,
    state::{AppState, LogEntry, TaskKind, TaskRecord, TaskState},
    theme::*,
};
use crate::{
    discovery::validate_directories,
    portrait::{
        self, Entry, ExportOptions, ExportSummary, FileOutcome, IndexProgress, PortraitIndex,
        index::Portrait,
    },
    story::CachePolicy,
};
use gpui::{AnyElement, Bounds, Context, IntoElement, Styled, Task, canvas, point, px, size};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        Arc, Condvar, Mutex,
        atomic::AtomicBool,
        mpsc::{self, Receiver, Sender},
    },
    thread,
};

// ---------------------------------------------------------------------------
// Loaded index and background tasks.

pub struct PortraitData {
    pub index: PortraitIndex,
}

impl PortraitData {
    pub fn new(index: PortraitIndex) -> Self {
        Self { index }
    }
}

pub enum PortraitMessage {
    Stage(String),
    Progress {
        done: usize,
        total: usize,
        current: Option<String>,
    },
    Indexed(Result<Option<Arc<PortraitData>>, String>),
    Exported(Result<ExportSummary, String>),
}

impl AppState {
    /// The loaded portrait index, if it belongs to the current game directory.
    pub fn portrait_data(&self) -> Option<&Arc<PortraitData>> {
        let root = PathBuf::from(self.game_directory.trim());
        self.portrait
            .as_ref()
            .filter(|_| self.portrait_root.as_ref() == Some(&root))
    }

    pub(super) fn begin_task(
        &mut self,
        kind: TaskKind,
        title: String,
        current: String,
        detail: String,
    ) {
        self.push_heading(&title);
        self.running = true;
        self.progress = 0.0;
        self.progress_label.clear();
        self.tasks.push(TaskRecord {
            kind,
            title,
            state: TaskState::Running,
            done: 0,
            total: 0,
            current: Some(current),
            detail,
            decoder_warnings: 0,
            failures: 0,
        });
    }

    /// Builds (or loads the cached) portrait index in the background.
    pub fn start_portrait_index(
        &mut self,
        policy: CachePolicy,
    ) -> Option<Receiver<PortraitMessage>> {
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
            tr!("重新建立立绘索引", "Rebuild the portrait index")
        } else {
            tr!("建立立绘索引", "Build the portrait index")
        };
        self.begin_task(
            TaskKind::PortraitIndex,
            title.to_owned(),
            tr!("准备中", "Preparing").to_owned(),
            String::new(),
        );
        self.progress_label = tr!("正在建立立绘索引…", "Building the portrait index…").to_owned();
        self.push_log(LogEntry::normal(tr!(
            "[立绘索引] {}",
            "[Portrait index] {}",
            root.display()
        )));
        thread::spawn(move || {
            let progress = |event: IndexProgress| {
                let message = match event {
                    IndexProgress::Stage(stage) => PortraitMessage::Stage(stage),
                    IndexProgress::Bundles { done, total } => {
                        if done != total && done % (total / 200).max(1) != 0 {
                            return;
                        }
                        PortraitMessage::Progress {
                            done,
                            total,
                            current: None,
                        }
                    }
                };
                let _ = sender.send(message);
            };
            let result = portrait::load_with_policy(&root, policy, &cancel, &progress)
                .map(|index| index.map(|index| Arc::new(PortraitData::new(index))))
                .map_err(|error| format!("{error:#}"));
            let _ = sender.send(PortraitMessage::Indexed(result));
        });
        Some(receiver)
    }

    /// Exports the given entries (indices into the loaded index).
    pub fn start_portrait_export(
        &mut self,
        indices: Vec<usize>,
        jpg: bool,
    ) -> Option<Receiver<PortraitMessage>> {
        if self.running {
            return None;
        }
        let Some(data) = self.portrait_data().cloned() else {
            self.append_error(tr!(
                "请先建立立绘索引。",
                "Please build the portrait index first."
            ));
            return None;
        };
        if indices.is_empty() {
            self.append_error(tr!(
                "请先选择要导出的立绘。",
                "Please select the portraits to export first."
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
        let title = tr!(
            "导出立绘 · {} 张{}",
            "Export portraits · {}{}",
            fmt_count(indices.len()),
            if jpg {
                tr!(" · 含 JPG 预览", " · with JPG previews")
            } else {
                ""
            }
        );
        self.begin_task(
            TaskKind::PortraitExport,
            title,
            tr!("准备中", "Preparing").to_owned(),
            output.display().to_string(),
        );
        let options = ExportOptions {
            output,
            jpg,
            overwrite: self.overwrite,
        };
        thread::spawn(move || {
            let entries: Vec<&Entry> = indices
                .iter()
                .filter_map(|index| data.index.entries.get(*index))
                .collect();
            let progress = |done: usize, total: usize, name: &str| {
                let _ = sender.send(PortraitMessage::Progress {
                    done,
                    total,
                    current: (!name.is_empty()).then(|| name.to_owned()),
                });
            };
            let result = portrait::export(&data.index, &entries, &options, &cancel, &progress)
                .map_err(|error| format!("{error:#}"));
            let _ = sender.send(PortraitMessage::Exported(result));
        });
        Some(receiver)
    }

    pub fn handle_portrait(&mut self, message: PortraitMessage) {
        match message {
            PortraitMessage::Stage(stage) => {
                if let Some(task) = self.tasks.last_mut() {
                    task.current = Some(stage.clone());
                }
                self.push_log(LogEntry::normal(tr!(
                    "[立绘索引] {stage}",
                    "[Portrait index] {stage}",
                    stage = stage
                )));
            }
            PortraitMessage::Progress {
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
            PortraitMessage::Indexed(result) => {
                self.running = false;
                let (state, detail) = match result {
                    Ok(Some(data)) => {
                        let summary = &data.index.summary;
                        let detail = tr!(
                            "{} 张立绘 · {} 个角色 · 不支持 {} · 特殊打包 {} · {}",
                            "{} portraits · {} characters · Unsupported {} · Special packing {} · {}",
                            fmt_count(summary.portraits),
                            fmt_count(summary.characters),
                            fmt_count(summary.unsupported),
                            fmt_count(summary.packing),
                            if summary.from_cache {
                                tr!("命中缓存", "from cache")
                            } else {
                                tr!("已重建", "rebuilt")
                            }
                        );
                        self.push_log(LogEntry::success(tr!(
                            "[完成] 立绘索引：{}；用时 {:.1} 秒",
                            "[Done] Portrait index: {}; took {:.1} s",
                            detail,
                            summary.elapsed_ms as f64 / 1000.0
                        )));
                        for sample in &summary.unsupported_samples {
                            self.push_log(LogEntry::warning(tr!(
                                "[立绘不支持] {} ({}#{})：{}",
                                "[Portrait unsupported] {} ({}#{}): {}",
                                sample.name,
                                sample.source_bundle,
                                sample.path_id,
                                sample.detail
                            )));
                        }
                        self.log_scan_warnings(
                            &data.index.scan_warnings,
                            summary.scan_warnings,
                            summary.from_cache,
                        );
                        self.portrait_root = Some(data.index.game_root.clone());
                        self.portrait = Some(data);
                        (TaskState::Finished, detail)
                    }
                    Ok(None) => {
                        self.push_log(LogEntry::normal(
                            tr!(
                                "[已停止] 立绘索引未完成。",
                                "[Stopped] The portrait index was not finished."
                            )
                            .to_owned(),
                        ));
                        (TaskState::Stopped, tr!("已停止", "Stopped").to_owned())
                    }
                    Err(error) => {
                        self.push_log(LogEntry::error(tr!(
                            "[立绘索引失败] {error}",
                            "[Portrait index failed] {error}",
                            error = error
                        )));
                        (TaskState::Failed, error)
                    }
                };
                self.finish_task(state, detail);
            }
            PortraitMessage::Exported(result) => {
                self.running = false;
                let (state, detail) = match result {
                    Ok(summary) => {
                        for file in &summary.files {
                            let target = file.output.display();
                            self.push_log(match file.outcome {
                                FileOutcome::Written => LogEntry::normal(tr!("[立绘] {} → {target}（{}×{}）", "[Portrait] {} → {target} ({}×{})",
                                    file.name, file.width, file.height, target = target)),
                                FileOutcome::Unchanged => LogEntry::normal(tr!("[立绘] {} 已存在且内容相同，跳过", "[Portrait] {} already exists and is identical; skipped",
                                    file.name)),
                                FileOutcome::Conflict => LogEntry::warning(tr!("[立绘] {target} 已存在且内容不同；在导出面板选择“全部覆盖”后重新导出", "[Portrait] {target} already exists with different content; choose \"Overwrite all\" in the export panel and export again", target = target)),
                                FileOutcome::Failed => LogEntry::error(tr!("[立绘导出失败] {}：{}", "[Portrait export failed] {}: {}",
                                    file.name,
                                    file.error.clone().unwrap_or_default())),
                            });
                        }
                        let detail = tr!(
                            "写入 {} · 内容相同 {} · 未覆盖 {} · 失败 {}",
                            "Written {} · Identical {} · Not overwritten {} · Failed {}",
                            summary.written,
                            summary.unchanged,
                            summary.conflicts,
                            summary.failed
                        );
                        self.push_log(if summary.cancelled {
                            LogEntry::normal(tr!(
                                "[已停止] 立绘导出：{}",
                                "[Stopped] Portrait export: {}",
                                detail
                            ))
                        } else {
                            LogEntry::success(tr!(
                                "[完成] 立绘导出：{}",
                                "[Done] Portrait export: {}",
                                detail
                            ))
                        });
                        let state = if summary.cancelled {
                            TaskState::Stopped
                        } else if summary.failed > 0 {
                            TaskState::Failed
                        } else {
                            TaskState::Finished
                        };
                        if let Some(task) = self.tasks.last_mut() {
                            task.failures = summary.failed + summary.conflicts;
                        }
                        self.portrait_last_export = Some(summary);
                        (state, detail)
                    }
                    Err(error) => {
                        self.push_log(LogEntry::error(tr!(
                            "[立绘导出失败] {error}",
                            "[Portrait export failed] {error}",
                            error = error
                        )));
                        (TaskState::Failed, error)
                    }
                };
                self.finish_task(state, detail);
            }
        }
    }

    pub(super) fn finish_task(&mut self, state: TaskState, detail: String) {
        self.progress_label = detail.clone();
        if let Some(task) = self.tasks.last_mut() {
            task.state = state;
            task.current = None;
            task.detail = detail;
        }
    }
}

// ---------------------------------------------------------------------------
// Thumbnail workers: newest request first, so visible cards win.

pub(super) struct Job {
    pub generation: usize,
    pub index: usize,
    pub bundle_root: PathBuf,
    pub portrait: Portrait,
}

#[derive(Default)]
pub(super) struct ThumbQueue {
    pub jobs: Mutex<Vec<Job>>,
    pub ready: Condvar,
}

pub(super) type ThumbResult = (usize, usize, Result<PathBuf, String>);

pub struct ThumbWorkers {
    queue: Arc<ThumbQueue>,
    _listener: Task<()>,
}

const THUMB_THREADS: usize = 3;

pub(super) fn spawn_workers(queue: &Arc<ThumbQueue>, sender: &Sender<ThumbResult>) {
    for _ in 0..THUMB_THREADS {
        let queue = queue.clone();
        let sender = sender.clone();
        thread::spawn(move || {
            loop {
                let job = {
                    let mut jobs = queue.jobs.lock().expect("thumbnail queue");
                    loop {
                        if let Some(job) = jobs.pop() {
                            break job;
                        }
                        jobs = queue.ready.wait(jobs).expect("thumbnail queue");
                    }
                };
                let result = portrait::thumbnail(&job.bundle_root, &job.portrait)
                    .map_err(|error| format!("{error:#}"));
                if sender.send((job.generation, job.index, result)).is_err() {
                    return;
                }
            }
        });
    }
}

#[derive(Clone, Debug)]
pub enum Thumb {
    Pending,
    Ready(PathBuf),
    Failed(String),
}

/// Background behind transparent pixels in previews.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Backdrop {
    Dark,
    Checker,
    White,
}

/// Thumbnails of the loaded portrait index (by entry index).
#[derive(Default)]
pub struct PortraitThumbs {
    /// Identity of the data the thumbnails belong to.
    pub generation: usize,
    pub thumbs: HashMap<usize, Thumb>,
    pub workers: Option<ThumbWorkers>,
}

pub(super) fn generation_of(data: &Arc<PortraitData>) -> usize {
    Arc::as_ptr(data) as usize
}

const CHECKER_CELL: f32 = 10.;
const CHECKER_DARK: Color = ART_CHECKER_DARK;
const CHECKER_LIGHT: Color = ART_CHECKER_LIGHT;
/// Line segments per corner when a cell has to follow the rounded edge.
const CORNER_SEGMENTS: usize = 16;

type Vertex = (f32, f32);

/// Clockwise (on screen) outline of a rounded rectangle at the origin.
fn rounded_outline(width: f32, height: f32, radius: f32) -> Vec<Vertex> {
    use std::f32::consts::{FRAC_PI_2, PI};
    let centres = [
        (radius, radius, PI),
        (width - radius, radius, PI + FRAC_PI_2),
        (width - radius, height - radius, 0.),
        (radius, height - radius, FRAC_PI_2),
    ];
    let mut outline = Vec::with_capacity((CORNER_SEGMENTS + 1) * 4);
    for (x, y, start) in centres {
        for step in 0..=CORNER_SEGMENTS {
            let angle = start + FRAC_PI_2 * step as f32 / CORNER_SEGMENTS as f32;
            outline.push((x + radius * angle.cos(), y + radius * angle.sin()));
        }
    }
    outline
}

/// The part of a convex polygon inside the convex `outline`
/// (Sutherland–Hodgman).
fn clip_polygon(mut polygon: Vec<Vertex>, outline: &[Vertex]) -> Vec<Vertex> {
    for (index, &from) in outline.iter().enumerate() {
        if polygon.is_empty() {
            break;
        }
        let to = outline[(index + 1) % outline.len()];
        let side = |p: Vertex| (to.0 - from.0) * (p.1 - from.1) - (to.1 - from.1) * (p.0 - from.0);
        let mut kept = Vec::with_capacity(polygon.len() + 1);
        for (position, &current) in polygon.iter().enumerate() {
            let next = polygon[(position + 1) % polygon.len()];
            let (a, b) = (side(current), side(next));
            if a >= 0. {
                kept.push(current);
            }
            if (a >= 0.) != (b >= 0.) {
                let t = a / (a - b);
                kept.push((
                    current.0 + (next.0 - current.0) * t,
                    current.1 + (next.1 - current.1) * t,
                ));
            }
        }
        polygon = kept;
    }
    polygon
}

/// Checkerboard behind transparent pixels. `overflow_hidden` clips to the
/// rectangle only, so the board itself follows the card's rounded corners.
pub(super) fn checker_backdrop(radius: f32) -> AnyElement {
    canvas(
        |bounds, _, _| bounds,
        move |bounds: Bounds<gpui::Pixels>, _, window, _| {
            window.paint_quad(gpui::fill(bounds, hsla(CHECKER_DARK)).corner_radii(px(radius)));
            let (width, height) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
            let outline = rounded_outline(width, height, radius);
            let cols = (width / CHECKER_CELL).ceil() as usize;
            let rows = (height / CHECKER_CELL).ceil() as usize;
            for row in 0..rows {
                for col in (row % 2..cols).step_by(2) {
                    let (x, y) = (col as f32 * CHECKER_CELL, row as f32 * CHECKER_CELL);
                    let (right, bottom) = (
                        (x + CHECKER_CELL).min(width),
                        (y + CHECKER_CELL).min(height),
                    );
                    let in_corner = (x < radius || right > width - radius)
                        && (y < radius || bottom > height - radius);
                    if !in_corner {
                        let cell = Bounds {
                            origin: bounds.origin + point(px(x), px(y)),
                            size: size(px(right - x), px(bottom - y)),
                        };
                        window.paint_quad(gpui::fill(cell, hsla(CHECKER_LIGHT)));
                        continue;
                    }
                    let clipped = clip_polygon(
                        vec![(x, y), (right, y), (right, bottom), (x, bottom)],
                        &outline,
                    );
                    let Some(((first_x, first_y), rest)) = clipped.split_first() else {
                        continue;
                    };
                    let at = |px_x: f32, px_y: f32| bounds.origin + point(px(px_x), px(px_y));
                    let mut builder = gpui::PathBuilder::fill();
                    builder.move_to(at(*first_x, *first_y));
                    for (vertex_x, vertex_y) in rest {
                        builder.line_to(at(*vertex_x, *vertex_y));
                    }
                    builder.close();
                    if let Ok(path) = builder.build() {
                        window.paint_path(path, hsla(CHECKER_LIGHT));
                    }
                }
            }
        },
    )
    .size_full()
    .into_any_element()
}

impl EchoArchive {
    pub(super) fn start_portrait_index(
        &mut self,
        policy: CachePolicy,
        cx: &mut Context<Self>,
    ) -> bool {
        let receiver = self.state.start_portrait_index(policy);
        let started = receiver.is_some();
        if let Some(receiver) = receiver {
            self.listen_with(receiver, AppState::handle_portrait, cx);
        }
        cx.notify();
        started
    }

    pub(super) fn start_portrait_export(
        &mut self,
        indices: Vec<usize>,
        jpg: bool,
        cx: &mut Context<Self>,
    ) {
        if let Some(receiver) = self.state.start_portrait_export(indices, jpg) {
            self.listen_with(receiver, AppState::handle_portrait, cx);
        }
        cx.notify();
    }

    /// Drops thumbnails when a different index was loaded.
    pub(super) fn sync_portrait(&mut self) {
        let generation = self.state.portrait_data().map_or(0, generation_of);
        if self.portrait.generation != generation {
            self.portrait.generation = generation;
            self.portrait.thumbs.clear();
            if let Some(workers) = &self.portrait.workers {
                workers.queue.jobs.lock().expect("thumbnail queue").clear();
            }
        }
    }

    fn ensure_thumb_workers(&mut self, cx: &mut Context<Self>) {
        if self.portrait.workers.is_some() {
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
                    for (generation, index, result) in batch {
                        if generation == this.portrait.generation {
                            this.portrait.thumbs.insert(
                                index,
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
        self.portrait.workers = Some(ThumbWorkers {
            queue,
            _listener: listener,
        });
    }

    /// The thumbnail of a portrait entry, queued (newest first) when not
    /// known yet.
    pub(super) fn portrait_thumb(&mut self, index: usize, cx: &mut Context<Self>) -> Thumb {
        if let Some(thumb) = self.portrait.thumbs.get(&index) {
            return thumb.clone();
        }
        let Some(data) = self.state.portrait_data().cloned() else {
            return Thumb::Pending;
        };
        let Some(entry) = data.index.entries.get(index) else {
            return Thumb::Failed(tr!("没有这张立绘", "This portrait does not exist").to_owned());
        };
        if let Some(reason) = &entry.portrait.unsupported {
            let thumb = Thumb::Failed(reason.text());
            self.portrait.thumbs.insert(index, thumb.clone());
            return thumb;
        }
        self.ensure_thumb_workers(cx);
        self.portrait.thumbs.insert(index, Thumb::Pending);
        let workers = self.portrait.workers.as_ref().expect("workers started");
        workers
            .queue
            .jobs
            .lock()
            .expect("thumbnail queue")
            .push(Job {
                generation: self.portrait.generation,
                index,
                bundle_root: data.index.bundle_root.clone(),
                portrait: entry.portrait.clone(),
            });
        workers.queue.ready.notify_one();
        Thumb::Pending
    }
}

#[cfg(test)]
mod checker_tests {
    use super::*;

    const SIDE: f32 = 100.;
    const RADIUS: f32 = 16.;

    fn inside_rounded(point: Vertex) -> bool {
        let (x, y) = point;
        let dx = (RADIUS - x).max(x - (SIDE - RADIUS));
        let dy = (RADIUS - y).max(y - (SIDE - RADIUS));
        if dx > 0. && dy > 0. {
            (dx * dx + dy * dy).sqrt() <= RADIUS + 0.01
        } else {
            (-0.01..=SIDE + 0.01).contains(&x) && (-0.01..=SIDE + 0.01).contains(&y)
        }
    }

    #[test]
    fn cells_stay_inside_the_rounded_corners() {
        let outline = rounded_outline(SIDE, SIDE, RADIUS);
        for row in 0..10 {
            for col in 0..10 {
                let (x, y) = (col as f32 * 10., row as f32 * 10.);
                let cell = vec![(x, y), (x + 10., y), (x + 10., y + 10.), (x, y + 10.)];
                for vertex in clip_polygon(cell, &outline) {
                    assert!(inside_rounded(vertex), "{vertex:?} of cell {col},{row}");
                }
            }
        }
    }

    #[test]
    fn the_outermost_corner_cell_loses_its_corner_and_inner_cells_stay_whole() {
        let outline = rounded_outline(SIDE, SIDE, RADIUS);
        let corner = clip_polygon(vec![(0., 0.), (10., 0.), (10., 10.), (0., 10.)], &outline);
        assert!(!corner.is_empty());
        assert!(corner.iter().all(|&(x, y)| x + y > 4.));
        let middle = vec![(40., 40.), (50., 40.), (50., 50.), (40., 50.)];
        assert_eq!(clip_polygon(middle.clone(), &outline), middle);
    }
}
