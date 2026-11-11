use super::{
    EchoArchive,
    state::{LogFilter, LogLevel, TaskKind, TaskRecord, TaskState},
    theme::*,
    widgets::{chip, fmt_count, page_scroll, soft_button, truncated},
};
use gpui::{
    AnyElement, ClipboardItem, Context, FontWeight, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled, Window, div, list, prelude::FluentBuilder, px,
};
use gpui_component::progress::Progress;
use std::path::PathBuf;

/// Task column width from the design; narrower windows scale it down to the minimum.
const QUEUE_MAX_WIDTH: f32 = 520.;
const QUEUE_MIN_WIDTH: f32 = 360.;
const LOG_MIN_WIDTH: f32 = 400.;
const COLUMN_GAP: f32 = 16.;
/// Log height when the queue and log are stacked in a narrow window.
const STACKED_LOG_HEIGHT: f32 = 420.;
/// The page's horizontal padding (2 × 28).
const PAGE_PADDING: f32 = 56.;

fn level_color(level: LogLevel) -> Color {
    match level {
        LogLevel::Heading => ACCENT,
        LogLevel::Info => TEXT_MUTED,
        LogLevel::Success => SUCCESS_TEXT,
        LogLevel::Warning => WARNING,
        LogLevel::Error => DANGER,
    }
}

impl EchoArchive {
    pub(super) fn render_tasks(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        // Widths are computed in pixels: percentage widths do not resolve
        // reliably inside the page's scroll area.
        let content_width =
            f32::from(window.viewport_size().width) - self.nav_width(window) - PAGE_PADDING;
        let side_queue = (content_width * 0.4).clamp(QUEUE_MIN_WIDTH, QUEUE_MAX_WIDTH);
        let stacked = content_width - side_queue - COLUMN_GAP < LOG_MIN_WIDTH;
        let queue_width = if stacked {
            content_width.max(QUEUE_MIN_WIDTH)
        } else {
            side_queue
        };
        if self.tasks_layout != Some(stacked) {
            self.tasks_layout = Some(stacked);
            self.log_list.scroll_to(gpui::ListOffset {
                item_ix: self.log_rows.len(),
                offset_in_item: px(0.),
            });
        }
        let state = &self.state;

        let output = PathBuf::from(state.output_directory.trim());
        let output_exists = output.is_dir();
        let header = div()
            .flex()
            .items_end()
            .gap(px(12.))
            .child(
                div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .child(
                        div()
                            .text_size(px(22.))
                            .font_weight(FontWeight::BOLD)
                            .child(tr!("任务", "Tasks")),
                    )
                    .child(
                        div()
                            .text_size(px(13.))
                            .text_color(hsla(TEXT_MUTED))
                            .child(tr!("扫描和导出都在后台运行，可以继续浏览其他页面。", "Scans and exports run in the background; you can keep browsing other pages.")),
                    ),
            )
            .child(
                soft_button("open-output", tr!("打开输出目录", "Open output folder"), output_exists)
                    .when(output_exists, move |this| {
                        this.on_click(move |_, _, cx| cx.open_with_system(&output))
                    }),
            );

        let current = match state.current_task() {
            Some(task) => self
                .render_running_task(task, queue_width, cx)
                .into_any_element(),
            None => task_card(
                "task-idle",
                queue_width,
                StatusDot::Hollow,
                tr!("当前没有进行中的任务", "No task is running").to_owned(),
                String::new(),
                if state.tasks.is_empty() {
                    tr!("导出在角色页和音乐与音效页开始；解析在标题栏。", "Exports start on the Characters and Music & SFX pages; parsing starts in the title bar.").to_owned()
                } else {
                    tr!("已完成的任务列在下方。", "Finished tasks are listed below.").to_owned()
                },
            )
            .into_any_element(),
        };

        // One column of equally wide cards, 10px apart (design: Tasks.dc.html).
        let queue = div()
            .w(px(queue_width))
            .flex_shrink_0()
            .flex()
            .flex_col()
            .gap(px(10.))
            .child(current)
            .children(
                state
                    .tasks
                    .iter()
                    .enumerate()
                    .rev()
                    .filter(|(_, task)| task.state != TaskState::Running)
                    .map(|(index, task)| finished_task(index, task, queue_width)),
            )
            .children(
                state
                    .tasks
                    .iter()
                    .rev()
                    .find(|task| task.kind == TaskKind::Export && task.decoder_warnings > 0)
                    .map(|task| warning_panel(task.decoder_warnings, queue_width)),
            );

        let content = div()
            .w_full()
            .flex()
            .flex_col()
            .gap(px(18.))
            .px(px(28.))
            .py(px(24.))
            .when(!stacked, |this| this.min_h(page_min_height(window)))
            .child(header)
            .child(
                div()
                    .flex()
                    .gap_4()
                    .map(|this| {
                        if stacked {
                            this.flex_col()
                        } else {
                            this.flex_1().min_h(px(360.))
                        }
                    })
                    .child(queue)
                    .child(self.render_log_panel(stacked.then_some(content_width), cx)),
            );
        page_scroll("tasks", &self.tasks_scroll, content)
    }

    /// `stacked_width` is set when the log sits below the task column.
    fn render_log_panel(
        &self,
        stacked_width: Option<f32>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        use gpui_component::scroll::ScrollableElement;
        let state = &self.state;
        let filter_button =
            |id: &'static str, label: &'static str, filter: LogFilter, cx: &mut Context<Self>| {
                chip(id, self.log_filter == filter, true)
                    .h(px(26.))
                    .text_size(px(12.))
                    .child(label)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.log_filter = filter;
                        cx.notify();
                    }))
            };
        let rows = self.log_rows.clone();
        div()
            .min_w_0()
            .flex()
            .flex_col()
            .rounded(px(10.))
            .bg(hsla(BG_LOG))
            .overflow_hidden()
            .map(|this| match stacked_width {
                Some(width) => this.w(px(width)).h(px(STACKED_LOG_HEIGHT)),
                // Takes all remaining width beside the task column.
                None => this.flex_1(),
            })
            .child(
                div()
                    .flex()
                    .flex_shrink_0()
                    .items_center()
                    .gap_2()
                    .h(px(46.))
                    .px(px(14.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .items_baseline()
                            .gap_2()
                            .child(
                                div()
                                    .flex_shrink_0()
                                    .text_size(px(13.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(tr!("日志", "Log")),
                            )
                            .child(div().text_size(px(11.)).text_color(hsla(TEXT_FAINT)).child(
                                tr!("{} 条记录", "{} entries", fmt_count(state.logs.len())),
                            )),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_shrink_0()
                            .items_center()
                            .gap(px(2.))
                            .child(filter_button(
                                "log-all",
                                tr!("全部", "All"),
                                LogFilter::All,
                                cx,
                            ))
                            .child(filter_button(
                                "log-warn",
                                tr!("警告", "Warnings"),
                                LogFilter::Warnings,
                                cx,
                            ))
                            .child(filter_button(
                                "log-error",
                                tr!("错误", "Errors"),
                                LogFilter::Errors,
                                cx,
                            )),
                    )
                    .child(
                        soft_button("copy-log", tr!("复制", "Copy"), true)
                            .h(px(26.))
                            .px(px(10.))
                            .text_size(px(12.))
                            .on_click(cx.listener(|this, _, _, cx| {
                                let text = this.state.logs_text(this.log_filter);
                                cx.write_to_clipboard(ClipboardItem::new_string(text));
                            })),
                    )
                    .child({
                        let enabled = !state.running;
                        soft_button("clear-log", tr!("清空", "Clear"), enabled)
                            .h(px(26.))
                            .px(px(10.))
                            .text_size(px(12.))
                            .when(enabled, |this| {
                                this.on_click(cx.listener(|this, _, _, cx| {
                                    this.state.clear_logs();
                                    cx.notify();
                                }))
                            })
                    }),
            )
            .when(self.log_rows.is_empty(), |this| {
                this.child(
                    div()
                        .px(px(14.))
                        .py(px(12.))
                        .text_size(px(12.))
                        .text_color(hsla(TEXT_FAINT))
                        .child(tr!(
                            "没有符合筛选条件的日志。",
                            "No log entries match the filter."
                        )),
                )
            })
            .child(
                div()
                    .id("log-body")
                    .flex_1()
                    .min_h_0()
                    .relative()
                    .child(
                        list(self.log_list.clone(), move |index, _, _| {
                            let (level, text) = &rows[index];
                            mono(
                                div()
                                    .px(px(14.))
                                    .when(*level == LogLevel::Heading, |this| {
                                        this.pt(px(if index == 0 { 4. } else { 12. }))
                                            .pb(px(4.))
                                            .font_weight(FontWeight::SEMIBOLD)
                                    })
                                    .when(*level != LogLevel::Heading, |this| this.py(px(2.)))
                                    .text_size(px(12.))
                                    .line_height(px(19.))
                                    .text_color(hsla(level_color(*level)))
                                    .child(text.clone()),
                            )
                            .into_any_element()
                        })
                        .size_full()
                        .py_2(),
                    )
                    .vertical_scrollbar(&self.log_list),
            )
    }

    fn render_running_task(
        &self,
        task: &TaskRecord,
        width: f32,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let state = &self.state;
        let stop_requested = state.stop_requested();
        let (counter, current_label, stop_label) = match task.kind {
            TaskKind::Scan => (
                tr!("进行中", "Running").to_owned(),
                tr!("正在扫描", "Scanning"),
                if stop_requested {
                    tr!("正在停止…", "Stopping…")
                } else {
                    tr!("停止扫描", "Stop scanning")
                },
            ),
            TaskKind::Export => (
                format!("{} / {}", fmt_count(task.done), fmt_count(task.total)),
                tr!("正在解码", "Decoding"),
                if stop_requested {
                    tr!("将在当前包完成后停止", "Stopping after the current pack")
                } else {
                    tr!("完成当前包后停止", "Stop after the current pack")
                },
            ),
            TaskKind::StoryIndex => (
                if task.total > 0 {
                    format!("{} / {}", fmt_count(task.done), fmt_count(task.total))
                } else {
                    tr!("进行中", "Running").to_owned()
                },
                tr!("当前步骤", "Current step"),
                if stop_requested {
                    tr!("正在停止…", "Stopping…")
                } else {
                    tr!("停止建立索引", "Stop indexing")
                },
            ),
            TaskKind::StoryExport => (
                tr!("进行中", "Running").to_owned(),
                tr!("正在导出", "Exporting"),
                if stop_requested {
                    tr!(
                        "将在当前音频包完成后停止",
                        "Stopping after the current audio pack"
                    )
                } else {
                    tr!("停止导出", "Stop exporting")
                },
            ),
            TaskKind::PortraitIndex => (
                if task.total > 0 {
                    format!("{} / {}", fmt_count(task.done), fmt_count(task.total))
                } else {
                    tr!("进行中", "Running").to_owned()
                },
                tr!("当前步骤", "Current step"),
                if stop_requested {
                    tr!("正在停止…", "Stopping…")
                } else {
                    tr!("停止建立索引", "Stop indexing")
                },
            ),
            TaskKind::PortraitExport => (
                format!("{} / {}", fmt_count(task.done), fmt_count(task.total)),
                tr!("正在导出", "Exporting"),
                if stop_requested {
                    tr!(
                        "将在当前立绘完成后停止",
                        "Stopping after the current portrait"
                    )
                } else {
                    tr!("停止导出", "Stop exporting")
                },
            ),
            TaskKind::Live2dIndex => (
                if task.total > 0 {
                    format!("{} / {}", fmt_count(task.done), fmt_count(task.total))
                } else {
                    tr!("进行中", "Running").to_owned()
                },
                tr!("当前步骤", "Current step"),
                if stop_requested {
                    tr!("正在停止…", "Stopping…")
                } else {
                    tr!("停止建立索引", "Stop indexing")
                },
            ),
            TaskKind::Roster => (
                tr!("进行中", "Running").to_owned(),
                tr!("当前步骤", "Current step"),
                if stop_requested {
                    tr!("正在停止…", "Stopping…")
                } else {
                    tr!("停止解析", "Stop parsing")
                },
            ),
            TaskKind::Live2dExport => (
                format!("{} / {}", fmt_count(task.done), fmt_count(task.total)),
                tr!("正在导出", "Exporting"),
                if stop_requested {
                    tr!("将在当前模型完成后停止", "Stopping after the current model")
                } else {
                    tr!("停止导出", "Stop exporting")
                },
            ),
        };
        let progress = state.progress * 100.;
        card_frame(width)
            .bg(hsla(BG_SELECTED))
            .gap(px(12.))
            .child(card_header(
                "running-title",
                StatusDot::Filled(ACCENT),
                task.title.clone(),
                counter,
            ))
            .when(
                matches!(
                    task.kind,
                    TaskKind::Export
                        | TaskKind::StoryIndex
                        | TaskKind::PortraitIndex
                        | TaskKind::PortraitExport
                        | TaskKind::Live2dIndex
                        | TaskKind::Live2dExport
                ),
                |this| {
                    this.child(
                        Progress::new("task-progress")
                            .value(progress)
                            .h(px(6.))
                            .bg(hsla(ACCENT)),
                    )
                },
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_size(px(12.))
                    .text_color(hsla(TEXT_MUTED))
                    .child(div().flex_shrink_0().child(current_label))
                    .child(mono(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_color(hsla(TEXT_SECONDARY))
                            .child(truncated(
                                "running-current",
                                task.current.clone().unwrap_or_default(),
                            )),
                    ))
                    .child(
                        div()
                            .id("stop-task")
                            .flex()
                            .flex_shrink_0()
                            .items_center()
                            .h(px(30.))
                            .px(px(12.))
                            .rounded(px(8.))
                            .text_size(px(12.))
                            .text_color(hsla(if stop_requested { TEXT_FAINT } else { DANGER }))
                            .bg(hsla(BG_DANGER))
                            .when(!stop_requested, |this| {
                                this.cursor_pointer()
                                    .hover(|style| style.bg(hsla(BG_DANGER_HOVER)))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.state.request_stop();
                                        cx.notify();
                                    }))
                            })
                            .child(stop_label),
                    ),
            )
            .when(task.decoder_warnings > 0 || task.failures > 0, |this| {
                this.child(
                    div()
                        .text_size(px(12.))
                        .text_color(hsla(WARNING))
                        .child(tr!(
                            "密钥警告音轨 {} · 失败 {}",
                            "Key-warning tracks {} · Failed {}",
                            fmt_count(task.decoder_warnings),
                            fmt_count(task.failures)
                        )),
                )
            })
    }
}

#[derive(Clone, Copy)]
enum StatusDot {
    Filled(Color),
    Hollow,
}

/// Card shell shared by every entry in the task column: the column's width,
/// same padding and radius.
fn card_frame(width: f32) -> gpui::Div {
    div()
        .rounded(px(10.))
        .bg(hsla(BG_CARD))
        .w(px(width))
        .flex_shrink_0()
        .flex()
        .flex_col()
        .gap(px(6.))
        .px(px(16.))
        .py(px(14.))
}

/// First card row: status dot, title (fills the row, ellipsis when long) and
/// right-aligned status text.
fn card_header(
    id: impl Into<gpui::ElementId>,
    dot: StatusDot,
    title: String,
    status: String,
) -> impl IntoElement {
    div()
        .w_full()
        .flex()
        .items_center()
        .gap(px(10.))
        .child(match dot {
            StatusDot::Filled(color) => div()
                .size(px(8.))
                .rounded_full()
                .flex_shrink_0()
                .bg(hsla(color)),
            StatusDot::Hollow => div()
                .size(px(8.))
                .rounded_full()
                .flex_shrink_0()
                .border_1()
                .border_color(hsla(TEXT_FAINT)),
        })
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_size(px(14.))
                .child(truncated(id, title)),
        )
        .child(
            div()
                .flex_shrink_0()
                .text_size(px(12.))
                .text_color(hsla(TEXT_MUTED))
                .child(status),
        )
}

/// Second card row: detail text indented under the title.
fn card_detail(id: impl Into<gpui::ElementId>, detail: String) -> impl IntoElement {
    div()
        .w_full()
        .pl(px(18.))
        .text_size(px(12.))
        .text_color(hsla(TEXT_FAINT))
        .child(truncated(id, detail))
}

fn task_card(
    id: &'static str,
    width: f32,
    dot: StatusDot,
    title: String,
    status: String,
    detail: String,
) -> gpui::Div {
    card_frame(width)
        .child(card_header(
            gpui::SharedString::from(format!("{id}-title")),
            dot,
            title,
            status,
        ))
        .child(card_detail(
            gpui::SharedString::from(format!("{id}-detail")),
            detail,
        ))
}

fn finished_task(index: usize, task: &TaskRecord, width: f32) -> AnyElement {
    let (color, status) = match task.state {
        TaskState::Finished if task.failures > 0 => (
            DANGER_DOT,
            tr!("{} 个失败", "{} failed", fmt_count(task.failures)),
        ),
        TaskState::Finished if task.decoder_warnings > 0 => (
            WARNING,
            tr!("{} 条警告", "{} warnings", fmt_count(task.decoder_warnings)),
        ),
        TaskState::Finished => (SUCCESS, tr!("已完成", "Finished").to_owned()),
        TaskState::Stopped => (TEXT_FAINT, tr!("已停止", "Stopped").to_owned()),
        TaskState::Failed => (DANGER_DOT, tr!("异常结束", "Ended abnormally").to_owned()),
        TaskState::Running => (ACCENT, tr!("进行中", "Running").to_owned()),
    };
    card_frame(width)
        .child(card_header(
            ("task-title", index),
            StatusDot::Filled(color),
            task.title.clone(),
            status,
        ))
        .child(card_detail(("task-detail", index), task.detail.clone()))
        .into_any_element()
}

fn warning_panel(count: usize, width: f32) -> impl IntoElement {
    div()
        .w(px(width))
        .flex_shrink_0()
        .flex()
        .flex_col()
        .gap_2()
        .px(px(16.))
        .py(px(14.))
        .rounded(px(10.))
        .bg(hsla(BG_WARN_PANEL))
        .child(
            div()
                .text_size(px(13.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(hsla(WARNING))
                .child(tr!("需要核查的音轨", "Tracks to check")),
        )
        .child(
            div()
                .text_size(px(12.))
                .line_height(px(19.))
                .text_color(hsla(WARNING_BODY))
                .child(tr!("最近一次导出有 {} 条音轨需要核查，通常是静音占位。文件已保留，并在每包的 audio-index.json 中标记 decoder_warning。", "The last export has {} tracks to check, usually silent placeholders. The files were kept and are marked decoder_warning in each pack's audio-index.json.",
                    fmt_count(count))),
        )
}

/// Minimum height for content that should fill the window below the title
/// bar; taller content makes the page scroll instead.
fn page_min_height(window: &gpui::Window) -> gpui::Pixels {
    (window.viewport_size().height - px(super::TITLE_BAR_HEIGHT)).max(px(0.))
}
