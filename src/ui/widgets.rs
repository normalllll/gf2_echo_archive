//! Building blocks shared by the v2 pages: borderless rows, group titles,
//! chips, switches, buttons, the floating action bar, the right drawer and
//! search fields.

use super::{ellipsis::ellipsis_text, theme::*};
use gpui::{
    AnyElement, App, ElementId, Entity, FontWeight, InteractiveElement, IntoElement, ParentElement,
    SharedString, StatefulInteractiveElement, Styled, Svg, Window, div, prelude::FluentBuilder, px,
    svg,
};
use gpui_component::input::{Input, InputState};

pub fn fmt_count(value: usize) -> String {
    let digits = value.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

pub fn icon(path: &'static str, size: f32, color: Color) -> Svg {
    svg()
        .path(path)
        .size(px(size))
        .flex_shrink_0()
        .text_color(hsla(color))
}

/// Small grey group heading above a list section.
pub fn group_title(text: impl Into<SharedString>) -> gpui::Div {
    div()
        .text_size(px(SIZE_GROUP))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(hsla(TEXT_FAINT))
        .child(text.into())
}

/// Small grey monospace text for resource names and other technical data.
pub fn mono_note(text: impl Into<SharedString>) -> gpui::Div {
    mono(
        div()
            .min_w_0()
            .overflow_hidden()
            .text_size(px(SIZE_MONO))
            .text_color(hsla(TEXT_FAINT))
            .child(ellipsis_text(text.into())),
    )
}

/// Single-line text that ends in an ellipsis when it does not fit.
pub fn one_line(text: impl Into<SharedString>) -> gpui::Div {
    div()
        .min_w_0()
        .overflow_hidden()
        .child(ellipsis_text(text.into()))
}

/// Single-line text with an ellipsis and the full value in a tooltip.
/// Callers must give it a bounded width.
pub fn truncated(
    id: impl Into<ElementId>,
    text: impl Into<SharedString>,
) -> gpui::Stateful<gpui::Div> {
    let text: SharedString = text.into();
    let tooltip = text.clone();
    div()
        .id(id)
        .w_full()
        .min_w_0()
        .overflow_hidden()
        .child(ellipsis_text(text))
        .tooltip(move |window, cx| {
            gpui_component::tooltip::Tooltip::new(tooltip.clone()).build(window, cx)
        })
}

/// A list row without borders: spacing, hover and selection colour only.
/// The row extends 14 px past the content column on both sides.
pub fn row_frame(
    id: impl Into<ElementId>,
    selected: bool,
    height: f32,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .flex()
        .items_center()
        .gap(px(16.))
        .h(px(height))
        .px(px(14.))
        .mx(px(-14.))
        .rounded(px(10.))
        .cursor_pointer()
        .when(selected, |this| this.bg(hsla(BG_SELECTED)))
        .when(!selected, |this| {
            this.hover(|style| style.bg(hsla(BG_HOVER)))
        })
}

/// Checkbox glyph drawn like the design (filled when checked).
pub fn check_glyph(checked: bool) -> gpui::Div {
    div()
        .size(px(16.))
        .flex_shrink_0()
        .rounded(px(4.))
        .flex()
        .items_center()
        .justify_center()
        .when(checked, |this| {
            this.bg(hsla(ACCENT_FILL))
                .child(icon("app/check-bold.svg", 11., TEXT_ON_ACCENT))
        })
        .when(!checked, |this| {
            this.border_1().border_color(hsla(CHECK_BORDER))
        })
}

/// Round check badge on a selected card.
pub fn check_badge() -> gpui::Div {
    div()
        .size(px(24.))
        .rounded_full()
        .bg(hsla(ACCENT_FILL))
        .flex()
        .items_center()
        .justify_center()
        .child(icon("app/check-bold.svg", 13., TEXT_ON_ACCENT))
}

/// Outline of a selected or focused card. It is drawn over the card and
/// takes no layout space, so the content stays exactly where it was.
pub fn selection_ring(radius: f32, color: Color) -> gpui::Div {
    div()
        .absolute()
        .inset_0()
        .rounded(px(radius))
        .border_2()
        .border_color(hsla(color))
}

/// Pill used for filters (All / CN / JP …).
pub fn chip(id: impl Into<ElementId>, active: bool, enabled: bool) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .flex()
        .flex_shrink_0()
        .items_center()
        .gap(px(6.))
        .h(px(30.))
        .px(px(12.))
        .rounded(px(15.))
        .text_size(px(SIZE_CONTROL))
        .when(active, |this| {
            this.bg(hsla(BG_CHIP_ACTIVE)).text_color(hsla(TEXT_STRONG))
        })
        .when(!active && enabled, |this| {
            this.text_color(hsla(TEXT_MUTED))
                .cursor_pointer()
                .hover(|style| style.bg(hsla(BG_HOVER)))
        })
        .when(!enabled, |this| this.text_color(hsla(TEXT_DISABLED)))
}

/// Segmented control item used inside dialogs.
pub fn segment(id: impl Into<ElementId>, active: bool) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .flex()
        .items_center()
        .h(px(28.))
        .px(px(12.))
        .rounded(px(7.))
        .text_size(px(SIZE_CONTROL))
        .when(active, |this| {
            this.bg(hsla(BG_SEGMENT_ON)).text_color(hsla(TEXT_STRONG))
        })
        .when(!active, |this| {
            this.text_color(hsla(TEXT_MUTED))
                .cursor_pointer()
                .hover(|style| style.text_color(hsla(TEXT)))
        })
}

pub fn segmented() -> gpui::Div {
    div()
        .flex()
        .flex_shrink_0()
        .p(px(3.))
        .rounded(px(9.))
        .bg(hsla(BG_FIELD_DEEP))
}

/// On/off switch (38 × 22).
pub fn switch(id: impl Into<ElementId>, on: bool, enabled: bool) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .w(px(38.))
        .h(px(22.))
        .flex_shrink_0()
        .rounded(px(11.))
        .p(px(3.))
        .flex()
        .items_center()
        .when(on, |this| this.bg(hsla(ACCENT_FILL)).justify_end())
        .when(!on, |this| this.bg(hsla(BG_SWITCH_OFF)))
        .when(enabled, |this| this.cursor_pointer())
        .when(!enabled, |this| this.opacity(0.45))
        .child(div().size(px(16.)).rounded_full().bg(hsla(if on {
            TEXT_ON_ACCENT
        } else {
            SWITCH_KNOB_OFF
        })))
}

/// A labelled option row with a switch on the right (dialogs).
pub fn switch_row(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    note: Option<SharedString>,
    on: bool,
    enabled: bool,
) -> gpui::Stateful<gpui::Div> {
    let id = id.into();
    div()
        .id(id.clone())
        .flex()
        .items_center()
        .gap(px(12.))
        .min_h(px(44.))
        .when(enabled, |this| this.cursor_pointer())
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(
                    div()
                        .text_size(px(14.))
                        .text_color(hsla(if enabled { TEXT } else { TEXT_FAINT }))
                        .child(label.into()),
                )
                .children(note.map(|note| {
                    div()
                        .text_size(px(SIZE_GROUP))
                        .text_color(hsla(TEXT_FAINT))
                        .child(note)
                })),
        )
        .child(switch(
            ElementId::Name(format!("{id}-switch").into()),
            on,
            enabled,
        ))
}

pub fn primary_button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    enabled: bool,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .flex()
        .flex_shrink_0()
        .items_center()
        .justify_center()
        .h(px(36.))
        .px(px(18.))
        .rounded(px(9.))
        .text_size(px(13.))
        .font_weight(FontWeight::SEMIBOLD)
        .when(enabled, |this| {
            this.bg(hsla(ACCENT_FILL))
                .text_color(hsla(TEXT_ON_ACCENT))
                .cursor_pointer()
                .hover(|style| style.bg(hsla(ACCENT_FILL_HOVER)))
        })
        .when(!enabled, |this| {
            this.bg(hsla(BG_BUTTON)).text_color(hsla(TEXT_DISABLED))
        })
        .child(label.into())
}

/// Text-only button (Clear selection, Cancel …).
pub fn ghost_button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .flex()
        .flex_shrink_0()
        .items_center()
        .h(px(36.))
        .px(px(14.))
        .rounded(px(9.))
        .text_size(px(13.))
        .text_color(hsla(TEXT_SECONDARY))
        .cursor_pointer()
        .hover(|style| style.bg(hsla(BG_BUTTON_HOVER)))
        .child(label.into())
}

/// Filled secondary button (Export…, Change, Rebuild …).
pub fn soft_button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    enabled: bool,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .flex()
        .flex_shrink_0()
        .items_center()
        .justify_center()
        .gap(px(6.))
        .h(px(34.))
        .px(px(14.))
        .rounded(px(8.))
        .text_size(px(13.))
        .when(enabled, |this| {
            this.bg(hsla(BG_BAR))
                .text_color(hsla(TEXT))
                .cursor_pointer()
                .hover(|style| style.bg(hsla(BG_BUTTON_HOVER)))
        })
        .when(!enabled, |this| {
            this.bg(hsla(BG_BUTTON)).text_color(hsla(TEXT_DISABLED))
        })
        .child(label.into())
}

/// Accent text link.
pub fn link(id: impl Into<ElementId>, label: impl Into<SharedString>) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .flex_shrink_0()
        .text_size(px(13.))
        .text_color(hsla(ACCENT))
        .cursor_pointer()
        .hover(|style| style.text_color(hsla(ACCENT_HOVER)))
        .child(label.into())
}

/// Borderless filled search field around a gpui-component input.
pub fn search_field(input: &Entity<InputState>, height: f32, cx: &App) -> gpui::Div {
    div()
        .flex()
        .items_center()
        .gap(px(8.))
        .h(px(height))
        .pl(px(12.))
        .pr(px(4.))
        .rounded(px(8.))
        .bg(hsla(BG_FIELD))
        .child(icon("app/search.svg", 14., TEXT_FAINT))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_size(px(13.))
                .child(Input::new(input).appearance(false)),
        )
        .children(clear_button(input, cx))
}

/// Button that empties a search field and keeps it focused; only present
/// while there is text.
pub fn clear_button(input: &Entity<InputState>, cx: &App) -> Option<AnyElement> {
    if input.read(cx).value().is_empty() {
        return None;
    }
    let input = input.clone();
    Some(
        div()
            .id("search-clear")
            .size(px(26.))
            .flex()
            .flex_shrink_0()
            .items_center()
            .justify_center()
            .rounded(px(6.))
            .cursor_pointer()
            .hover(|style| style.bg(hsla(BG_BUTTON_HOVER)))
            .child(icon("app/close.svg", 16., TEXT_MUTED))
            .on_click(move |_, window, cx| {
                input.update(cx, |state, cx| {
                    state.set_value("", window, cx);
                    state.focus(window, cx);
                });
            })
            .into_any_element(),
    )
}

/// Floating action bar shown while something is selected:
/// the selection count, clear and export. Centred near the bottom of its parent.
pub fn action_bar(content: impl IntoElement) -> gpui::Div {
    div()
        .absolute()
        .left_0()
        .right_0()
        .bottom(px(24.))
        .flex()
        .justify_center()
        .child(
            div()
                .id("action-bar")
                .occlude()
                .flex()
                .items_center()
                .gap(px(16.))
                .h(px(56.))
                .pl(px(22.))
                .pr(px(10.))
                .rounded(px(14.))
                .bg(hsla(BG_BAR))
                .shadow(vec![gpui::BoxShadow {
                    color: hsla_alpha(SHADOW, 0.45),
                    offset: gpui::point(px(0.), px(12.)),
                    blur_radius: px(32.),
                    spread_radius: px(0.),
                    inset: false,
                }])
                .child(content),
        )
}

/// Selection count with its unit, for the action bar.
pub fn selected_label(count: usize, unit: &str) -> gpui::Div {
    div()
        .flex()
        .items_baseline()
        .gap(px(4.))
        .text_size(px(SIZE_BODY))
        .child(tr!("已选", "Selected"))
        .child(
            div()
                .font_weight(FontWeight::SEMIBOLD)
                .child(fmt_count(count)),
        )
        .child(unit.to_owned())
}

/// Right-side drawer frame with a small heading and close button. The
/// caller places it (inline column or overlay).
pub fn drawer_frame(
    width: f32,
    heading: impl Into<SharedString>,
    close: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
    body: impl IntoElement,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id("drawer")
        .occlude()
        .w(px(width))
        .flex_shrink_0()
        .h_full()
        .flex()
        .flex_col()
        .bg(hsla(BG_DRAWER))
        .shadow(vec![gpui::BoxShadow {
            color: hsla_alpha(SHADOW, 0.25),
            offset: gpui::point(px(-12.), px(0.)),
            blur_radius: px(28.),
            spread_radius: px(0.),
            inset: false,
        }])
        .child(
            div()
                .flex()
                .flex_shrink_0()
                .items_center()
                .gap(px(8.))
                .pl(px(28.))
                .pr(px(18.))
                .pt(px(20.))
                .pb(px(8.))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(px(SIZE_GROUP))
                        .text_color(hsla(TEXT_FAINT))
                        .child(one_line(heading.into())),
                )
                .child(
                    div()
                        .id("drawer-close")
                        .size(px(28.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(6.))
                        .cursor_pointer()
                        .hover(|style| style.bg(hsla(BG_BUTTON_HOVER)))
                        .child(icon("app/close.svg", 14., TEXT_MUTED))
                        .on_click(close),
                ),
        )
        .child(div().flex_1().min_h_0().child(body))
}

/// Vertical scroll container with a visible scrollbar. The content sits in a
/// block-level scroll area, so it keeps the full width and its natural height
/// and can be scrolled to its last element.
pub fn page_scroll(
    id: impl Into<SharedString>,
    handle: &gpui::ScrollHandle,
    content: impl IntoElement,
) -> gpui::Stateful<gpui::Div> {
    use gpui_component::scroll::ScrollableElement;
    let id: SharedString = id.into();
    div()
        .id(ElementId::Name(id.clone()))
        .size_full()
        .relative()
        .child(
            div()
                .id(ElementId::Name(format!("{id}-area").into()))
                .size_full()
                .overflow_y_scroll()
                .track_scroll(handle)
                .child(content),
        )
        .vertical_scrollbar(handle)
}

/// Centered grey message for empty states.
pub fn empty_state(text: impl Into<SharedString>) -> AnyElement {
    div()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .p(px(32.))
        .text_size(px(13.))
        .text_color(hsla(TEXT_FAINT))
        .child(text.into())
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::fmt_count;

    #[test]
    fn formats_counts_with_separators() {
        assert_eq!(fmt_count(0), "0");
        assert_eq!(fmt_count(873), "873");
        assert_eq!(fmt_count(2931), "2,931");
        assert_eq!(fmt_count(1234567), "1,234,567");
    }
}
