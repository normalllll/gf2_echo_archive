//! GPUI desktop interface: custom title bar, left navigation and the
//! overview / audio / tasks pages.

mod assets;
mod chapter;
mod character;
mod ellipsis;
mod export;
mod live2d;
mod parse;
mod parse_record;
mod picker;
mod portrait;
mod roster;
mod settings_page;
mod sounds;
mod state;
mod story;
mod tasks;
mod theme;
mod widgets;

use crate::{
    extraction::WorkerMessage,
    settings::{LanguageChoice, Settings, StoryScope, ThemeChoice},
    story::{CachePolicy, Selection},
};
use chapter::ChapterView;
use character::{CharacterView, ScriptScope};
use ellipsis::ellipsis_text;
use export::{ExportChoices, ExportPanel, LineFilter};
use gpui::{
    AnyElement, App, AppContext, Bounds, Context, Entity, InteractiveElement, IntoElement,
    ListAlignment, ListState, ParentElement, Render, SharedString, StatefulInteractiveElement,
    Styled, Subscription, Task, TitlebarOptions, Window, WindowBounds, WindowOptions, div,
    prelude::FluentBuilder, px, size,
};
use gpui_component::{
    Root, TitleBar,
    input::{InputEvent, InputState},
};
use sounds::SoundsView;
use state::{AppState, LogFilter, LogLevel};
use std::collections::HashMap;
use std::sync::Arc;
use std::{path::PathBuf, rc::Rc, sync::mpsc::Receiver};
use theme::*;
pub use widgets::fmt_count;

pub const APP_TITLE: &str = "GF2 Echo Archive";
/// Where the program is published; the only official source.
pub const REPO_URL: &str = "https://github.com/normalllll/gf2_echo_archive";

/// Shown next to the name wherever the window is titled, so a resold copy
/// tells its buyer that the program is free.
pub fn free_notice() -> &'static str {
    tr!(
        "免费软件，付费获得请立即退款",
        "Free software. If you paid for it, ask for a refund"
    )
}

/// The title of the system window (task bar and window switcher).
fn window_title() -> String {
    format!("{APP_TITLE} - {}", free_notice())
}

pub fn run() {
    gpui_platform::application()
        .with_assets(assets::Assets)
        .run(|cx: &mut App| {
            gpui_component::init(cx);
            gpui_component::set_locale(crate::i18n::language().component_locale());
            theme::apply(
                theme::resolve_theme(Settings::load().theme, cx.window_appearance()),
                cx,
            );
            let display = cx
                .primary_display()
                .map(|display| display.bounds().size)
                .map(|size| (f32::from(size.width), f32::from(size.height)));
            let (width, height) = initial_window_size(display);
            let bounds = Bounds::centered(None, size(px(width), px(height)), cx);
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some(window_title().into()),
                    ..TitleBar::title_bar_options()
                }),
                window_min_size: Some(size(px(WINDOW_MIN.0), px(WINDOW_MIN.1))),
                ..Default::default()
            };
            cx.spawn(async move |cx| {
                cx.open_window(options, |window, cx| {
                    window.set_window_title(&window_title());
                    let view = cx.new(|cx| EchoArchive::new(window, cx));
                    cx.new(|cx| Root::new(view, window, cx))
                })?;
                Ok::<_, anyhow::Error>(())
            })
            .detach();
        });
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Character,
    Chapter,
    Sounds,
    Tasks,
    Settings,
}

#[derive(Clone, Copy)]
pub enum PathTarget {
    Game,
    Output,
}

pub struct EchoArchive {
    state: AppState,
    settings: Settings,
    page: Page,
    /// Sidebar state chosen in this session for narrow windows, where the
    /// sidebar starts collapsed regardless of the saved state.
    nav_override: Option<bool>,
    /// Width class the override was made in; crossing it drops the override.
    nav_was_narrow: Option<bool>,
    export: Option<ExportPanel>,
    export_choices: ExportChoices,
    /// Settings could not be written; reported once.
    settings_error_reported: bool,
    game_input: Entity<InputState>,
    output_input: Entity<InputState>,
    chapter: ChapterView,
    chapter_search: Entity<InputState>,
    sounds: SoundsView,
    sounds_search: Entity<InputState>,
    portrait: portrait::PortraitThumbs,
    live2d: live2d::Live2dThumbs,
    character: CharacterView,
    voice_search: Entity<InputState>,
    story_tab_search: Entity<InputState>,
    picker_open: bool,
    picker_query: String,
    /// The picker's search box is emptied on the next render.
    picker_reset: bool,
    picker_input: Entity<InputState>,
    picker_scroll: gpui::ScrollHandle,
    roster: Option<Arc<roster::RosterData>>,
    avatars: HashMap<String, PathBuf>,
    settings_scroll: gpui::ScrollHandle,
    /// Transient message shown on the settings page.
    settings_note: Option<String>,
    tasks_scroll: gpui::ScrollHandle,
    /// Last tasks-page layout (stacked or side by side); a change re-pins the log to its end.
    tasks_layout: Option<bool>,
    log_filter: LogFilter,
    log_list: ListState,
    log_rows: Rc<Vec<(LogLevel, SharedString)>>,
    log_rows_key: Option<(usize, usize, LogFilter)>,
    listener: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl EchoArchive {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let settings = Settings::load();
        let state = AppState::from_settings(&settings);
        let game_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(placeholder_game())
                .default_value(state.game_directory.clone())
        });
        let output_input =
            cx.new(|cx| InputState::new(window, cx).default_value(state.output_directory.clone()));
        let chapter_search =
            cx.new(|cx| InputState::new(window, cx).placeholder(placeholder_chapter()));
        let sounds_search =
            cx.new(|cx| InputState::new(window, cx).placeholder(placeholder_sounds()));
        let voice_search =
            cx.new(|cx| InputState::new(window, cx).placeholder(placeholder_voice()));
        let story_tab_search =
            cx.new(|cx| InputState::new(window, cx).placeholder(placeholder_story_tab()));
        let picker_input =
            cx.new(|cx| InputState::new(window, cx).placeholder(placeholder_picker()));

        let subscriptions = vec![
            cx.observe_window_appearance(window, |this, window, cx| {
                if this.settings.theme == ThemeChoice::System {
                    this.apply_theme(window, cx);
                }
            }),
            cx.subscribe_in(&voice_search, window, |this, input, event, _, cx| {
                if let InputEvent::Change = event {
                    this.voice_search_changed(input, cx);
                }
            }),
            cx.subscribe_in(&story_tab_search, window, |this, input, event, _, cx| {
                if let InputEvent::Change = event {
                    this.story_tab_search_changed(input, cx);
                }
            }),
            cx.subscribe_in(&picker_input, window, |this, input, event, _, cx| {
                if let InputEvent::Change = event {
                    this.picker_query = input.read(cx).value().to_string();
                    cx.notify();
                }
            }),
            cx.subscribe_in(&game_input, window, |this, input, event, _, cx| {
                if let InputEvent::Change = event {
                    let value = input.read(cx).value().to_string();
                    if value.trim() != this.state.game_directory.trim() {
                        this.game_directory_changed();
                    }
                    this.state.game_directory = value;
                    this.settings_note = None;
                    this.save_settings();
                    cx.notify();
                }
            }),
            cx.subscribe_in(&output_input, window, |this, input, event, _, cx| {
                if let InputEvent::Change = event {
                    this.state.output_directory = input.read(cx).value().to_string();
                    this.save_settings();
                    cx.notify();
                }
            }),
            cx.subscribe_in(&chapter_search, window, |this, input, event, _, cx| {
                if let InputEvent::Change = event {
                    let value = input.read(cx).value().to_string();
                    this.chapter_search_changed(value, cx);
                }
            }),
            cx.subscribe_in(&sounds_search, window, |this, input, event, _, cx| {
                if let InputEvent::Change = event {
                    this.sounds.query = input.read(cx).value().to_string();
                    this.sounds
                        .scroll
                        .scroll_to_item(0, gpui::ScrollStrategy::Top);
                    cx.notify();
                }
            }),
        ];

        let log_list = ListState::new(0, ListAlignment::Top, px(400.));
        let story_scope = settings.story_scope;
        let mut this = Self {
            state,
            settings,
            page: Page::Character,
            nav_override: None,
            nav_was_narrow: None,
            export: None,
            export_choices: ExportChoices::default(),
            settings_error_reported: false,
            game_input,
            output_input,
            chapter: ChapterView::default(),
            chapter_search,
            sounds: SoundsView::default(),
            sounds_search,
            portrait: portrait::PortraitThumbs::default(),
            live2d: live2d::Live2dThumbs::default(),
            character: CharacterView {
                scope: match story_scope {
                    StoryScope::Cn => ScriptScope::Cn,
                    StoryScope::All => ScriptScope::All,
                },
                ..CharacterView::default()
            },
            voice_search,
            story_tab_search,
            picker_open: false,
            picker_query: String::new(),
            picker_reset: false,
            picker_input,
            picker_scroll: gpui::ScrollHandle::new(),
            roster: None,
            avatars: HashMap::new(),
            settings_scroll: gpui::ScrollHandle::new(),
            settings_note: None,
            tasks_scroll: gpui::ScrollHandle::new(),
            tasks_layout: None,
            log_filter: LogFilter::All,
            log_list,
            log_rows: Rc::new(Vec::new()),
            log_rows_key: None,
            listener: None,
            _subscriptions: subscriptions,
        };
        this.start_startup_check(cx);
        this
    }

    fn start_scan(&mut self, cx: &mut Context<Self>) -> bool {
        let receiver = self.state.start_scan();
        let started = receiver.is_some();
        if let Some(receiver) = receiver {
            self.listen(receiver, cx);
        }
        cx.notify();
        started
    }

    /// Writes the current settings; a failure is logged once and never
    /// interrupts the user.
    fn save_settings(&mut self) {
        let game = self.state.game_directory.trim();
        self.settings.game_directory = (!game.is_empty()).then(|| PathBuf::from(game));
        let output = self.state.output_directory.trim();
        self.settings.output_directory = (!output.is_empty()).then(|| PathBuf::from(output));
        if let Err(error) = self.settings.save()
            && !self.settings_error_reported
        {
            self.settings_error_reported = true;
            self.state.push_log(state::LogEntry::warning(tr!(
                "[设置] 无法保存设置：{error:#}",
                "[Settings] Cannot save the settings: {error:#}",
                error = error
            )));
        }
    }

    fn apply_theme(&self, window: &Window, cx: &mut Context<Self>) {
        theme::apply(
            theme::resolve_theme(self.settings.theme, window.appearance()),
            cx,
        );
        cx.refresh_windows();
    }

    fn set_theme(&mut self, choice: ThemeChoice, window: &Window, cx: &mut Context<Self>) {
        self.settings.theme = choice;
        self.save_settings();
        self.apply_theme(window, cx);
    }

    /// Applies a language choice at once. Text already in the log and in
    /// finished tasks keeps the language it was written in.
    fn set_language(
        &mut self,
        choice: LanguageChoice,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.settings.language = choice;
        self.save_settings();
        let language = crate::i18n::resolve_for_system(choice);
        crate::i18n::set_language(language);
        gpui_component::set_locale(language.component_locale());
        window.set_window_title(&window_title());
        self.update_placeholders(window, cx);
        cx.refresh_windows();
    }

    /// Placeholders of the text inputs; the inputs keep their own copy.
    fn update_placeholders(&self, window: &mut Window, cx: &mut Context<Self>) {
        let placeholders = [
            (&self.game_input, placeholder_game()),
            (&self.chapter_search, placeholder_chapter()),
            (&self.sounds_search, placeholder_sounds()),
            (&self.voice_search, placeholder_voice()),
            (&self.story_tab_search, placeholder_story_tab()),
            (&self.picker_input, placeholder_picker()),
        ];
        for (input, placeholder) in placeholders {
            input.update(cx, |input, cx| {
                input.set_placeholder(placeholder, window, cx)
            });
        }
    }

    /// Below 1280 px the sidebar starts collapsed; wider windows use the
    /// saved state. A toggle in this session wins in both cases.
    fn sidebar_collapsed(&self, window: &Window) -> bool {
        if f32::from(window.viewport_size().width) < NARROW_WIDTH {
            self.nav_override.unwrap_or(true)
        } else {
            self.nav_override.unwrap_or(self.settings.sidebar_collapsed)
        }
    }

    /// A toggle in a wide window is saved; in a narrow one it lasts until the
    /// window changes width class.
    fn toggle_sidebar(&mut self, window: &Window, cx: &mut Context<Self>) {
        let collapsed = !self.sidebar_collapsed(window);
        self.nav_override = Some(collapsed);
        if f32::from(window.viewport_size().width) >= NARROW_WIDTH {
            self.settings.sidebar_collapsed = collapsed;
            self.save_settings();
        }
        cx.notify();
    }

    /// Forgets the session toggle when the window crosses the narrow threshold.
    fn sync_sidebar(&mut self, window: &Window) {
        let narrow = f32::from(window.viewport_size().width) < NARROW_WIDTH;
        if self.nav_was_narrow.is_some_and(|was| was != narrow) {
            self.nav_override = None;
        }
        self.nav_was_narrow = Some(narrow);
    }

    /// Width taken by the sidebar in the current window.
    #[allow(dead_code)]
    pub(super) fn nav_width(&self, window: &Window) -> f32 {
        if self.sidebar_collapsed(window) {
            NAV_COLLAPSED
        } else {
            NAV_EXPANDED
        }
    }

    fn start_story_index(&mut self, policy: CachePolicy, cx: &mut Context<Self>) -> bool {
        let receiver = self.state.start_story_index(policy);
        let started = receiver.is_some();
        if let Some(receiver) = receiver {
            self.listen_with(receiver, AppState::handle_story, cx);
        }
        cx.notify();
        started
    }

    fn start_story_export(
        &mut self,
        selection: Selection,
        filter: Option<LineFilter>,
        cx: &mut Context<Self>,
    ) {
        let choices = self.export_choices;
        if let Some(receiver) = self.state.start_story_export(
            selection,
            filter,
            choices.with_audio,
            choices.with_audio && choices.by_speaker,
        ) {
            self.listen_with(receiver, AppState::handle_story, cx);
        }
        cx.notify();
    }

    fn listen(&mut self, receiver: Receiver<WorkerMessage>, cx: &mut Context<Self>) {
        self.listen_with(receiver, AppState::handle, cx);
    }

    /// Receives worker messages on a GPUI task. Each blocking `recv` runs on
    /// the background executor, so the UI is woken only when a message arrives.
    fn listen_with<M: Send + 'static>(
        &mut self,
        receiver: Receiver<M>,
        handle: fn(&mut AppState, M),
        cx: &mut Context<Self>,
    ) {
        let executor = cx.background_executor().clone();
        self.listener = Some(cx.spawn(async move |this, cx| {
            let mut receiver = receiver;
            loop {
                let (first, returned) = executor
                    .spawn(async move {
                        let message = receiver.recv();
                        (message, receiver)
                    })
                    .await;
                receiver = returned;
                let mut batch = Vec::new();
                let mut disconnected = false;
                match first {
                    Ok(message) => batch.push(message),
                    Err(_) => disconnected = true,
                }
                while let Ok(message) = receiver.try_recv() {
                    batch.push(message);
                }
                let updated = this.update(cx, |this, cx| {
                    for message in batch {
                        handle(&mut this.state, message);
                    }
                    if disconnected {
                        this.state.worker_disconnected();
                    }
                    this.after_task(cx);
                    cx.notify();
                });
                if disconnected || updated.is_err() {
                    break;
                }
            }
        }));
    }

    fn pick_folder(&mut self, target: PathTarget, window: &mut Window, cx: &mut Context<Self>) {
        let dialog = rfd::AsyncFileDialog::new().pick_folder();
        cx.spawn_in(window, async move |this, cx| {
            if let Some(handle) = dialog.await {
                let path = handle.path().to_path_buf();
                let _ = this.update_in(cx, |this, window, cx| {
                    this.set_path(target, path, window, cx);
                });
            }
        })
        .detach();
    }

    fn set_path(
        &mut self,
        target: PathTarget,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let text = path.to_string_lossy().into_owned();
        let input = match target {
            PathTarget::Game => {
                if text.trim() != self.state.game_directory.trim() {
                    self.game_directory_changed();
                }
                self.state.game_directory = text.clone();
                &self.game_input
            }
            PathTarget::Output => {
                self.state.output_directory = text.clone();
                &self.output_input
            }
        };
        input.update(cx, |input, cx| input.set_value(text, window, cx));
        self.save_settings();
        cx.notify();
    }

    fn sync_log_rows(&mut self) {
        let key = (
            self.state.logs_version,
            self.state.logs_generation,
            self.log_filter,
        );
        if self.log_rows_key == Some(key) {
            return;
        }
        let previous = self.log_rows.len();
        let appended_only = self
            .log_rows_key
            .is_some_and(|(_, generation, filter)| generation == key.1 && filter == key.2);
        let rows: Vec<_> = self
            .state
            .logs
            .iter()
            .filter(|entry| self.log_filter.accepts(entry.level))
            .map(|entry| {
                (
                    entry.level,
                    SharedString::from(display_log_text(&entry.text)),
                )
            })
            .collect();
        if appended_only && rows.len() >= previous && previous == self.log_list.item_count() {
            self.log_list
                .splice(previous..previous, rows.len() - previous);
        } else {
            self.log_list.reset(rows.len());
        }
        // New records always bring the newest line into view.
        if (rows.len() > previous || !appended_only) && !rows.is_empty() {
            // Past-the-end offset: the list lays out backwards from the last row,
            // which works even before the new rows have been measured.
            self.log_list.scroll_to(gpui::ListOffset {
                item_ix: rows.len(),
                offset_in_item: px(0.),
            });
        }
        self.log_rows = Rc::new(rows);
        self.log_rows_key = Some(key);
    }

    /// Title bar, left to right: logo, name and free notice, game directory, parse status
    /// and button; the window controls follow.
    fn render_title_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let game = self.state.game_directory.trim();
        TitleBar::new().h(px(TITLE_BAR_HEIGHT)).pl(px(14.)).child(
            div()
                .flex()
                .items_center()
                .flex_1()
                .min_w_0()
                .h_full()
                .child(
                    div()
                        .flex()
                        .flex_shrink_0()
                        .items_center()
                        .gap(px(8.))
                        .child(gpui::img(assets::APP_ICON).size(px(20.)).flex_shrink_0())
                        .child(
                            div()
                                .text_size(px(13.))
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .child(APP_TITLE),
                        )
                        .child(
                            // Inside the drag area, so it has to occlude to be clickable.
                            div()
                                .id("free-notice")
                                .occlude()
                                .ml(px(4.))
                                .text_size(px(12.))
                                .text_color(hsla(WARNING))
                                .cursor_pointer()
                                .hover(|style| style.underline())
                                .tooltip(|window, cx| {
                                    gpui_component::tooltip::Tooltip::new(tr!(
                                        "打开项目主页：{}",
                                        "Open the project page: {}",
                                        REPO_URL
                                    ))
                                    .build(window, cx)
                                })
                                .on_click(|_, _, cx| cx.open_url(REPO_URL))
                                .child(free_notice()),
                        ),
                )
                .child(
                    div().flex_1().min_w_0().ml(px(24.)).pr(px(24.)).child(mono(
                        div()
                            .min_w_0()
                            .overflow_hidden()
                            .text_size(px(11.5))
                            .text_color(hsla(TEXT_DIM))
                            .child(ellipsis_text(if game.is_empty() {
                                tr!("未指定游戏目录", "No game directory selected").to_owned()
                            } else {
                                game.to_owned()
                            })),
                    )),
                )
                .child(self.render_parse_controls(cx)),
        )
    }

    fn nav_item(
        &self,
        collapsed: bool,
        (id, icon_path, label, page): NavEntry,
        busy: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let active = page == self.page;
        let progress = self.state.progress;
        let base = div()
            .id(id)
            .relative()
            .flex()
            .items_center()
            .rounded(px(if collapsed { 10. } else { 8. }))
            .cursor_pointer()
            .when(active, |this| {
                this.bg(hsla(BG_NAV_ACTIVE)).text_color(hsla(TEXT_STRONG))
            })
            .when(!active, |this| {
                this.text_color(hsla(TEXT_MUTED))
                    .hover(|style| style.bg(hsla(BG_HOVER)))
            })
            .on_click(cx.listener(move |this, _, _, cx| {
                this.page = page;
                cx.notify();
            }));
        let color = if active { TEXT_STRONG } else { TEXT_MUTED };
        if collapsed {
            base.size(px(42.))
                .justify_center()
                .child(widgets::icon(icon_path, 19., color))
                .when(busy, |this| {
                    this.child(
                        div()
                            .absolute()
                            .top(px(8.))
                            .right(px(8.))
                            .size(px(7.))
                            .rounded_full()
                            .bg(hsla(ACCENT)),
                    )
                })
                .tooltip(move |window, cx| {
                    gpui_component::tooltip::Tooltip::new(label()).build(window, cx)
                })
                .into_any_element()
        } else {
            base.gap(px(12.))
                .h(px(38.))
                .px(px(10.))
                .text_size(px(SIZE_BODY))
                .when(active, |this| this.font_weight(gpui::FontWeight::SEMIBOLD))
                .child(widgets::icon(icon_path, 18., color))
                .child(div().flex_1().min_w_0().child(widgets::one_line(label())))
                .when(busy, |this| {
                    this.child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .text_size(px(11.))
                            .text_color(hsla(ACCENT))
                            .child(format!("{:.0}%", progress * 100.))
                            .child(div().size(px(7.)).rounded_full().bg(hsla(ACCENT))),
                    )
                })
                .into_any_element()
        }
    }

    fn render_nav(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let collapsed = self.sidebar_collapsed(window);
        let toggle = div()
            .id(if collapsed {
                "nav-expand"
            } else {
                "nav-collapse"
            })
            .size(px(if collapsed { 42. } else { 38. }))
            .flex_shrink_0()
            .rounded(px(if collapsed { 10. } else { 8. }))
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .hover(|style| style.bg(hsla(BG_HOVER)))
            .child(widgets::icon("app/sidebar.svg", 18., TEXT_MUTED))
            .tooltip(move |window, cx| {
                gpui_component::tooltip::Tooltip::new(if collapsed {
                    tr!("展开侧栏", "Expand sidebar")
                } else {
                    tr!("收起侧栏", "Collapse sidebar")
                })
                .build(window, cx)
            })
            .on_click(cx.listener(|this, _, window, cx| this.toggle_sidebar(window, cx)));
        let settings = self.nav_item(collapsed, NAV_SETTINGS, false, cx);
        // Collapsed: settings above the toggle. Expanded: side by side.
        let footer = div()
            .flex()
            .flex_shrink_0()
            .w_full()
            .when(collapsed, |this| this.flex_col().items_center().gap(px(6.)))
            .when(!collapsed, |this| this.items_center().gap(px(2.)))
            .child(
                div()
                    .when(!collapsed, |this| this.flex_1().min_w_0())
                    .child(settings),
            )
            .child(toggle);
        let running = self.state.running;
        let items = NAV_PAGES
            .iter()
            .map(|entry| {
                let busy = entry.3 == Page::Tasks && running;
                self.nav_item(collapsed, *entry, busy, cx)
            })
            .collect::<Vec<_>>();
        div()
            .w(px(if collapsed {
                NAV_COLLAPSED
            } else {
                NAV_EXPANDED
            }))
            .flex_shrink_0()
            .h_full()
            .flex()
            .flex_col()
            .gap(px(if collapsed { 6. } else { 2. }))
            .when(collapsed, |this| this.items_center().py(px(14.)))
            .when(!collapsed, |this| this.px(px(10.)).py(px(14.)))
            .bg(hsla(BG_NAV))
            .children(items)
            .child(div().flex_1())
            .child(footer)
    }
}

fn placeholder_game() -> &'static str {
    tr!(
        "选择或输入游戏安装目录",
        "Choose or enter the game installation folder"
    )
}

fn placeholder_chapter() -> &'static str {
    tr!("搜索全部台词", "Search all lines")
}

fn placeholder_sounds() -> &'static str {
    tr!(
        "资源名关键词，例如 Agent、ASMR",
        "Resource keyword, e.g. Agent, ASMR"
    )
}

fn placeholder_voice() -> &'static str {
    tr!("筛选音频包", "Filter audio packs")
}

fn placeholder_story_tab() -> &'static str {
    tr!("在该角色的台词中搜索", "Search this character's lines")
}

fn placeholder_picker() -> &'static str {
    tr!(
        "搜索中文名、资源名或别名，例如 Krolik",
        "Search by name, Chinese name, resource name or alias, e.g. Krolik"
    )
}

type NavEntry = (&'static str, &'static str, fn() -> &'static str, Page);

const NAV_PAGES: [NavEntry; 4] = [
    (
        "nav-character",
        "app/chars.svg",
        || tr!("角色", "Characters"),
        Page::Character,
    ),
    (
        "nav-chapter",
        "app/book.svg",
        || tr!("剧情章节", "Story"),
        Page::Chapter,
    ),
    (
        "nav-sounds",
        "app/audio.svg",
        || tr!("音乐与音效", "Music & SFX"),
        Page::Sounds,
    ),
    (
        "nav-tasks",
        "app/tasks.svg",
        || tr!("任务", "Tasks"),
        Page::Tasks,
    ),
];

const NAV_SETTINGS: NavEntry = (
    "nav-settings",
    "app/settings.svg",
    || tr!("设置", "Settings"),
    Page::Settings,
);

/// Windows narrower than this start with the sidebar collapsed.
const NARROW_WIDTH: f32 = 1280.;
const TITLE_BAR_HEIGHT: f32 = 38.;
const WINDOW_DEFAULT: (f32, f32) = (1600., 1000.);
const WINDOW_MIN: (f32, f32) = (1200., 760.);
/// Largest share of the primary display the first window may take.
const WINDOW_SCREEN_SHARE: f32 = 0.9;
const NAV_EXPANDED: f32 = 216.;
const NAV_COLLAPSED: f32 = 64.;

impl Render for EchoArchive {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_portrait();
        self.sync_live2d();
        self.sync_log_rows();
        self.sync_sidebar(window);
        if std::mem::take(&mut self.picker_reset) {
            self.picker_query.clear();
            self.picker_input
                .update(cx, |input, cx| input.set_value("", window, cx));
        }
        let page = match self.page {
            Page::Character | Page::Chapter | Page::Sounds if !self.parsed() => {
                self.render_parse_gate(cx)
            }
            Page::Character => self.render_character(window, cx).into_any_element(),
            Page::Settings => self.render_settings(window, cx).into_any_element(),
            Page::Sounds => self.render_sounds(window, cx).into_any_element(),
            Page::Chapter => self.render_chapter(window, cx).into_any_element(),
            Page::Tasks => self.render_tasks(window, cx).into_any_element(),
        };
        let picker = self.render_picker(window, cx);
        let overlay = self.render_export_panel(cx);
        div()
            .id("echo-archive")
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .bg(hsla(BG_APP))
            .text_color(hsla(TEXT))
            .font_family(FONT_FAMILY)
            .text_size(px(13.))
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                let keystroke = &event.keystroke;
                if keystroke.key == "escape" {
                    if this.export.is_some() {
                        this.close_export(cx);
                    } else if this.picker_open {
                        this.close_picker(cx);
                    }
                } else if keystroke.key == "k" && keystroke.modifiers.control && this.parsed() {
                    this.open_picker(window, cx);
                }
            }))
            .child(self.render_title_bar(cx))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .child(self.render_nav(window, cx))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .overflow_hidden()
                            .child(page),
                    ),
            )
            .children(picker)
            .children(overlay)
    }
}

/// First window size: the default, limited to 90% of the primary display but
/// never below the minimum size.
fn initial_window_size(display: Option<(f32, f32)>) -> (f32, f32) {
    let fit = |default: f32, screen: Option<f32>, min: f32| {
        screen
            .map_or(default, |screen| default.min(screen * WINDOW_SCREEN_SHARE))
            .max(min)
    };
    (
        fit(WINDOW_DEFAULT.0, display.map(|d| d.0), WINDOW_MIN.0),
        fit(WINDOW_DEFAULT.1, display.map(|d| d.1), WINDOW_MIN.1),
    )
}

// ---------------------------------------------------------------------------
/// Lines wider than the log panel would wrap at arbitrary characters (and
/// could start a line with a semicolon), so long records break after each
/// semicolon instead, full-width or ASCII.
/// Only the display changes; copying the log keeps the original text.
pub fn display_log_text(text: &str) -> String {
    text.lines()
        .map(|line| {
            if line.chars().count() > 40 {
                line.replace('\u{ff1b}', "\u{ff1b}\n    ")
                    .replace("; ", ";\n    ")
            } else {
                line.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::{display_log_text, initial_window_size};

    #[test]
    fn window_fits_the_display_without_going_below_the_minimum() {
        assert_eq!(initial_window_size(None), (1600., 1000.));
        assert_eq!(initial_window_size(Some((3840., 2160.))), (1600., 1000.));
        assert_eq!(initial_window_size(Some((1600., 900.))), (1440., 810.));
        assert_eq!(initial_window_size(Some((1280., 800.))), (1200., 760.));
    }

    #[test]
    fn breaks_long_log_lines_after_semicolons() {
        let short = "[执行摘要] 成功=1；失败=0";
        assert_eq!(display_log_text(short), short);
        let long = "[扫描完成] 2931 个音频包；1785 个纯索引 ACB；1 个重复资源已优先使用缓存版本。";
        let shown = display_log_text(long);
        assert!(shown.lines().count() == 3);
        assert!(
            shown
                .lines()
                .all(|line| !line.trim_start().starts_with('；'))
        );
        assert_eq!(shown.replace("\n    ", ""), long);
    }
}
