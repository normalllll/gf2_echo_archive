//! The parse flow: audio scan, story, portrait and Live2D indexes, then the
//! roster and avatars, one after another. A launch reuses the last finished
//! parse when it can; otherwise the title bar starts the flow. The title bar
//! also stops it, and the data pages stay behind one placeholder until it
//! has finished.

use super::{
    EchoArchive, Page,
    parse_record::{self, ParseRecord, StartupDecision},
    state::{LogEntry, ParseFlow, ParseStatus},
    theme::*,
    widgets::{primary_button, soft_button},
};
use gpui::{
    AnyElement, Context, FontWeight, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled, div, prelude::FluentBuilder, px,
};
use gpui_component::progress::Progress;
use std::path::PathBuf;

impl EchoArchive {
    /// The current game directory exists.
    pub(super) fn game_directory_valid(&self) -> bool {
        let game = self.state.game_directory.trim();
        !game.is_empty() && PathBuf::from(game).is_dir()
    }

    pub(super) fn parsed(&self) -> bool {
        matches!(self.state.parse, ParseStatus::Done { .. })
    }

    /// Decides in the background whether this launch can reuse the last
    /// parse. Runs once, from the constructor.
    pub(super) fn start_startup_check(&mut self, cx: &mut Context<Self>) {
        if !self.game_directory_valid() {
            return;
        }
        let game = PathBuf::from(self.state.game_directory.trim());
        self.state.begin_startup_check();
        let check = cx
            .background_executor()
            .spawn(async move { parse_record::check(&game) });
        cx.spawn(async move |this, cx| {
            let decision = check.await;
            let _ = this.update(cx, |this, cx| this.finish_startup_check(decision, cx));
        })
        .detach();
    }

    /// Stopping or changing the game directory meanwhile drops the result.
    fn finish_startup_check(&mut self, decision: StartupDecision, cx: &mut Context<Self>) {
        if self.state.parse != ParseStatus::Checking {
            return;
        }
        self.state.parse = ParseStatus::NotParsed;
        match decision {
            StartupDecision::NotParsed => {}
            StartupDecision::Restore => self.start_parse(ParseFlow::Restore, cx),
            StartupDecision::RestoreStale => self.start_parse(ParseFlow::RestoreStale, cx),
        }
        cx.notify();
    }

    /// Remembers the finished parse for the next launch. The fingerprint is
    /// the one the story index was built from, so a game update during the
    /// parse still shows up as an update afterwards.
    pub(super) fn save_parse_record(&mut self, characters: usize) {
        let Some(story) = self.state.story_data() else {
            return;
        };
        let record = ParseRecord::new(
            story.index.game_root.clone(),
            story.index.fingerprint.clone(),
            characters,
        );
        if let Err(error) = record.save() {
            self.state.push_log(LogEntry::warning(tr!(
                "[解析记录] 无法保存解析记录：{error:#}",
                "[Parse record] Cannot save the parse record: {error:#}",
                error = error
            )));
        }
    }

    /// Starts the flow from the first stage, dropping whatever was loaded.
    fn start_parse(&mut self, flow: ParseFlow, cx: &mut Context<Self>) {
        if self.state.running || !self.game_directory_valid() {
            return;
        }
        if let Err(error) = crate::paths::ensure_data_dir() {
            self.state.push_log(LogEntry::error(tr!(
                "[数据目录] {error:#}",
                "[Data folder] {error:#}",
                error = error
            )));
            self.state.parse = ParseStatus::Failed;
            cx.notify();
            return;
        }
        self.reset_parsed();
        self.state.begin_parse(flow);
        self.run_parse_stage(cx);
    }

    /// Starts the stage the state machine is waiting on.
    pub(super) fn run_parse_stage(&mut self, cx: &mut Context<Self>) {
        use super::state::ParseStage::*;
        let ParseStatus::Running { stage, flow } = self.state.parse else {
            return;
        };
        if !std::mem::take(&mut self.state.parse_stage_pending) {
            return;
        }
        let policy = flow.policy();
        let started = match stage {
            Audio => self.start_scan(cx),
            Story => self.start_story_index(policy, cx),
            Portrait => self.start_portrait_index(policy, cx),
            Live2d => self.start_live2d_index(policy, cx),
            Roster => self.start_roster_stage(policy, cx),
        };
        if !started {
            self.state.parse = ParseStatus::Failed;
        }
        cx.notify();
    }

    /// Called after every batch of worker messages: moves on when the
    /// running stage has ended. The next worker starts from a separate task
    /// so the listener delivering this message is not replaced under itself.
    pub(super) fn after_task(&mut self, cx: &mut Context<Self>) {
        let before = self.state.parse;
        if self.state.finish_parse_stage() {
            cx.spawn(async move |this, cx| {
                let _ = this.update(cx, |this, cx| this.run_parse_stage(cx));
            })
            .detach();
        } else if before != self.state.parse && self.state.parse == ParseStatus::NotParsed {
            self.reset_parsed();
        }
    }

    /// Forgets everything derived from the game directory.
    pub(super) fn reset_parsed(&mut self) {
        self.state.clear_parsed();
        self.roster = None;
        self.avatars.clear();
        self.export = None;
        self.picker_open = false;
        self.picker_reset = true;
        let character = &mut self.character;
        character.voice_selected.clear();
        character.portrait_selected.clear();
        character.live2d_selected.clear();
        character.story_drawer = None;
        character.live2d_drawer = None;
        character.linked = None;
        character.story_list_for = None;
        let chapter = &mut self.chapter;
        chapter.book = None;
        chapter.selected = None;
        chapter.drawer = None;
        chapter.list_for = None;
        chapter.reveal = None;
        chapter.dropdown = None;
        chapter.results = None;
        chapter.search_open = false;
        self.sounds.selected.clear();
    }

    /// A different game directory invalidates the flow and the loaded data.
    pub(super) fn game_directory_changed(&mut self) {
        if matches!(self.state.parse, ParseStatus::Running { .. }) {
            self.state.request_stop();
        }
        self.state.parse = ParseStatus::NotParsed;
        self.state.parse_stage_pending = false;
        self.reset_parsed();
        self.character.current = None;
    }

    pub(super) fn parse_action(&mut self, cx: &mut Context<Self>) {
        match self.state.parse {
            ParseStatus::NotParsed => self.start_parse(ParseFlow::Parse, cx),
            ParseStatus::Done { .. } | ParseStatus::Failed => {
                self.start_parse(ParseFlow::Rebuild, cx)
            }
            ParseStatus::Checking => {
                self.state.parse = ParseStatus::NotParsed;
                cx.notify();
            }
            ParseStatus::Running { .. } => {
                self.state.request_stop();
                cx.notify();
            }
        }
    }

    /// Label of the parse button and whether it can be pressed.
    pub(super) fn parse_button(&self) -> (&'static str, bool) {
        let running = self.state.running;
        match self.state.parse {
            ParseStatus::Running { .. } if self.state.stop_requested() => {
                (tr!("正在停止…", "Stopping…"), false)
            }
            ParseStatus::Running { .. } | ParseStatus::Checking => (tr!("停止", "Stop"), true),
            ParseStatus::NotParsed => (
                tr!("解析", "Parse"),
                !running && self.game_directory_valid(),
            ),
            ParseStatus::Done { .. } | ParseStatus::Failed => (
                tr!("重新解析", "Re-parse"),
                !running && self.game_directory_valid(),
            ),
        }
    }

    /// Status text and buttons at the right of the title bar.
    pub(super) fn render_parse_controls(&self, cx: &mut Context<Self>) -> gpui::Div {
        let update = self.state.update_available();
        let color = match self.state.parse {
            ParseStatus::NotParsed => TEXT_MUTED,
            ParseStatus::Checking | ParseStatus::Running { .. } => ACCENT,
            ParseStatus::Done { .. } if update => ACCENT,
            ParseStatus::Done { .. } => SUCCESS_TEXT,
            ParseStatus::Failed => DANGER,
        };
        let (label, enabled) = self.parse_button();
        let not_parsed = self.state.parse == ParseStatus::NotParsed;
        let button = if not_parsed {
            primary_button("parse-button", label, enabled)
        } else {
            soft_button("parse-button", label, enabled)
        };
        let update_enabled = !self.state.running && self.game_directory_valid();
        let update_button = update.then(|| {
            primary_button("update-button", tr!("更新", "Update"), update_enabled)
                .occlude()
                .h(px(26.))
                .px(px(12.))
                .rounded(px(7.))
                .text_size(px(12.))
                .when(update_enabled, |this| {
                    this.on_click(
                        cx.listener(|this, _, _, cx| this.start_parse(ParseFlow::Parse, cx)),
                    )
                })
        });
        div()
            .flex()
            .flex_shrink_0()
            .items_center()
            .gap(px(12.))
            .mr(px(10.))
            .child(
                div()
                    .text_size(px(12.))
                    .text_color(hsla(color))
                    .child(self.state.parse_text()),
            )
            .children(update_button)
            .child(
                // `occlude` keeps the button out of the title bar's drag area.
                button
                    .occlude()
                    .h(px(26.))
                    .px(px(12.))
                    .rounded(px(7.))
                    .text_size(px(12.))
                    .when(enabled, |this| {
                        this.on_click(cx.listener(|this, _, _, cx| this.parse_action(cx)))
                    }),
            )
    }

    /// Placeholder of the character, chapter and sound pages until the flow
    /// has finished: never partial results.
    pub(super) fn render_parse_gate(&self, cx: &mut Context<Self>) -> AnyElement {
        let running = matches!(
            self.state.parse,
            ParseStatus::Running { .. } | ParseStatus::Checking
        );
        let needs_directory = !running && !self.game_directory_valid();
        let (title, note) = if needs_directory {
            (
                tr!("还没有可用的游戏目录", "No usable game directory yet").to_owned(),
                tr!(
                    "请先在设置页选择游戏安装目录，然后解析游戏资源。",
                    "Choose the game installation folder on the Settings page, then parse the game resources."
                ),
            )
        } else if self.state.restoring() {
            (
                tr!("正在载入上次的解析结果", "Loading the last parse results").to_owned(),
                tr!(
                    "载入完成后这里会显示内容。可以用标题栏的按钮停止。",
                    "Content appears here once loading finishes. Use the title bar button to stop."
                ),
            )
        } else {
            match self.state.parse {
                ParseStatus::Running { .. } => (
                    self.state.parse_text(),
                    tr!(
                        "解析完成后这里会显示内容。可以用标题栏的按钮停止。",
                        "Content appears here once parsing finishes. Use the title bar button to stop."
                    ),
                ),
                ParseStatus::Failed => (
                    tr!("解析失败", "Parse failed").to_owned(),
                    tr!(
                        "详细原因在任务页的日志里。可以重新解析。",
                        "The reason is in the log on the Tasks page. You can parse again."
                    ),
                ),
                _ => (
                    tr!("尚未解析游戏资源", "Game resources not parsed yet").to_owned(),
                    tr!(
                        "解析会读取音频、剧情、立绘、Live2D 和角色资料。完成后，下次启动会自动载入结果。",
                        "Parsing reads audio, story, portraits, Live2D and character data. The results load automatically on the next launch."
                    ),
                ),
            }
        };
        let action: Option<AnyElement> = if running {
            None
        } else if needs_directory {
            Some(
                primary_button(
                    "gate-pick-directory",
                    tr!("选择游戏目录", "Choose game directory"),
                    true,
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.page = Page::Settings;
                    cx.notify();
                }))
                .into_any_element(),
            )
        } else {
            let (label, enabled) = self.parse_button();
            Some(
                primary_button("gate-parse", label, enabled)
                    .when(enabled, |this| {
                        this.on_click(cx.listener(|this, _, _, cx| this.parse_action(cx)))
                    })
                    .into_any_element(),
            )
        };
        let progress = self.state.parse_progress().map(|value| {
            div().w(px(320.)).child(
                Progress::new("parse-progress")
                    .value(value * 100.)
                    .h(px(6.))
                    .bg(hsla(ACCENT)),
            )
        });
        div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(14.))
            .p(px(32.))
            .child(
                div()
                    .text_size(px(SIZE_TAB))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(title),
            )
            .child(
                div()
                    .text_size(px(13.))
                    .text_color(hsla(TEXT_MUTED))
                    .child(note),
            )
            .children(progress)
            .children(action.map(|button| div().pt(px(6.)).child(button)))
            .into_any_element()
    }
}
