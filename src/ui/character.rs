//! Character page: header with fading portrait, tabs for voice, story
//! lines, portraits and Live2D, the story-line and model drawers.

use super::{
    EchoArchive, Page,
    export::{ExportKind, ExportPanel, LineFilter},
    fmt_count,
    live2d::{core_line, file_name_of, file_tree, status_color},
    portrait::{Backdrop, Thumb, checker_backdrop},
    roster::{CharEntry, CharKind},
    state::RowStatus,
    story::{
        StoryData, display_speaker, multi_line_text, narrator_label, tree_label, wrap_for_display,
    },
    theme::*,
    widgets::{
        action_bar, check_badge, check_glyph, chip, drawer_frame, empty_state, ghost_button,
        group_title, icon, link, mono_note, one_line, page_scroll, primary_button, row_frame,
        search_field, selected_label, selection_ring, soft_button,
    },
};
use crate::{
    catalog::{Language, SourceInfo},
    live2d::{FileStatus, manifest::Role},
    settings::StoryScope,
    story::{
        Selection,
        link::{Line, Linked},
    },
};
use gpui::{
    AnyElement, App, Context, Entity, FontWeight, InteractiveElement, IntoElement, ListAlignment,
    ListState, ObjectFit, ParentElement, ScrollStrategy, SharedString, StatefulInteractiveElement,
    Styled, StyledImage, UniformListScrollHandle, WeakEntity, Window, div, img, linear_color_stop,
    linear_gradient, list, prelude::FluentBuilder, px, uniform_list,
};
use gpui_component::{input::InputState, scroll::ScrollableElement};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    path::PathBuf,
    rc::Rc,
    sync::Arc,
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum CharTab {
    Voice,
    Story,
    Portrait,
    Live2d,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum LangFilter {
    All,
    Cn,
    Jp,
}

impl LangFilter {
    pub fn accepts(self, language: Language) -> bool {
        match self {
            Self::All => true,
            Self::Cn => language != Language::Jp,
            Self::Jp => language != Language::Cn,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::All => "CN + JP",
            Self::Cn => "CN",
            Self::Jp => "JP",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ScriptScope {
    Cn,
    Original,
    All,
}

impl ScriptScope {
    pub fn accepts(self, cn_script: bool) -> bool {
        match self {
            Self::Cn => cn_script,
            Self::Original => !cn_script,
            Self::All => true,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Cn => tr!("中文脚本", "Chinese scripts"),
            Self::Original => tr!("原始区域", "Original region"),
            Self::All => tr!("全部脚本", "All scripts"),
        }
    }
}

pub struct CharacterView {
    /// Roster key of the character on screen.
    pub current: Option<String>,
    pub tab: CharTab,
    pub lang: LangFilter,
    pub voice_query: String,
    pub voice_selected: BTreeSet<String>,
    pub voice_scroll: gpui::ScrollHandle,
    pub scope: ScriptScope,
    pub story_query: String,
    pub story_list: ListState,
    /// Line open in the drawer (index into `StoryData::search`).
    pub story_drawer: Option<usize>,
    pub drawer_sources_open: bool,
    pub drawer_scroll: gpui::ScrollHandle,
    /// Book position → linked lines, for the drawer.
    pub linked: Option<(usize, Rc<Linked>)>,
    pub portrait_backdrop: Backdrop,
    pub portrait_selected: BTreeSet<usize>,
    pub portrait_scroll: UniformListScrollHandle,
    pub live2d_selected: BTreeSet<usize>,
    pub live2d_scroll: UniformListScrollHandle,
    pub live2d_drawer: Option<usize>,
    /// (character, script scope, query, text width) the story list was measured for.
    pub story_list_for: Option<(String, ScriptScope, String, u32)>,
    /// Figure top of header images, by thumbnail path.
    pub art_tops: HashMap<PathBuf, f32>,
}

impl Default for CharacterView {
    fn default() -> Self {
        Self {
            current: None,
            tab: CharTab::Voice,
            lang: LangFilter::All,
            voice_query: String::new(),
            voice_selected: BTreeSet::new(),
            voice_scroll: gpui::ScrollHandle::new(),
            scope: ScriptScope::Cn,
            story_query: String::new(),
            story_list: ListState::new(0, ListAlignment::Top, px(600.)),
            story_drawer: None,
            drawer_sources_open: false,
            drawer_scroll: gpui::ScrollHandle::new(),
            linked: None,
            portrait_backdrop: Backdrop::Dark,
            portrait_selected: BTreeSet::new(),
            portrait_scroll: UniformListScrollHandle::new(),
            live2d_selected: BTreeSet::new(),
            live2d_scroll: UniformListScrollHandle::new(),
            live2d_drawer: None,
            story_list_for: None,
            art_tops: HashMap::new(),
        }
    }
}

/// One merged voice row: every CN/JP and AWB/ACB file of one resource.
struct VoiceRow {
    key: String,
    title: String,
    sources: Vec<usize>,
    languages: String,
    packaging: String,
    character_voice: bool,
}

/// The resource name without `VO_` and the character's own name tokens;
/// nothing left means the base voice pack. Never translated or guessed.
pub fn voice_title(key: &str, names: &[String]) -> String {
    let rest = key
        .get(..3)
        .filter(|prefix| prefix.eq_ignore_ascii_case("VO_"))
        .map_or(key, |_| &key[3..]);
    let kept: Vec<&str> = rest
        .split('_')
        .filter(|token| {
            !token.is_empty() && !names.iter().any(|name| name.eq_ignore_ascii_case(token))
        })
        .collect();
    if kept.is_empty() {
        tr!("基础语音", "Base voice").to_owned()
    } else {
        kept.join("_")
    }
}

fn is_character_voice(key: &str) -> bool {
    key.get(..3)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("VO_"))
}

fn language_label(languages: &BTreeSet<Language>) -> String {
    let has = |language| languages.contains(&language);
    match (has(Language::Cn), has(Language::Jp)) {
        (true, true) => "CN · JP".to_owned(),
        (true, false) => "CN".to_owned(),
        (false, true) => "JP".to_owned(),
        (false, false) => tr!("通用", "Common").to_owned(),
    }
}

fn voice_rows(entry: &CharEntry, sources: &[SourceInfo]) -> Vec<VoiceRow> {
    let names: Vec<String> = entry
        .resource
        .iter()
        .chain(entry.aliases.iter().filter(|alias| alias.is_ascii()))
        .cloned()
        .collect();
    let mut grouped: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for &index in &entry.voice {
        if let Some(source) = sources.get(index) {
            grouped.entry(source.key.as_str()).or_default().push(index);
        }
    }
    grouped
        .into_iter()
        .map(|(key, indices)| {
            let languages: BTreeSet<Language> =
                indices.iter().map(|&i| sources[i].language).collect();
            let kinds: BTreeSet<&str> = indices.iter().map(|&i| sources[i].kind.label()).collect();
            VoiceRow {
                key: key.to_owned(),
                title: voice_title(key, &names),
                languages: language_label(&languages),
                packaging: kinds.into_iter().collect::<Vec<_>>().join(" + "),
                character_voice: is_character_voice(key),
                sources: indices,
            }
        })
        .collect()
}

/// Rows of the story tab: a heading per script, then its lines.
#[derive(Clone)]
enum StoryRow {
    Script { book: usize },
    Line(usize),
}

const HEADER_TALL: f32 = 196.;
const HEADER_SHORT: f32 = 160.;
const ROW_VOICE: f32 = 60.;
const ROW_LINE: f32 = 48.;
const LINE_HEIGHT: f32 = 24.;
const LINE_NUMBER: f32 = 44.;
const LINE_VOICES: f32 = 56.;
const LINE_GAP: f32 = 18.;
const STORY_SEARCH_MIN: f32 = 140.;
/// Search field at its minimum plus the three script-scope chips.
const STORY_TOOLS_MIN: f32 = 500.;
const STORY_SEARCH_MAX: f32 = 320.;
const DRAWER_WIDTH: f32 = 360.;
const CARD_GAP: f32 = 24.;
const CARD_CAPTION: f32 = 34.;
const CARD_MAX: f32 = 420.;
const CARD_RADIUS: f32 = 16.;
const MODEL_CARD_RADIUS: f32 = 14.;
const MODEL_CARD_THUMB: f32 = 200.;
const MODEL_CARD_CAPTION: f32 = 52.;

/// Horizontal page padding for a main-area width.
pub(super) fn page_padding(main_width: f32) -> f32 {
    if main_width >= 1000. { 48. } else { 28. }
}

impl EchoArchive {
    /// Switches the character, keeping the tab when the new one has content
    /// there; selections belong to the previous character and are cleared.
    pub(super) fn select_character(&mut self, key: String, cx: &mut Context<Self>) {
        let view = &mut self.character;
        if view.current.as_deref() != Some(key.as_str()) {
            view.voice_selected.clear();
            view.portrait_selected.clear();
            view.live2d_selected.clear();
            view.story_drawer = None;
            view.live2d_drawer = None;
            view.voice_scroll = gpui::ScrollHandle::new();
            view.story_list_for = None;
            view.portrait_scroll.scroll_to_item(0, ScrollStrategy::Top);
            view.live2d_scroll.scroll_to_item(0, ScrollStrategy::Top);
        }
        view.current = Some(key.clone());
        if let Some(data) = self.roster.clone()
            && let Some(index) = data.find(&key)
        {
            let entry = &data.entries[index];
            let has = |tab| match tab {
                CharTab::Voice => !entry.voice.is_empty(),
                CharTab::Story => !entry.lines.is_empty(),
                CharTab::Portrait => !entry.portraits.is_empty(),
                CharTab::Live2d => !entry.live2d.is_empty(),
            };
            if !has(self.character.tab)
                && let Some(tab) = [
                    CharTab::Voice,
                    CharTab::Story,
                    CharTab::Portrait,
                    CharTab::Live2d,
                ]
                .into_iter()
                .find(|tab| has(*tab))
            {
                self.character.tab = tab;
            }
            if entry.kind != CharKind::Unassigned {
                self.settings.touch_character(&entry.key);
                self.save_settings();
            }
        }
        cx.notify();
    }

    /// Lines of the entry in the current script scope.
    fn scope_line_count(&self, entry: &CharEntry) -> usize {
        let Some(data) = self.state.story_data() else {
            return 0;
        };
        entry
            .lines
            .iter()
            .filter(|&&i| {
                self.character
                    .scope
                    .accepts(data.books[data.search[i].book].label.starts_with("CN_"))
            })
            .count()
    }

    fn current_entry(&self) -> Option<(Arc<super::roster::RosterData>, usize)> {
        let data = self.roster.clone()?;
        let index = data.find(self.character.current.as_deref()?)?;
        Some((data, index))
    }

    pub(super) fn render_character(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let Some((data, index)) = self.current_entry() else {
            return self.render_picker_page(window, cx);
        };
        let entry = &data.entries[index];
        let main_width = f32::from(window.viewport_size().width) - self.nav_width(window);
        let height = f32::from(window.viewport_size().height);
        let header_height = if height >= 760. {
            HEADER_TALL
        } else {
            HEADER_SHORT
        };
        let pad = page_padding(main_width);
        let header = self.render_character_header(entry, header_height, pad, cx);
        let body = match self.character.tab {
            CharTab::Voice => self.render_voice_tab(entry, pad, cx),
            CharTab::Story => self.render_story_tab(entry, main_width, pad, window, cx),
            CharTab::Portrait => self.render_portrait_tab(entry, main_width, pad, cx),
            CharTab::Live2d => self.render_live2d_tab(entry, main_width, pad, cx),
        };
        div()
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .child(header)
            .child(div().flex_1().min_h_0().relative().child(body))
            .into_any_element()
    }

    /// Where the figure starts in a header image (share of its height),
    /// measured once per image.
    fn art_top_fraction(&mut self, path: &std::path::Path) -> f32 {
        *self
            .character
            .art_tops
            .entry(path.to_path_buf())
            .or_insert_with(|| {
                image::open(path)
                    .map(|image| figure_top(&image.to_rgba8()))
                    .unwrap_or(0.17)
            })
    }

    fn render_character_header(
        &mut self,
        entry: &CharEntry,
        height: f32,
        pad: f32,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let art = entry.portraits.first().copied().and_then(|index| {
            match self.portrait_thumb(index, cx) {
                Thumb::Ready(path) => Some(path),
                _ => None,
            }
        });
        let art = art.map(|path| {
            let top = art_offset(self.art_top_fraction(&path), height);
            div()
                .absolute()
                .right(px(24.))
                .top(px(top))
                .size(px(420.))
                .child(img(path).size_full().object_fit(ObjectFit::Cover))
                .child(
                    // Fades the art into the header, as the design's mask does.
                    div().absolute().inset_0().bg(linear_gradient(
                        90.,
                        linear_color_stop(hsla(BG_HEADER), 0.),
                        linear_color_stop(hsla_alpha(BG_HEADER, 0.), 0.45),
                    )),
                )
        });
        let voice_count = entry.voice.len();
        let has_lines = !entry.lines.is_empty();
        let story_count = self.scope_line_count(entry);
        let tabs = [
            (
                CharTab::Voice,
                "tab-voice",
                tr!("语音", "Voices"),
                voice_count,
            ),
            (
                CharTab::Story,
                "tab-story",
                tr!("剧情台词", "Story lines"),
                story_count,
            ),
            (
                CharTab::Portrait,
                "tab-portrait",
                tr!("立绘", "Portraits"),
                entry.portraits.len(),
            ),
            (CharTab::Live2d, "tab-live2d", "Live2D", entry.live2d.len()),
        ];
        let current = self.character.tab;
        div()
            .relative()
            .flex_shrink_0()
            .h(px(height))
            .overflow_hidden()
            .bg(hsla(BG_HEADER))
            .children(art)
            .child(
                div()
                    .absolute()
                    .left(px(pad))
                    .top(px(if height >= HEADER_TALL { 36. } else { 24. }))
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .child(
                        div()
                            .id("character-name")
                            .flex()
                            .items_center()
                            .gap(px(10.))
                            .cursor_pointer()
                            .child(
                                div()
                                    .text_size(px(SIZE_HERO))
                                    .font_weight(FontWeight::BOLD)
                                    .child(entry.display_name()),
                            )
                            .child(icon("app/chevron-down.svg", 18., TEXT_MUTED))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.open_picker(window, cx);
                            })),
                    )
                    .child(
                        div()
                            .text_size(px(13.))
                            .text_color(hsla(TEXT_MUTED))
                            .child(entry.subtitle()),
                    ),
            )
            .child(
                div()
                    .absolute()
                    .left(px(pad))
                    .bottom_0()
                    .flex()
                    .gap(px(28.))
                    .children(tabs.into_iter().map(|(tab, id, label, count)| {
                        // Story stays open when another script scope has lines.
                        let enabled = count > 0 || (tab == CharTab::Story && has_lines);
                        let active = tab == current && enabled;
                        div()
                            .id(id)
                            .flex()
                            .items_baseline()
                            .gap(px(6.))
                            .pb(px(12.))
                            .border_b_2()
                            .border_color(if active {
                                hsla(ACCENT)
                            } else {
                                gpui::transparent_black()
                            })
                            .text_size(px(SIZE_TAB))
                            .when(active, |this| {
                                this.text_color(hsla(TEXT_STRONG))
                                    .font_weight(FontWeight::SEMIBOLD)
                            })
                            .when(!active && enabled, |this| {
                                this.text_color(hsla(TEXT_MUTED))
                                    .cursor_pointer()
                                    .hover(|style| style.text_color(hsla(TEXT)))
                            })
                            .when(!enabled, |this| this.text_color(hsla(TEXT_DISABLED)))
                            .child(label)
                            .child(
                                div()
                                    .text_size(px(12.))
                                    .font_weight(FontWeight::NORMAL)
                                    .text_color(hsla(if active {
                                        TEXT_MUTED
                                    } else if enabled {
                                        TEXT_FAINT
                                    } else {
                                        TEXT_DISABLED
                                    }))
                                    .child(fmt_count(count)),
                            )
                            .when(enabled, |this| {
                                this.on_click(cx.listener(move |this, _, _, cx| {
                                    this.character.tab = tab;
                                    cx.notify();
                                }))
                            })
                    })),
            )
            .into_any_element()
    }

    // -----------------------------------------------------------------------
    // Voice tab.

    fn render_voice_tab(
        &mut self,
        entry: &CharEntry,
        pad: f32,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if !self.state.is_scanned() {
            return empty_state(tr!("音频尚未扫描。", "Audio has not been scanned yet."));
        }
        let query = self.character.voice_query.trim().to_lowercase();
        let lang = self.character.lang;
        let rows: Vec<VoiceRow> = voice_rows(entry, &self.state.sources)
            .into_iter()
            .filter(|row| {
                row.sources
                    .iter()
                    .any(|&i| lang.accepts(self.state.sources[i].language))
                    && (query.is_empty()
                        || row.key.to_lowercase().contains(&query)
                        || row.title.to_lowercase().contains(&query))
            })
            .collect();
        let (voices, others): (Vec<VoiceRow>, Vec<VoiceRow>) =
            rows.into_iter().partition(|row| row.character_voice);

        let list = div()
            .flex()
            .flex_col()
            .px(px(pad))
            .pb(px(100.))
            .when(!voices.is_empty(), |this| {
                this.child(
                    group_title(tr!("角色语音 · {}", "Character voices · {}", voices.len()))
                        .pt(px(14.))
                        .pb(px(6.)),
                )
                .children(voices.iter().map(|row| self.render_voice_row(row, cx)))
            })
            .when(!others.is_empty(), |this| {
                this.child(
                    group_title(tr!("相关音效 · {}", "Related sounds · {}", others.len()))
                        .pt(px(22.))
                        .pb(px(6.)),
                )
                .children(others.iter().map(|row| self.render_voice_row(row, cx)))
            })
            .when(voices.is_empty() && others.is_empty(), |this| {
                this.child(
                    div()
                        .pt(px(40.))
                        .text_size(px(13.))
                        .text_color(hsla(TEXT_FAINT))
                        .child(tr!(
                            "没有符合筛选条件的音频包。",
                            "No audio packs match the filter."
                        )),
                )
            });

        let toolbar = div()
            .flex()
            .flex_shrink_0()
            .items_center()
            .gap(px(12.))
            .pt(px(20.))
            .pb(px(8.))
            .px(px(pad))
            .child(search_field(&self.voice_search, 34., cx).w(px(280.)))
            .child(
                div().flex().gap(px(4.)).children(
                    [
                        (LangFilter::All, "lang-all", tr!("全部", "All")),
                        (LangFilter::Cn, "lang-cn", "CN"),
                        (LangFilter::Jp, "lang-jp", "JP"),
                    ]
                    .into_iter()
                    .map(|(filter, id, label)| {
                        chip(id, lang == filter, true)
                            .child(label)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.character.lang = filter;
                                cx.notify();
                            }))
                    }),
                ),
            );

        let selected = self.character.voice_selected.len();
        let bar = (selected > 0).then(|| {
            action_bar(
                div()
                    .flex()
                    .items_center()
                    .gap(px(16.))
                    .child(selected_label(selected, tr!("个音频包", "audio packs")))
                    .child(
                        div()
                            .text_size(px(SIZE_CONTROL))
                            .text_color(hsla(TEXT_MUTED))
                            .child(lang.label()),
                    )
                    .child(
                        ghost_button("voice-clear", tr!("取消选择", "Clear selection")).on_click(
                            cx.listener(|this, _, _, cx| {
                                this.character.voice_selected.clear();
                                cx.notify();
                            }),
                        ),
                    )
                    .child(
                        primary_button("voice-export", tr!("导出…", "Export…"), true)
                            .on_click(cx.listener(|this, _, _, cx| this.open_voice_export(cx))),
                    ),
            )
        });
        div()
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .child(toolbar)
            .child(div().flex_1().min_h_0().child(page_scroll(
                "voice-list",
                &self.character.voice_scroll,
                list,
            )))
            .children(bar)
            .into_any_element()
    }

    fn voice_status(&self, row: &VoiceRow) -> Option<RowStatus> {
        let statuses: Vec<RowStatus> = row
            .sources
            .iter()
            .filter_map(|&i| {
                self.state
                    .row_status
                    .get(&self.state.sources[i].path)
                    .copied()
            })
            .collect();
        let find = |wanted: fn(&RowStatus) -> bool| statuses.iter().find(|s| wanted(s)).copied();
        find(|s| matches!(s, RowStatus::Decoding))
            .or_else(|| find(|s| matches!(s, RowStatus::Queued)))
            .or_else(|| find(|s| matches!(s, RowStatus::Failed)))
            .or_else(|| find(|s| matches!(s, RowStatus::Warning(_))))
            .or_else(|| find(|s| matches!(s, RowStatus::Done)))
            .or_else(|| find(|s| matches!(s, RowStatus::Reused)))
            .or_else(|| statuses.first().copied())
    }

    fn render_voice_row(&self, row: &VoiceRow, cx: &mut Context<Self>) -> AnyElement {
        let selected = self.character.voice_selected.contains(&row.key);
        let key = row.key.clone();
        let status = self.voice_status(row);
        let status_color = match status {
            Some(RowStatus::Done | RowStatus::Reused) => SUCCESS_TEXT,
            Some(RowStatus::Failed) => DANGER,
            Some(RowStatus::Warning(_)) => WARNING,
            _ => TEXT_MUTED,
        };
        row_frame(
            SharedString::from(format!("voice-{}", row.key)),
            selected,
            ROW_VOICE,
        )
        .child(check_glyph(selected).when(!selected, |this| this.opacity(0.7)))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(3.))
                .child(
                    div()
                        .text_size(px(SIZE_ROW_TITLE))
                        .font_weight(if selected {
                            FontWeight::SEMIBOLD
                        } else {
                            FontWeight::MEDIUM
                        })
                        .child(one_line(row.title.clone())),
                )
                .child(mono_note(row.key.clone())),
        )
        .child(
            div()
                .flex_shrink_0()
                .text_size(px(SIZE_CONTROL))
                .text_color(hsla(TEXT_MUTED))
                .child(row.languages.clone()),
        )
        .child(
            div()
                .w(px(96.))
                .flex_shrink_0()
                .text_size(px(SIZE_CONTROL))
                .text_color(hsla(TEXT_FAINT))
                .child(row.packaging.clone()),
        )
        .child(
            div()
                .w(px(132.))
                .flex_shrink_0()
                .flex()
                .justify_end()
                .text_size(px(SIZE_CONTROL))
                .text_color(hsla(status_color))
                .children(status.map(|status| status.label())),
        )
        .on_click(cx.listener(move |this, _, _, cx| {
            let selected = &mut this.character.voice_selected;
            if !selected.remove(&key) {
                selected.insert(key.clone());
            }
            cx.notify();
        }))
        .into_any_element()
    }

    fn open_voice_export(&mut self, cx: &mut Context<Self>) {
        let Some((data, index)) = self.current_entry() else {
            return;
        };
        let entry = &data.entries[index];
        let lang = self.character.lang;
        let selected = &self.character.voice_selected;
        let sources: Vec<SourceInfo> = entry
            .voice
            .iter()
            .filter_map(|&i| self.state.sources.get(i))
            .filter(|source| selected.contains(&source.key) && lang.accepts(source.language))
            .cloned()
            .collect();
        let count = selected.len();
        self.open_export(
            ExportPanel {
                kind: ExportKind::Audio {
                    sources,
                    character: entry.resource.clone(),
                },
                title: tr!(
                    "导出 {} 个音频包",
                    "Export {} audio packs",
                    fmt_count(count)
                ),
                subtitle: format!("{} · {}", entry.display_name(), lang.label()),
                destination: entry.resource.iter().cloned().collect(),
                thumbs: Vec::new(),
                agree: false,
                scope: None,
            },
            cx,
        );
    }

    // -----------------------------------------------------------------------
    // Story tab.

    /// Search positions of the entry's lines in the current scope and query.
    fn visible_story_lines(&self, entry: &CharEntry) -> Vec<usize> {
        let Some(data) = self.state.story_data() else {
            return Vec::new();
        };
        let query = self.character.story_query.trim().to_lowercase();
        entry
            .lines
            .iter()
            .copied()
            .filter(|&i| {
                let line = &data.search[i];
                self.character
                    .scope
                    .accepts(data.books[line.book].label.starts_with("CN_"))
                    && (query.is_empty() || line.text.to_lowercase().contains(&query))
            })
            .collect()
    }

    fn set_story_scope(&mut self, scope: ScriptScope, cx: &mut Context<Self>) {
        self.character.scope = scope;
        self.character.story_drawer = None;
        match scope {
            ScriptScope::Cn => self.settings.story_scope = StoryScope::Cn,
            ScriptScope::All => self.settings.story_scope = StoryScope::All,
            // Saved scope is CN or all; the original-region view is per session.
            ScriptScope::Original => {}
        }
        self.save_settings();
        cx.notify();
    }

    fn render_story_tab(
        &mut self,
        entry: &CharEntry,
        main_width: f32,
        pad: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(data) = self.state.story_data().cloned() else {
            return empty_state(tr!(
                "剧情索引尚未载入。",
                "The story index is not loaded yet."
            ));
        };
        let lines = self.visible_story_lines(entry);
        let mut rows: Vec<StoryRow> = Vec::with_capacity(lines.len() + 16);
        let mut last_book = None;
        for &line in &lines {
            let book = data.search[line].book;
            if last_book != Some(book) {
                last_book = Some(book);
                rows.push(StoryRow::Script { book });
            }
            rows.push(StoryRow::Line(line));
        }
        let rows = Rc::new(rows);
        let scope = self.character.scope;
        let toolbar = div()
            .flex()
            .flex_wrap()
            .flex_shrink_0()
            .items_center()
            .gap(px(12.))
            .pt(px(20.))
            .pb(px(8.))
            .px(px(pad))
            .child(
                // The search field shrinks first; the export link keeps its
                // place at the right and wraps below only as a last resort.
                div()
                    .flex()
                    .flex_1()
                    .min_w(px(STORY_TOOLS_MIN))
                    .items_center()
                    .gap(px(12.))
                    .child(
                        search_field(&self.story_tab_search, 34., cx)
                            .flex_1()
                            .min_w(px(STORY_SEARCH_MIN))
                            .max_w(px(STORY_SEARCH_MAX)),
                    )
                    .child(
                        div().flex().flex_shrink_0().gap(px(4.)).children(
                            [
                                (ScriptScope::Cn, "scope-cn"),
                                (ScriptScope::Original, "scope-original"),
                                (ScriptScope::All, "scope-all"),
                            ]
                            .into_iter()
                            .map(|(option, id)| {
                                chip(id, scope == option, true)
                                    .child(option.label())
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.set_story_scope(option, cx);
                                    }))
                            }),
                        ),
                    ),
            )
            .when(!lines.is_empty(), |this| {
                this.child(
                    link(
                        "story-export-all",
                        tr!("导出全部台词…", "Export all lines…"),
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.open_character_story_export(cx))),
                )
            });

        let drawer = self.render_line_drawer(DRAWER_WIDTH, window, cx);
        let content_width = main_width - drawer.as_ref().map_or(0., |_| DRAWER_WIDTH);
        let text_width =
            (content_width - pad * 2. - LINE_NUMBER - LINE_VOICES - LINE_GAP * 2.).max(120.);
        let key = (
            entry.key.clone(),
            scope,
            self.character.story_query.trim().to_owned(),
            text_width as u32,
        );
        let list_for = &mut self.character.story_list_for;
        if list_for.as_ref() != Some(&key) {
            // Only a change of width keeps the reading position.
            let same_rows = list_for
                .as_ref()
                .is_some_and(|old| (&old.0, old.1, &old.2) == (&key.0, key.1, &key.2));
            *list_for = Some(key);
            let list = &self.character.story_list;
            let top = same_rows.then(|| list.logical_scroll_top());
            list.reset(rows.len());
            if let Some(top) = top {
                list.scroll_to(top);
            }
        }

        let list: AnyElement = if rows.is_empty() {
            empty_state(if self.character.story_query.trim().is_empty() {
                tr!(
                    "这个范围内没有该角色的台词。",
                    "This character has no lines in this scope."
                )
            } else {
                tr!("没有符合搜索条件的台词。", "No lines match the search.")
            })
        } else {
            let weak = cx.entity().downgrade();
            let selected = self.character.story_drawer;
            let rows = rows.clone();
            let story = data.clone();
            let body = list(self.character.story_list.clone(), move |position, _, cx| {
                story_row(
                    &rows[position],
                    &story,
                    selected,
                    pad,
                    text_width,
                    &weak,
                    cx,
                )
            })
            .size_full()
            .pb(px(90.));
            div()
                .id("story-lines-area")
                .size_full()
                .relative()
                .child(body)
                .vertical_scrollbar(&self.character.story_list)
                .into_any_element()
        };

        div()
            .size_full()
            .relative()
            .flex()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(toolbar)
                    .child(div().flex_1().min_h_0().child(list)),
            )
            .children(drawer)
            .into_any_element()
    }

    /// Linked lines of a book (kept for the most recent book).
    pub(super) fn linked_book(&mut self, book: usize) -> Option<Rc<Linked>> {
        if let Some((cached, linked)) = &self.character.linked
            && *cached == book
        {
            return Some(linked.clone());
        }
        let data = self.state.story_data()?.clone();
        let linked = Rc::new(
            data.index
                .link(&Selection::Script(data.books[book].book.clone())),
        );
        self.character.linked = Some((book, linked.clone()));
        Some(linked)
    }

    fn render_line_drawer(
        &mut self,
        width: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let position = self.character.story_drawer?;
        let body = self.line_drawer_body(position, width, window, cx)?;
        let close = cx.listener(|this: &mut Self, _: &gpui::ClickEvent, _, cx| {
            this.character.story_drawer = None;
            cx.notify();
        });
        let data = self.state.story_data()?.clone();
        let line = data.search.get(position)?;
        let info = &data.books[line.book];
        Some(
            drawer_frame(
                width,
                tr!(
                    "{} · {} · 第 {} 行",
                    "{} · {} · line {}",
                    tree_label(&info.chapter),
                    info.label,
                    line.row
                ),
                close,
                body,
            )
            .into_any_element(),
        )
    }

    /// Drawer content for a story line: shared by the character tab and the
    /// chapter reader.
    pub(super) fn line_drawer_body(
        &mut self,
        position: usize,
        width: f32,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let data = self.state.story_data()?.clone();
        let entry = data.search.get(position)?;
        let book_name = data.books[entry.book].book.clone();
        let row = entry.row;
        let linked = self.linked_book(entry.book)?;
        let index = linked.lines.iter().position(|l| l.identity.row == row)?;
        let line = &linked.lines[index];
        let previous = index.checked_sub(1).map(|p| &linked.lines[p]);
        let text_width = width - 56.;

        let previous_block = previous.map(|prev| {
            let speaker = prev
                .speaker
                .as_deref()
                .map(display_speaker)
                .unwrap_or_else(|| narrator_label().to_owned());
            let text = multi_line_text(prev.text.as_deref().unwrap_or_default());
            div()
                .flex()
                .flex_col()
                .gap(px(4.))
                .child(
                    div()
                        .text_size(px(SIZE_GROUP))
                        .text_color(hsla(TEXT_FAINT))
                        .child(tr!(
                            "上一句 · {speaker}",
                            "Previous line · {speaker}",
                            speaker = speaker
                        )),
                )
                .child(
                    div()
                        .text_size(px(SIZE_BODY))
                        .text_color(hsla(TEXT_MUTED))
                        .child(wrap_for_display(&text, text_width, SIZE_BODY, cx)),
                )
        });

        let voices = voice_lines(line);
        let voice_block = (!voices.is_empty()).then(|| {
            div()
                .flex()
                .flex_col()
                .gap(px(10.))
                .child(group_title(tr!("语音", "Voices")))
                .children(voices.into_iter().map(|(label, value)| {
                    div()
                        .flex()
                        .items_center()
                        .gap(px(12.))
                        .child(
                            div()
                                .w(px(38.))
                                .flex_shrink_0()
                                .whitespace_nowrap()
                                .text_size(px(SIZE_CONTROL))
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(label),
                        )
                        .child(mono_note(value).flex_1().text_color(hsla(TEXT_MUTED)))
                }))
        });

        let open = self.character.drawer_sources_open;
        let source_rows = source_chain(line);
        let sources = div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .text_size(px(SIZE_CONTROL))
            .child(
                div()
                    .id("drawer-sources")
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .cursor_pointer()
                    .text_color(hsla(TEXT_FAINT))
                    .child(icon(
                        if open {
                            "app/chevron-down.svg"
                        } else {
                            "app/chevron-right.svg"
                        },
                        12.,
                        TEXT_FAINT,
                    ))
                    .child(tr!("数据来源", "Data source"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.character.drawer_sources_open = !this.character.drawer_sources_open;
                        cx.notify();
                    })),
            )
            .when(open, |this| {
                this.child(mono(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(4.))
                        .text_size(px(SIZE_MONO))
                        .line_height(px(19.))
                        .text_color(hsla(TEXT_MUTED))
                        .children(
                            source_rows
                                .into_iter()
                                .map(|row| div().child(row).into_any_element()),
                        ),
                ))
            });

        let primary = multi_line_text(entry.primary());
        let alternate = line
            .text_alt
            .clone()
            .filter(|alt| Some(alt) != line.text.as_ref());
        let speaker = entry.speaker.label();
        Some(
            div()
                .size_full()
                .flex()
                .flex_col()
                .child(
                    div().flex_1().min_h_0().child(page_scroll(
                        "line-drawer",
                        &self.character.drawer_scroll,
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(22.))
                            .px(px(28.))
                            .pt(px(6.))
                            .pb(px(20.))
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap(px(8.))
                                    .child(
                                        div()
                                            .text_size(px(SIZE_GROUP))
                                            .text_color(hsla(TEXT_MUTED))
                                            .child(speaker),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(18.))
                                            .line_height(px(30.))
                                            .font_weight(FontWeight::MEDIUM)
                                            .child(wrap_for_display(&primary, text_width, 18., cx)),
                                    )
                                    .children(alternate.map(|alt| {
                                        div()
                                            .text_size(px(SIZE_CONTROL))
                                            .text_color(hsla(TEXT_MUTED))
                                            .child(wrap_for_display(
                                                &multi_line_text(&tr!(
                                                    "备选文本：{alt}",
                                                    "Alternate text: {alt}",
                                                    alt = alt
                                                )),
                                                text_width,
                                                SIZE_CONTROL,
                                                cx,
                                            ))
                                    })),
                            )
                            .children(previous_block)
                            .children(voice_block)
                            .child(sources),
                    )),
                )
                .child(
                    div()
                        .flex_shrink_0()
                        .px(px(28.))
                        .pb(px(24.))
                        .pt(px(8.))
                        .child(
                            soft_button(
                                "drawer-open-chapter",
                                tr!("在章节中查看上下文", "View in chapter"),
                                true,
                            )
                            .w_full()
                            .h(px(40.))
                            .rounded(px(10.))
                            .text_size(px(SIZE_BODY))
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    this.open_chapter(book_name.clone(), Some(row), cx);
                                },
                            )),
                        ),
                )
                .into_any_element(),
        )
    }

    fn open_character_story_export(&mut self, cx: &mut Context<Self>) {
        let Some((roster, index)) = self.current_entry() else {
            return;
        };
        let Some(data) = self.state.story_data().cloned() else {
            return;
        };
        let entry = &roster.entries[index];
        let scope = self.character.scope;
        // The export covers the whole scope, not the search term.
        let positions: Vec<usize> = entry
            .lines
            .iter()
            .copied()
            .filter(|&i| scope.accepts(data.books[data.search[i].book].label.starts_with("CN_")))
            .collect();
        let books: BTreeSet<usize> = positions.iter().map(|&i| data.search[i].book).collect();
        let name = entry.resource.clone().unwrap_or_else(|| entry.key.clone());
        let speakers: BTreeSet<String> = entry.speaker_names.iter().cloned().collect();
        let filter: LineFilter = Arc::new(move |line: &Line| {
            line.speaker
                .as_ref()
                .is_some_and(|speaker| speakers.contains(speaker))
                && scope.accepts(line.identity.script.starts_with("CN_"))
        });
        let selection = Selection::Character {
            name: name.clone(),
            books: books
                .iter()
                .map(|&book| data.books[book].book.clone())
                .collect(),
        };
        self.open_export(
            ExportPanel {
                kind: ExportKind::Story {
                    selection,
                    filter: Some(filter),
                },
                title: tr!(
                    "导出 {} 条台词",
                    "Export {} lines",
                    fmt_count(positions.len())
                ),
                subtitle: format!("{} · {}", entry.display_name(), scope.label()),
                destination: vec!["Story".to_owned(), name],
                thumbs: Vec::new(),
                agree: false,
                scope: None,
            },
            cx,
        );
    }

    // -----------------------------------------------------------------------
    // Portrait tab.

    fn render_portrait_tab(
        &mut self,
        entry: &CharEntry,
        main_width: f32,
        pad: f32,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(portrait) = self.state.portrait_data().cloned() else {
            return empty_state(tr!(
                "立绘索引尚未载入。",
                "The portrait index is not loaded yet."
            ));
        };
        let sizes: BTreeSet<(usize, usize)> = entry
            .portraits
            .iter()
            .filter_map(|&i| portrait.index.entries.get(i))
            .map(|e| (e.portrait.width(), e.portrait.height()))
            .collect();
        let size_note = match sizes.iter().next() {
            Some((w, h)) if sizes.len() == 1 => tr!(
                "原尺寸透明 PNG · {w} × {h}",
                "Full-size transparent PNG · {w} × {h}",
                w = w,
                h = h
            ),
            _ => tr!("原尺寸透明 PNG", "Full-size transparent PNG").to_owned(),
        };
        let current = self.character.portrait_backdrop;
        let toolbar = div()
            .flex()
            .flex_shrink_0()
            .items_center()
            .gap(px(12.))
            .pt(px(20.))
            .pb(px(8.))
            .px(px(pad))
            .child(
                div()
                    .text_size(px(13.))
                    .text_color(hsla(TEXT_MUTED))
                    .child(size_note),
            )
            .child(div().flex_1())
            .child(
                div()
                    .text_size(px(SIZE_CONTROL))
                    .text_color(hsla(TEXT_FAINT))
                    .child(tr!("背景", "Backdrop")),
            )
            .child(
                div().flex().gap(px(4.)).children(
                    [
                        (Backdrop::Dark, "bg-dark", tr!("深色", "Dark")),
                        (Backdrop::Checker, "bg-checker", tr!("棋盘格", "Checker")),
                        (Backdrop::White, "bg-white", tr!("白", "White")),
                    ]
                    .into_iter()
                    .map(|(kind, id, label)| {
                        chip(id, current == kind, true)
                            .child(label)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.character.portrait_backdrop = kind;
                                cx.notify();
                            }))
                    }),
                ),
            );

        let content_width = (main_width - pad * 2.).max(200.);
        let columns: usize = if content_width >= 800. { 3 } else { 2 };
        let card =
            ((content_width - CARD_GAP * (columns - 1) as f32) / columns as f32).min(CARD_MAX);
        let indices = Rc::new(entry.portraits.clone());
        let resource = entry.resource.clone().unwrap_or_default();
        let rows = indices.len().div_ceil(columns);
        let list = uniform_list(
            "portrait-cards",
            rows,
            cx.processor(move |this, range: std::ops::Range<usize>, _, cx| {
                range
                    .map(|row| {
                        let cards: Vec<AnyElement> = indices
                            .iter()
                            .skip(row * columns)
                            .take(columns)
                            .map(|&index| this.render_portrait_card(index, &resource, card, cx))
                            .collect();
                        div()
                            .flex()
                            .gap(px(CARD_GAP))
                            .px(px(pad))
                            .pb(px(CARD_GAP))
                            .children(cards)
                            .into_any_element()
                    })
                    .collect()
            }),
        )
        .track_scroll(&self.character.portrait_scroll)
        .size_full()
        .pb(px(90.));

        let selected = self.character.portrait_selected.len();
        let bar = (selected > 0).then(|| {
            action_bar(
                div()
                    .flex()
                    .items_center()
                    .gap(px(16.))
                    .child(selected_label(selected, tr!("张立绘", "portraits")))
                    .child(
                        ghost_button("portrait-clear", tr!("取消选择", "Clear selection"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.character.portrait_selected.clear();
                                cx.notify();
                            })),
                    )
                    .child(
                        primary_button("portrait-export", tr!("导出…", "Export…"), true)
                            .on_click(cx.listener(|this, _, _, cx| this.open_portrait_export(cx))),
                    ),
            )
        });
        div()
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .child(toolbar)
            .child(
                div()
                    .id("portrait-area")
                    .flex_1()
                    .min_h_0()
                    .relative()
                    .pt(px(12.))
                    .child(list)
                    .vertical_scrollbar(&self.character.portrait_scroll),
            )
            .children(bar)
            .into_any_element()
    }

    fn render_portrait_card(
        &mut self,
        index: usize,
        resource: &str,
        size: f32,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(data) = self.state.portrait_data().cloned() else {
            return div().into_any_element();
        };
        let Some(entry) = data.index.entries.get(index) else {
            return div().into_any_element();
        };
        let thumb = self.portrait_thumb(index, cx);
        let selected = self.character.portrait_selected.contains(&index);
        let name = entry.portrait.name.clone();
        let label = portrait_label(&name, resource);
        let kind = self.character.portrait_backdrop;
        let image: AnyElement = match thumb {
            Thumb::Ready(path) => div()
                .absolute()
                .inset_0()
                .child(
                    img(path)
                        .size_full()
                        .rounded(px(CARD_RADIUS))
                        .object_fit(ObjectFit::Contain),
                )
                .into_any_element(),
            Thumb::Pending => div()
                .absolute()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(12.))
                .text_color(hsla(TEXT_FAINT))
                .child(tr!("载入中…", "Loading…"))
                .into_any_element(),
            Thumb::Failed(reason) => div()
                .absolute()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .p(px(16.))
                .text_size(px(12.))
                .text_color(hsla(WARNING))
                .child(reason)
                .into_any_element(),
        };
        div()
            .id(("portrait-card", index))
            .w(px(size))
            .flex()
            .flex_col()
            .gap(px(12.))
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, _, cx| {
                let selected = &mut this.character.portrait_selected;
                if !selected.remove(&index) {
                    selected.insert(index);
                }
                cx.notify();
            }))
            .child(
                div()
                    .relative()
                    .w(px(size))
                    .h(px(size))
                    .rounded(px(CARD_RADIUS))
                    .overflow_hidden()
                    .when_some(backdrop_fill(kind), |this, fill| this.bg(hsla(fill)))
                    .when(kind == Backdrop::Checker, |this| {
                        this.child(checker_backdrop(CARD_RADIUS))
                    })
                    .child(image)
                    .when(selected, |this| {
                        this.child(selection_ring(CARD_RADIUS, ACCENT)).child(
                            div()
                                .absolute()
                                .top(px(12.))
                                .left(px(12.))
                                .child(check_badge()),
                        )
                    }),
            )
            .child(
                div()
                    .h(px(CARD_CAPTION - 12.))
                    .flex()
                    .items_baseline()
                    .gap(px(10.))
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_size(px(SIZE_ROW_TITLE))
                            .font_weight(if selected {
                                FontWeight::SEMIBOLD
                            } else {
                                FontWeight::MEDIUM
                            })
                            .child(label),
                    )
                    .child(mono_note(name).flex_1()),
            )
            .into_any_element()
    }

    fn open_portrait_export(&mut self, cx: &mut Context<Self>) {
        let Some((data, index)) = self.current_entry() else {
            return;
        };
        let entry = &data.entries[index];
        let indices: Vec<usize> = self.character.portrait_selected.iter().copied().collect();
        let thumbs: Vec<PathBuf> = indices
            .iter()
            .filter_map(|i| match self.portrait.thumbs.get(i) {
                Some(Thumb::Ready(path)) => Some(path.clone()),
                _ => None,
            })
            .take(2)
            .collect();
        let count = indices.len();
        let folder = self.state.portrait_data().and_then(|portrait| {
            indices
                .first()
                .and_then(|&i| portrait.index.entries.get(i))
                .map(|e| e.character_resource.clone())
        });
        let mut destination = vec![crate::portrait::OUTPUT_FOLDER.to_owned()];
        destination.extend(folder);
        self.open_export(
            ExportPanel {
                kind: ExportKind::Portraits { indices },
                title: tr!("导出 {} 张立绘", "Export {} portraits", fmt_count(count)),
                subtitle: tr!(
                    "{} · 原尺寸透明 PNG",
                    "{} · Full-size transparent PNG",
                    entry.display_name()
                ),
                destination,
                thumbs,
                agree: false,
                scope: None,
            },
            cx,
        );
    }

    // -----------------------------------------------------------------------
    // Live2D tab.

    fn render_live2d_tab(
        &mut self,
        entry: &CharEntry,
        main_width: f32,
        pad: f32,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if self.state.live2d_data().is_none() {
            return empty_state(tr!(
                "Live2D 索引尚未载入。",
                "The Live2D index is not loaded yet."
            ));
        }
        let drawer = self.render_model_drawer(cx);
        let list_width = main_width - drawer.as_ref().map_or(0., |_| DRAWER_WIDTH);
        let content_width = (list_width - pad * 2.).max(200.);
        let columns =
            (((content_width + CARD_GAP) / (MODEL_CARD_THUMB + CARD_GAP)) as usize).max(1);
        let card = ((content_width - CARD_GAP * (columns - 1) as f32) / columns as f32)
            .min(MODEL_CARD_THUMB + 64.);
        let indices = Rc::new(entry.live2d.clone());
        let rows = indices.len().div_ceil(columns);
        let list = uniform_list(
            "live2d-cards",
            rows,
            cx.processor(move |this, range: std::ops::Range<usize>, _, cx| {
                range
                    .map(|row| {
                        let cards: Vec<AnyElement> = indices
                            .iter()
                            .skip(row * columns)
                            .take(columns)
                            .map(|&index| this.render_model_card(index, card, cx))
                            .collect();
                        div()
                            .flex()
                            .gap(px(CARD_GAP))
                            .px(px(pad))
                            .pb(px(CARD_GAP))
                            .children(cards)
                            .into_any_element()
                    })
                    .collect()
            }),
        )
        .track_scroll(&self.character.live2d_scroll)
        .size_full()
        .pb(px(90.));

        let selected = self.character.live2d_selected.len();
        let bar = (selected > 0).then(|| {
            action_bar(
                div()
                    .flex()
                    .items_center()
                    .gap(px(16.))
                    .child(selected_label(selected, tr!("个模型", "models")))
                    .child(
                        ghost_button("live2d-clear", tr!("取消选择", "Clear selection")).on_click(
                            cx.listener(|this, _, _, cx| {
                                this.character.live2d_selected.clear();
                                cx.notify();
                            }),
                        ),
                    )
                    .child(
                        primary_button("live2d-export", tr!("导出…", "Export…"), true)
                            .on_click(cx.listener(|this, _, _, cx| this.open_live2d_export(cx))),
                    ),
            )
        });
        div()
            .size_full()
            .relative()
            .flex()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .relative()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .id("live2d-area")
                            .flex_1()
                            .min_h_0()
                            .relative()
                            .pt(px(24.))
                            .child(list)
                            .vertical_scrollbar(&self.character.live2d_scroll),
                    )
                    .children(bar),
            )
            .children(drawer)
            .into_any_element()
    }

    fn render_model_card(
        &mut self,
        index: usize,
        width: f32,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(data) = self.state.live2d_data().cloned() else {
            return div().into_any_element();
        };
        let Some(entry) = data.index.entries.get(index) else {
            return div().into_any_element();
        };
        let thumb = self.live2d_thumb(index, 0, cx);
        let selected = self.character.live2d_selected.contains(&index);
        let focused = self.character.live2d_drawer == Some(index);
        let complete = entry.model.is_complete();
        let problems = entry.model.problems().len();
        let name = entry.model.name.clone();
        let preview: AnyElement = match thumb {
            Some(Thumb::Ready(path)) => img(path)
                .size_full()
                .rounded(px(MODEL_CARD_RADIUS))
                .object_fit(ObjectFit::Contain)
                .into_any_element(),
            Some(Thumb::Failed(reason)) => div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .p(px(12.))
                .text_size(px(11.5))
                .text_color(hsla(WARNING))
                .child(reason)
                .into_any_element(),
            Some(Thumb::Pending) => div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(12.))
                .text_color(hsla(TEXT_FAINT))
                .child(tr!("载入中…", "Loading…"))
                .into_any_element(),
            None => div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(12.))
                .text_color(hsla(TEXT_FAINT))
                .child(tr!("没有贴图", "No texture"))
                .into_any_element(),
        };
        div()
            .id(("model-card", index))
            .w(px(width))
            .flex()
            .flex_col()
            .gap(px(10.))
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, _, cx| {
                let selected = &mut this.character.live2d_selected;
                if !selected.remove(&index) {
                    selected.insert(index);
                }
                cx.notify();
            }))
            .child(
                div()
                    .relative()
                    .h(px(MODEL_CARD_THUMB))
                    .rounded(px(MODEL_CARD_RADIUS))
                    .overflow_hidden()
                    .bg(hsla(BG_CARD))
                    .child(preview)
                    .when(selected || focused, |this| {
                        this.child(selection_ring(
                            MODEL_CARD_RADIUS,
                            if selected { ACCENT } else { FOCUS_RING },
                        ))
                    })
                    .child(
                        // State only: the whole card toggles the selection.
                        div()
                            .absolute()
                            .top(px(10.))
                            .left(px(10.))
                            .size(px(26.))
                            .rounded(px(6.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .bg(hsla_alpha(BG_APP, 0.72))
                            .child(check_glyph(selected)),
                    )
                    .child(
                        div()
                            .id(("model-details", index))
                            .absolute()
                            .top(px(10.))
                            .right(px(10.))
                            .size(px(26.))
                            .rounded(px(6.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .bg(hsla_alpha(BG_APP, 0.72))
                            .cursor_pointer()
                            .hover(|style| style.bg(hsla(BG_BUTTON_HOVER)))
                            .child(icon("app/info.svg", 15., TEXT_SECONDARY))
                            .tooltip(|window, cx| {
                                gpui_component::tooltip::Tooltip::new(tr!("详情", "Details"))
                                    .build(window, cx)
                            })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.character.live2d_drawer = Some(index);
                                cx.notify();
                            })),
                    ),
            )
            .child(
                div()
                    .h(px(MODEL_CARD_CAPTION - 10.))
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(mono_note(name).text_size(px(12.)).text_color(hsla(TEXT)))
                    .child(
                        div()
                            .text_size(px(SIZE_GROUP))
                            .text_color(hsla(if complete { TEXT_FAINT } else { WARNING }))
                            .child(if complete {
                                tr!("完整", "Complete").to_owned()
                            } else {
                                tr!(
                                    "不完整 · {problems} 个问题",
                                    "Incomplete · {problems} problems",
                                    problems = problems
                                )
                            }),
                    ),
            )
            .into_any_element()
    }

    fn render_model_drawer(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let index = self.character.live2d_drawer?;
        let data = self.state.live2d_data()?.clone();
        let entry = data.index.entries.get(index)?;
        let model = &entry.model;
        let mut rows: Vec<AnyElement> = Vec::new();
        for (directory, files) in file_tree(&model.files) {
            if let Some(directory) = &directory {
                rows.push(tree_row(
                    format!("{directory}/"),
                    String::new(),
                    true,
                    TEXT_FAINT,
                    0.,
                ));
            }
            let indent = if directory.is_some() { 18. } else { 0. };
            for file in files {
                let meta = match file.status {
                    FileStatus::Ok => match (&file.texture, file.size) {
                        (Some(texture), _) => {
                            format!("{} × {}", texture.info.width, texture.info.height)
                        }
                        (None, Some(size)) => format!("{} B", fmt_count(size as usize)),
                        (None, None) => String::new(),
                    },
                    status => status.label().to_owned(),
                };
                rows.push(tree_row(
                    file_name_of(&file.path).to_owned(),
                    meta,
                    false,
                    status_color(file.status),
                    indent,
                ));
            }
        }
        let problems: Vec<AnyElement> = model
            .problems()
            .into_iter()
            .map(|(problem, file)| {
                div()
                    .text_size(px(SIZE_MONO))
                    .text_color(hsla(WARNING_BODY))
                    .child(format!(
                        "{} · {}{}",
                        problem.label(),
                        file.path,
                        file.detail
                            .as_ref()
                            .map(|d| tr!("：{}", ": {}", d.text()))
                            .unwrap_or_default()
                    ))
                    .into_any_element()
            })
            .collect();
        let core = self.state.live2d_core.get(&model.name).map(core_line);
        let stat = |value: String, label: &'static str| {
            div()
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(
                    div()
                        .text_size(px(18.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(value),
                )
                .child(
                    div()
                        .text_size(px(SIZE_GROUP))
                        .text_color(hsla(TEXT_FAINT))
                        .child(label),
                )
        };
        let close = cx.listener(|this: &mut Self, _: &gpui::ClickEvent, _, cx| {
            this.character.live2d_drawer = None;
            cx.notify();
        });
        let body = page_scroll(
            "model-drawer",
            &self.character.drawer_scroll,
            div()
                .flex()
                .flex_col()
                .gap(px(20.))
                .px(px(28.))
                .pt(px(6.))
                .pb(px(24.))
                .child(
                    div()
                        .flex()
                        .gap(px(28.))
                        .child(stat(fmt_count(model.count(Role::Expression)), tr!("表情", "Expressions")))
                        .child(stat(fmt_count(model.count(Role::Motion)), tr!("动作", "Motions")))
                        .child(stat(fmt_count(model.files.len()), tr!("文件", "Files"))),
                )
                .children(core.map(|line| {
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(4.))
                        .child(group_title(tr!("Core 校验", "Core check")))
                        .child(
                            div()
                                .text_size(px(SIZE_CONTROL))
                                .text_color(hsla(SUCCESS_TEXT))
                                .child(line),
                        )
                }))
                .when(!problems.is_empty(), |this| {
                    this.child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(4.))
                            .p(px(12.))
                            .rounded(px(10.))
                            .bg(hsla(BG_WARN_PANEL))
                            .child(
                                div()
                                    .text_size(px(SIZE_GROUP))
                                    .text_color(hsla(WARNING))
                                    .child(tr!("不完整：以下文件不会导出", "Incomplete: these files will not be exported")),
                            )
                            .children(problems),
                    )
                })
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(8.))
                        .child(group_title(tr!("导出内容", "Export contents")))
                        .child(mono(div().flex().flex_col().text_size(px(12.)).children(rows))),
                )
                .child(
                    div()
                        .text_size(px(SIZE_GROUP))
                        .line_height(px(19.))
                        .text_color(hsla(TEXT_FAINT))
                        .child(tr!("导出的是运行时资源（model3.json、moc3、贴图、表情、动作），不是 Cubism Editor 的 .cmo3 工程。", "These are runtime resources (model3.json, moc3, textures, expressions, motions), not a Cubism Editor .cmo3 project.")),
                ),
        );
        Some(drawer_frame(DRAWER_WIDTH, model.name.clone(), close, body).into_any_element())
    }

    fn open_live2d_export(&mut self, cx: &mut Context<Self>) {
        let Some((data, index)) = self.current_entry() else {
            return;
        };
        let entry = &data.entries[index];
        let indices: Vec<usize> = self.character.live2d_selected.iter().copied().collect();
        let count = indices.len();
        let thumbs: Vec<PathBuf> = indices
            .iter()
            .filter_map(|&i| match self.live2d.thumbs.get(&(i, 0)) {
                Some(Thumb::Ready(path)) => Some(path.clone()),
                _ => None,
            })
            .take(2)
            .collect();
        self.open_export(
            ExportPanel {
                kind: ExportKind::Live2d { indices },
                title: tr!(
                    "导出 {} 个 Live2D 模型",
                    "Export {} Live2D models",
                    fmt_count(count)
                ),
                subtitle: tr!(
                    "{} · 运行时资源整包",
                    "{} · Runtime resource package",
                    entry.display_name()
                ),
                destination: vec![crate::live2d::OUTPUT_FOLDER.to_owned()],
                thumbs,
                agree: false,
                scope: None,
            },
            cx,
        );
    }

    /// Keeps a search input's text in the matching view field.
    pub(super) fn voice_search_changed(
        &mut self,
        input: &Entity<InputState>,
        cx: &mut Context<Self>,
    ) {
        self.character.voice_query = input.read(cx).value().to_string();
        cx.notify();
    }

    pub(super) fn story_tab_search_changed(
        &mut self,
        input: &Entity<InputState>,
        cx: &mut Context<Self>,
    ) {
        self.character.story_query = input.read(cx).value().to_string();
        cx.notify();
    }

    pub(super) fn go_to_character_story(&mut self, key: String, cx: &mut Context<Self>) {
        self.page = Page::Character;
        self.character.tab = CharTab::Story;
        self.select_character(key, cx);
    }
}

/// One row of the story tab: a script heading or a line wrapped to the
/// available width.
fn story_row(
    row: &StoryRow,
    data: &Arc<StoryData>,
    selected: Option<usize>,
    pad: f32,
    text_width: f32,
    view: &WeakEntity<EchoArchive>,
    cx: &mut App,
) -> AnyElement {
    match row {
        StoryRow::Script { book } => {
            let book = *book;
            let info = &data.books[book];
            let book_name = info.book.clone();
            let view = view.clone();
            div()
                .w_full()
                .h(px(ROW_LINE))
                .px(px(pad))
                .flex()
                .items_end()
                .gap(px(10.))
                .pb(px(8.))
                .child(
                    div()
                        .flex_shrink_0()
                        .text_size(px(13.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(hsla(TEXT_SECONDARY))
                        .child(tree_label(&info.chapter)),
                )
                .child(mono_note(info.label.clone()).text_size(px(12.)))
                .child(div().flex_1())
                .child(
                    link(
                        SharedString::from(format!("story-open-{book}")),
                        tr!("在章节中阅读 →", "Read in chapter →"),
                    )
                    .text_size(px(12.))
                    .on_click(move |_, _, cx| {
                        let book_name = book_name.clone();
                        let _ = view.update(cx, move |this, cx| {
                            this.open_chapter(book_name, None, cx);
                        });
                    }),
                )
                .into_any_element()
        }
        StoryRow::Line(index) => {
            let index = *index;
            let line = &data.search[index];
            let is_selected = selected == Some(index);
            let voices = match (line.cn, line.jp) {
                (true, true) => "CN · JP",
                (true, false) => "CN",
                (false, true) => "JP",
                (false, false) => "",
            };
            let view = view.clone();
            div()
                .px(px(pad))
                .child(
                    row_frame(
                        SharedString::from(format!("line-{index}")),
                        is_selected,
                        ROW_LINE - 2.,
                    )
                    .h_auto()
                    .min_h(px(ROW_LINE - 2.))
                    .py(px(11.))
                    .items_start()
                    .gap(px(LINE_GAP))
                    .child(mono(
                        div()
                            .w(px(LINE_NUMBER))
                            .flex_shrink_0()
                            .text_size(px(12.))
                            .line_height(px(LINE_HEIGHT))
                            .text_color(hsla(if is_selected {
                                ACCENT_HOVER
                            } else {
                                TEXT_FAINT
                            }))
                            .child(line.row.to_string()),
                    ))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(px(15.))
                            .line_height(px(LINE_HEIGHT))
                            .text_color(hsla(if is_selected { TEXT } else { TEXT_READING }))
                            .child(wrap_for_display(
                                &multi_line_text(line.primary()),
                                text_width,
                                15.,
                                cx,
                            )),
                    )
                    .child(
                        div()
                            .w(px(LINE_VOICES))
                            .flex_shrink_0()
                            .flex()
                            .justify_end()
                            .text_size(px(12.))
                            .line_height(px(LINE_HEIGHT))
                            .text_color(hsla(if is_selected { TEXT_MUTED } else { TEXT_FAINT }))
                            .child(voices),
                    )
                    .on_click(move |_, _, cx| {
                        let _ = view.update(cx, move |this, cx| {
                            this.character.story_drawer = Some(index);
                            this.character.drawer_sources_open = false;
                            cx.notify();
                        });
                    }),
                )
                .into_any_element()
        }
    }
}

fn tree_row(
    name: String,
    meta: String,
    directory: bool,
    meta_color: Color,
    indent: f32,
) -> AnyElement {
    div()
        .flex()
        .items_center()
        .gap(px(8.))
        .h(px(26.))
        .pl(px(indent))
        .text_color(hsla(if directory { TEXT } else { TEXT_SECONDARY }))
        .child(icon(
            if directory {
                "app/folder.svg"
            } else {
                "app/file.svg"
            },
            13.,
            if directory { TEXT_MUTED } else { TEXT_FAINT },
        ))
        .child(div().flex_1().min_w_0().child(one_line(name)))
        .child(
            div()
                .flex_shrink_0()
                .text_color(hsla(meta_color))
                .child(meta),
        )
        .into_any_element()
}

const ART_SIZE: f32 = 420.;
/// Space kept between the top of the header and the figure.
const ART_MARGIN: f32 = 12.;
/// Pixels at or above this alpha count as part of the figure.
const ART_OPAQUE: u8 = 128;
/// A row belongs to the head once this share of the image width is opaque;
/// thin raised weapons and ornaments stay below it.
const ART_HEAD_SHARE: f32 = 0.05;

/// Top of the figure as a share of the image height: the first row wide
/// enough to be the head rather than a weapon tip or a spark.
fn figure_top(image: &image::RgbaImage) -> f32 {
    let needed = (image.width() as f32 * ART_HEAD_SHARE).ceil() as usize;
    (0..image.height())
        .find(|&y| {
            (0..image.width())
                .filter(|&x| image.get_pixel(x, y)[3] >= ART_OPAQUE)
                .count()
                >= needed
        })
        .map_or(0.0, |y| y as f32 / image.height() as f32)
}

/// Vertical position of the header art so the figure's top sits just below
/// the header's top edge, and the image still covers the header's height.
fn art_offset(figure_top: f32, header_height: f32) -> f32 {
    (ART_MARGIN - figure_top * ART_SIZE).clamp(header_height - ART_SIZE, ART_MARGIN)
}

/// Card colour for the plain backdrops (the checkerboard is drawn).
fn backdrop_fill(kind: Backdrop) -> Option<Color> {
    match kind {
        Backdrop::Dark => Some(ART_BACKDROP_DARK),
        Backdrop::White => Some(ART_BACKDROP_WHITE),
        Backdrop::Checker => None,
    }
}

/// Variant part of a portrait name: `Avatar_Whole_CharolicSR01` → `SR01`.
fn portrait_label(name: &str, resource: &str) -> String {
    let prefix = format!("{}{resource}", crate::portrait::index::NAME_PREFIX);
    let rest = name
        .get(..prefix.len())
        .filter(|head| head.eq_ignore_ascii_case(&prefix))
        .map_or(name, |_| &name[prefix.len()..])
        .trim_start_matches('_');
    if rest.is_empty() {
        tr!("基础", "Base").to_owned()
    } else {
        rest.to_owned()
    }
}

/// (label, `bank / cue`) rows of a line's voices, default variants first.
fn voice_lines(line: &Line) -> Vec<(String, String)> {
    let mut rows: Vec<(String, String)> = Vec::new();
    for variant in ["default", "alternate"] {
        for voice in line.voices.iter().filter(|v| v.variant == variant) {
            let label = if variant == "default" {
                voice.language.to_owned()
            } else {
                tr!("{}备", "{} alt", voice.language)
            };
            let value = match (&voice.bank, &voice.cue) {
                (Some(bank), Some(cue)) => format!("{bank} / {cue}"),
                _ => tr!(
                    "未解析（{}={}）",
                    "Unresolved ({}={})",
                    voice.column,
                    voice.audio_id
                ),
            };
            if !rows.iter().any(|(l, v)| *l == label && *v == value) {
                rows.push((label, value));
            }
        }
    }
    rows
}

/// Where a line's speaker, text and voices come from.
fn source_chain(line: &Line) -> Vec<String> {
    let cell = |key: &str| line.raw_cells.get(key).unwrap_or_default().to_owned();
    let speaker_id = cell("Arg1");
    let mut rows = vec![
        if speaker_id.is_empty() {
            tr!("Arg1 为空（旁白）", "Arg1 is empty (narrator)").to_owned()
        } else if speaker_id.bytes().all(|b| b.is_ascii_digit()) {
            tr!(
                "Arg1={speaker_id} → AvgContent → 说话人",
                "Arg1={speaker_id} → AvgContent → speaker",
                speaker_id = speaker_id
            )
        } else {
            tr!(
                "Arg1={speaker_id}（字面文本）",
                "Arg1={speaker_id} (literal text)",
                speaker_id = speaker_id
            )
        },
        tr!(
            "Text={} → AvgContent → 中文语言表",
            "Text={} → AvgContent → Chinese language table",
            line.content_id
        ),
    ];
    let mut seen = BTreeSet::new();
    for voice in &line.voices {
        let row = format!("{}={} → AudioData", voice.column, voice.audio_id);
        if seen.insert(row.clone()) {
            rows.push(row);
        }
    }
    rows.push(tr!("表格 {}", "Sheet {}", line.identity.sheet));
    rows.push(tr!(
        "资源包 {} @ {} · PathID {}",
        "Bundle {} @ {} · PathID {}",
        line.identity.source_bundle,
        line.identity.source_offset,
        line.identity.source_path_id
    ));
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|name| (*name).to_owned()).collect()
    }

    #[test]
    fn voice_titles_drop_prefix_and_character_tokens() {
        let charolic = names(&["Charolic", "Krolik"]);
        assert_eq!(voice_title("VO_Charolic", &charolic), "基础语音");
        assert_eq!(voice_title("VO_Bedroom_Charolic", &charolic), "Bedroom");
        assert_eq!(
            voice_title("VO_Lobby_Exclam_krolik", &charolic),
            "Lobby_Exclam"
        );
        assert_eq!(voice_title("C_SR01_Charolic", &charolic), "C_SR01");
        assert_eq!(voice_title("vo_ServR_CHAROLIC", &charolic), "ServR");
    }

    #[test]
    fn header_art_starts_at_the_figure_and_ignores_thin_tips() {
        let mut image = image::RgbaImage::new(100, 100);
        // A thin weapon tip above a wide head.
        for y in 5..20 {
            image.put_pixel(50, y, image::Rgba([1, 1, 1, 255]));
        }
        for y in 20..60 {
            for x in 30..70 {
                image.put_pixel(x, y, image::Rgba([1, 1, 1, 255]));
            }
        }
        assert!((figure_top(&image) - 0.2).abs() < 1e-6);
        assert_eq!(figure_top(&image::RgbaImage::new(10, 10)), 0.0);
        assert!((art_offset(0.2, 196.) - (12. - 84.)).abs() < 1e-6);
        // Never above the header's top, never short of its bottom.
        assert_eq!(art_offset(0.0, 196.), 12.);
        assert_eq!(art_offset(0.9, 196.), 196. - 420.);
    }

    #[test]
    fn language_filter_always_keeps_common_packs() {
        assert!(LangFilter::Cn.accepts(Language::Common));
        assert!(LangFilter::Jp.accepts(Language::Common));
        assert!(!LangFilter::Cn.accepts(Language::Jp));
        assert!(!LangFilter::Jp.accepts(Language::Cn));
        assert!(LangFilter::All.accepts(Language::Jp));
    }

    #[test]
    fn script_scopes_split_cn_and_original() {
        assert!(ScriptScope::Cn.accepts(true) && !ScriptScope::Cn.accepts(false));
        assert!(ScriptScope::Original.accepts(false) && !ScriptScope::Original.accepts(true));
        assert!(ScriptScope::All.accepts(true) && ScriptScope::All.accepts(false));
    }

    #[test]
    fn portrait_labels_keep_only_the_variant() {
        assert_eq!(
            portrait_label("Avatar_Whole_CharolicSR01", "Charolic"),
            "SR01"
        );
        assert_eq!(portrait_label("Avatar_Whole_Charolic", "charolic"), "基础");
        assert_eq!(
            portrait_label("Avatar_Whole_NeuralCloud_Lind", ""),
            "NeuralCloud_Lind"
        );
    }
}
