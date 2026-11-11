//! Character switch panel: search, recently viewed, avatar grids by group
//! and the unassigned resources.

use super::{
    EchoArchive,
    character::page_padding,
    roster::{CharEntry, CharKind, RosterData, UNASSIGNED_KEY},
    theme::*,
    widgets::{clear_button, fmt_count, group_title, icon, page_scroll},
};
use gpui::{
    AnyElement, App, Context, FontWeight, InteractiveElement, IntoElement, ObjectFit,
    ParentElement, SharedString, StatefulInteractiveElement, Styled, StyledImage, Window, div, img,
    prelude::FluentBuilder, px,
};
use gpui_component::input::Input;
use std::sync::Arc;

const PANEL_WIDTH: f32 = 620.;
const COLUMNS: usize = 6;
const PAGE_MAX: f32 = 1040.;
const TILE_MIN: f32 = 90.;
const TILE_GAP: f32 = 8.;

fn initial(name: &str) -> String {
    name.chars().next().map(String::from).unwrap_or_default()
}

impl EchoArchive {
    pub(super) fn open_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.picker_open = true;
        self.picker_input.update(cx, |input, cx| {
            input.set_value("", window, cx);
            input.focus(window, cx);
        });
        self.picker_query.clear();
        cx.notify();
    }

    pub(super) fn close_picker(&mut self, cx: &mut Context<Self>) {
        self.picker_open = false;
        cx.notify();
    }

    fn pick_character(&mut self, key: String, cx: &mut Context<Self>) {
        self.picker_open = false;
        self.select_character(key, cx);
    }

    fn avatar_circle(&self, entry: &CharEntry, size: f32, current: bool) -> gpui::Div {
        let path = entry
            .resource
            .as_ref()
            .and_then(|resource| self.avatars.get(&resource.to_lowercase()));
        let base = div()
            .size(px(size))
            .flex_shrink_0()
            .rounded_full()
            .overflow_hidden()
            .flex()
            .items_center()
            .justify_center()
            .when(current, |this| this.border_2().border_color(hsla(ACCENT)));
        match path {
            Some(path) => base.bg(hsla(BG_FIELD_DEEP)).child(
                img(path.clone())
                    .size_full()
                    .rounded_full()
                    .object_fit(ObjectFit::Cover),
            ),
            None => base
                .bg(hsla(BG_INITIAL))
                .text_size(px(size * 0.3))
                .text_color(hsla(TEXT_MUTED))
                .child(initial(&entry.display_name())),
        }
    }

    fn avatar_tile(&self, entry: &CharEntry, width: f32, cx: &mut Context<Self>) -> AnyElement {
        let key = entry.key.clone();
        let current = self.character.current.as_deref() == Some(entry.key.as_str());
        div()
            .id(SharedString::from(format!("pick-{}", entry.key)))
            .flex()
            .flex_col()
            .items_center()
            .gap(px(6.))
            .w(px(width))
            .py(px(6.))
            .rounded(px(10.))
            .cursor_pointer()
            .hover(|style| style.bg(hsla(BG_BUTTON_HOVER)))
            .tooltip({
                let tooltip: SharedString = entry.tooltip_text().into();
                move |window, cx| {
                    gpui_component::tooltip::Tooltip::new(tooltip.clone()).build(window, cx)
                }
            })
            .child(self.avatar_circle(entry, 60., current))
            .child(
                div()
                    .text_size(px(SIZE_CONTROL))
                    .child(super::widgets::one_line(entry.display_name())),
            )
            .on_click(cx.listener(move |this, _, _, cx| this.pick_character(key.clone(), cx)))
            .into_any_element()
    }

    fn recent_chip(&self, entry: &CharEntry, cx: &mut Context<Self>) -> AnyElement {
        let key = entry.key.clone();
        let current = self.character.current.as_deref() == Some(entry.key.as_str());
        div()
            .id(SharedString::from(format!("recent-{}", entry.key)))
            .flex()
            .items_center()
            .gap(px(8.))
            .h(px(34.))
            .pl(px(4.))
            .pr(px(12.))
            .rounded(px(17.))
            .bg(hsla(if current { BG_CHIP_ACTIVE } else { BG_CHIP }))
            .text_size(px(13.))
            .text_color(hsla(if current { TEXT } else { TEXT_SECONDARY }))
            .cursor_pointer()
            .hover(|style| style.bg(hsla(BG_BUTTON_HOVER)))
            .child(self.avatar_circle(entry, 26., false))
            .child(entry.display_name())
            .on_click(cx.listener(move |this, _, _, cx| this.pick_character(key.clone(), cx)))
            .into_any_element()
    }

    fn picker_group(
        &self,
        title: String,
        entries: &[&CharEntry],
        tile: f32,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if entries.is_empty() {
            return None;
        }
        Some(
            div()
                .flex()
                .flex_col()
                .gap(px(10.))
                .child(group_title(title))
                .child(
                    div().flex().flex_wrap().gap(px(8.)).children(
                        entries
                            .iter()
                            .map(|entry| self.avatar_tile(entry, tile, cx)),
                    ),
                )
                .into_any_element(),
        )
    }

    /// Groups of the panel and the page, plus the unassigned-resources row.
    fn picker_sections(
        &mut self,
        tile: f32,
        cx: &mut Context<Self>,
    ) -> (Vec<AnyElement>, Option<gpui::Stateful<gpui::Div>>) {
        let data: Option<Arc<RosterData>> = self.roster.clone();
        let needle = self.picker_query.trim().to_lowercase();
        let mut sections: Vec<AnyElement> = Vec::new();
        let mut unassigned_note = None;
        if let Some(data) = &data {
            if needle.is_empty() {
                let recent: Vec<&CharEntry> = self
                    .settings
                    .recent_characters
                    .iter()
                    .filter_map(|key| data.find(key))
                    .map(|index| &data.entries[index])
                    .collect();
                if !recent.is_empty() {
                    sections.push(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(10.))
                            .child(group_title(tr!("最近查看", "Recent")))
                            .child(
                                div().flex().flex_wrap().gap(px(8.)).children(
                                    recent.iter().map(|entry| self.recent_chip(entry, cx)),
                                ),
                            )
                            .into_any_element(),
                    );
                }
            }
            let of_kind = |kind: CharKind| -> Vec<&CharEntry> {
                data.entries
                    .iter()
                    .filter(|entry| entry.kind == kind && entry.matches(&needle))
                    .collect()
            };
            sections.extend(self.picker_group(
                tr!("可操控角色", "Playable characters").to_owned(),
                &of_kind(CharKind::Playable),
                tile,
                cx,
            ));
            sections.extend(self.picker_group(
                tr!("其他角色", "Other characters").to_owned(),
                &of_kind(CharKind::Other),
                tile,
                cx,
            ));
            sections.extend(self.picker_group(
                tr!("剧情人物", "Story characters").to_owned(),
                &of_kind(CharKind::StoryPerson),
                tile,
                cx,
            ));
            if let Some(index) = data.find(UNASSIGNED_KEY) {
                let entry = &data.entries[index];
                let parts: Vec<String> = [
                    (entry.voice.len(), tr!("个音频包", "audio packs")),
                    (entry.portraits.len(), tr!("张立绘", "portraits")),
                    (entry.live2d.len(), tr!("个 Live2D", "Live2D")),
                    (entry.lines.len(), tr!("条台词", "lines")),
                ]
                .into_iter()
                .filter(|(count, _)| *count > 0)
                .map(|(count, unit)| format!("{} {unit}", fmt_count(count)))
                .collect();
                if !parts.is_empty() {
                    unassigned_note = Some(parts.join(" · "));
                }
            }
        }
        if sections.is_empty() {
            sections.push(
                div()
                    .py(px(24.))
                    .text_size(px(13.))
                    .text_color(hsla(TEXT_FAINT))
                    .child(if data.is_some() {
                        tr!("没有匹配的角色。", "No matching characters.")
                    } else {
                        tr!("角色资料载入中…", "Loading character data…")
                    })
                    .into_any_element(),
            );
        }
        let unassigned =
            unassigned_note.map(|note| {
                div()
                    .id("pick-unassigned")
                    .flex()
                    .flex_shrink_0()
                    .items_center()
                    .gap(px(10.))
                    .pt(px(12.))
                    .border_t_1()
                    .border_color(hsla(DIVIDER))
                    .cursor_pointer()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(px(SIZE_CONTROL))
                            .text_color(hsla(TEXT_MUTED))
                            .child(tr!(
                                "没有对应到角色的资源",
                                "Resources not matched to a character"
                            )),
                    )
                    .child(
                        div()
                            .text_size(px(SIZE_CONTROL))
                            .text_color(hsla(ACCENT))
                            .child(tr!(
                                "未归类 · {note} →",
                                "Unassigned · {note} →",
                                note = note
                            )),
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.pick_character(UNASSIGNED_KEY.to_owned(), cx)
                    }))
            });

        (sections, unassigned)
    }

    /// Search box shared by the panel and the page.
    fn picker_search(&self, shortcut: bool, cx: &App) -> gpui::Div {
        div()
            .flex()
            .flex_shrink_0()
            .items_center()
            .gap(px(10.))
            .h(px(44.))
            .px(px(14.))
            .rounded(px(10.))
            .bg(hsla(BG_FIELD_DEEP))
            .border_1()
            .border_color(hsla(FOCUS_RING))
            .child(icon("app/search.svg", 16., TEXT_MUTED))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_size(px(14.))
                    .child(Input::new(&self.picker_input).appearance(false)),
            )
            .children(clear_button(&self.picker_input, cx))
            .when(shortcut, |this| {
                this.child(
                    div()
                        .text_size(px(SIZE_MONO))
                        .text_color(hsla(TEXT_FAINT))
                        .child("Ctrl K"),
                )
            })
    }

    /// Full-page chooser shown on the character page when nothing is
    /// selected: the panel's content, without a scrim or a way to close it.
    pub(super) fn render_picker_page(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let main_width = f32::from(window.viewport_size().width) - self.nav_width(window);
        let pad = page_padding(main_width);
        let width = (main_width - pad * 2.).min(PAGE_MAX);
        let columns = (((width + TILE_GAP) / (TILE_MIN + TILE_GAP)) as usize).max(1);
        // Half a pixel keeps rounding from wrapping the last tile of a row.
        let tile = (width - TILE_GAP * (columns - 1) as f32) / columns as f32 - 0.5;
        let (sections, unassigned) = self.picker_sections(tile, cx);
        div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .pt(px(32.))
            .pb(px(20.))
            .gap(px(18.))
            .child(
                div()
                    .w(px(width))
                    .flex_shrink_0()
                    .flex()
                    .flex_col()
                    .gap(px(14.))
                    .child(
                        div()
                            .text_size(px(22.))
                            .font_weight(FontWeight::BOLD)
                            .child(tr!("选择角色", "Choose a character")),
                    )
                    .child(self.picker_search(false, cx)),
            )
            .child(
                div().w_full().flex_1().min_h_0().child(page_scroll(
                    "picker-page",
                    &self.picker_scroll,
                    div().w_full().flex().justify_center().child(
                        div()
                            .w(px(width))
                            .flex()
                            .flex_col()
                            .gap(px(18.))
                            .font_weight(FontWeight::NORMAL)
                            .children(sections),
                    ),
                )),
            )
            .children(unassigned.map(|row| div().w(px(width)).flex_shrink_0().child(row)))
            .into_any_element()
    }

    pub(super) fn render_picker(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !self.picker_open {
            return None;
        }
        let tile = (PANEL_WIDTH - 40. - TILE_GAP * (COLUMNS - 1) as f32) / COLUMNS as f32;
        let (sections, unassigned) = self.picker_sections(tile, cx);
        let viewport = window.viewport_size();
        let left = self.nav_width(window) + 32.;
        let top = 84f32.min((f32::from(viewport.height) * 0.08).max(40.));
        let max_height = (f32::from(viewport.height) - top - 40.).max(320.);

        let dialog = div()
            .id("picker")
            .occlude()
            .absolute()
            .left(px(left))
            .top(px(top))
            .w(px(PANEL_WIDTH))
            .h(px(max_height.min(640.)))
            .flex()
            .flex_col()
            .gap(px(18.))
            .p(px(20.))
            .rounded(px(16.))
            .bg(hsla(BG_DIALOG))
            .shadow(vec![gpui::BoxShadow {
                color: hsla_alpha(SHADOW, 0.55),
                offset: gpui::point(px(0.), px(24.)),
                blur_radius: px(60.),
                spread_radius: px(0.),
                inset: false,
            }])
            .on_click(|_, _, cx| cx.stop_propagation())
            .child(self.picker_search(true, cx))
            .child(
                div().flex_1().min_h_0().child(page_scroll(
                    "picker-list",
                    &self.picker_scroll,
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(18.))
                        .font_weight(FontWeight::NORMAL)
                        .children(sections),
                )),
            )
            .children(unassigned);

        Some(
            div()
                .id("picker-scrim")
                .absolute()
                .inset_0()
                .occlude()
                .bg(hsla_alpha(SCRIM, 0.62))
                .on_click(cx.listener(|this, _, _, cx| this.close_picker(cx)))
                .child(dialog)
                .into_any_element(),
        )
    }
}
