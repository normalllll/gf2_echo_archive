//! Single-line text that ends in "…" when it does not fit.
//!
//! gpui 0.2.2's `text_ellipsis` is not used: its text element moves the
//! text runs into the layout measure closure and `truncate_line` shortens
//! them in place. When the same element is measured again in one layout
//! pass at a width where more of the text fits, the full text is shaped with
//! the stale run length; if that length falls inside a multi-byte character
//! (mixed Chinese / Latin text), DirectWrite slices the string there and the
//! process aborts (`direct_write.rs:595`, "not a char boundary").
//!
//! This element measures with the full text and truncates only while
//! painting, at the final width, on character boundaries, building fresh
//! runs each time. Explicit `\n` lines are truncated one by one.

use gpui::{
    App, AvailableSpace, Bounds, Element, ElementId, GlobalElementId, InspectorElementId,
    IntoElement, LayoutId, Pixels, SharedString, Size, Style, TextAlign, Window, point, px,
};

pub const ELLIPSIS: &str = "…";

/// Largest prefix (a byte index on a character boundary) whose width plus
/// the ellipsis fits in `available`; `None` when the whole text fits.
/// `prefix_width(i)` is the width of `text[..i]`.
pub fn fit_prefix(
    text: &str,
    available: f32,
    full_width: f32,
    ellipsis_width: f32,
    prefix_width: impl Fn(usize) -> f32,
) -> Option<usize> {
    if full_width <= available + 0.5 {
        return None;
    }
    let budget = available - ellipsis_width;
    let mut fit = 0;
    for (index, _) in text.char_indices().skip(1) {
        if prefix_width(index) > budget {
            break;
        }
        fit = index;
    }
    // Do not end the visible part with a space before the ellipsis.
    Some(text[..fit].trim_end().len())
}

pub struct EllipsisText {
    text: SharedString,
}

/// Text that is truncated with an ellipsis instead of wrapping.
pub fn ellipsis_text(text: impl Into<SharedString>) -> EllipsisText {
    EllipsisText { text: text.into() }
}

impl IntoElement for EllipsisText {
    type Element = Self;
    fn into_element(self) -> Self::Element {
        self
    }
}

fn shape(text: &str, window: &Window) -> gpui::ShapedLine {
    let style = window.text_style();
    let font_size = style.font_size.to_pixels(window.rem_size());
    let run = style.to_run(text.len());
    window
        .text_system()
        .shape_line(SharedString::from(text.to_owned()), font_size, &[run], None)
}

/// The text of one line as displayed in `width`.
fn fitted(line: &str, width: Pixels, window: &Window) -> (String, gpui::ShapedLine) {
    let full = shape(line, window);
    let ellipsis = shape(ELLIPSIS, window);
    match fit_prefix(
        line,
        f32::from(width),
        f32::from(full.width),
        f32::from(ellipsis.width),
        |index| f32::from(full.x_for_index(index)),
    ) {
        None => (line.to_owned(), full),
        Some(cut) => {
            let text = format!("{}{ELLIPSIS}", &line[..cut]);
            let shaped = shape(&text, window);
            (text, shaped)
        }
    }
}

impl Element for EllipsisText {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        _: &mut App,
    ) -> (LayoutId, ()) {
        let style = window.text_style();
        let font_size = style.font_size.to_pixels(window.rem_size());
        let line_height = style
            .line_height
            .to_pixels(font_size.into(), window.rem_size());
        let lines: Vec<&str> = self.text.split('\n').collect();
        let full_width = lines
            .iter()
            .map(|line| shape(line, window).width)
            .fold(px(0.), Pixels::max)
            .ceil();
        let height = line_height * lines.len() as f32;
        let layout_id = window.request_measured_layout(
            Style::default(),
            move |known: Size<Option<Pixels>>, available: Size<AvailableSpace>, _, _| {
                // Like gpui's text element: intrinsic sizes are the whole
                // text; only a definite width truncates.
                let width = known.width.unwrap_or(match available.width {
                    AvailableSpace::Definite(width) => full_width.min(width),
                    AvailableSpace::MinContent | AvailableSpace::MaxContent => full_width,
                });
                Size {
                    width,
                    height: known.height.unwrap_or(height),
                }
            },
        );
        (layout_id, ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        _: &mut Window,
        _: &mut App,
    ) {
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        let style = window.text_style();
        let font_size = style.font_size.to_pixels(window.rem_size());
        let line_height = style
            .line_height
            .to_pixels(font_size.into(), window.rem_size());
        let align = style.text_align;
        for (row, line) in self.text.split('\n').enumerate() {
            let (_, shaped) = fitted(line, bounds.size.width, window);
            let offset = match align {
                TextAlign::Left => px(0.),
                TextAlign::Center => ((bounds.size.width - shaped.width) / 2.).max(px(0.)),
                TextAlign::Right => (bounds.size.width - shaped.width).max(px(0.)),
            };
            let origin = bounds.origin + point(offset, line_height * row as f32);
            let _ = shaped.paint(origin, line_height, TextAlign::Left, None, window, cx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Width model for tests: CJK 12 px, everything else 6 px.
    fn width(text: &str) -> f32 {
        text.chars()
            .map(|c| if c.len_utf8() > 1 { 12. } else { 6. })
            .sum()
    }

    fn cut(text: &str, available: f32) -> String {
        match fit_prefix(text, available, width(text), 12., |i| width(&text[..i])) {
            None => text.to_owned(),
            Some(i) => format!("{}{ELLIPSIS}", &text[..i]),
        }
    }

    #[test]
    fn whole_text_when_it_fits() {
        let text = "立绘索引之后自动建立 Live2D 索引";
        assert_eq!(cut(text, width(text)), text);
        assert_eq!(cut(text, 1000.), text);
    }

    #[test]
    fn cuts_mixed_text_only_on_character_boundaries() {
        let texts = [
            "立绘索引之后自动建立 Live2D 索引",
            "导出音频 · 1 个音频包 · CHA_Lenna_SSR_01",
            "输出到 V:\\Exports\\输出目录\\Live2D",
            "ABCDEFGHIJKLMNOPQRSTUVWXYZ",
            "剧情索引之后自动建立",
        ];
        for text in texts {
            let full = width(text);
            let mut previous = 0;
            // Every width from nothing to the full text, in both directions,
            // like repeated layout passes.
            for step in (0..=full as usize).chain((0..=full as usize).rev()) {
                let shown = cut(text, step as f32);
                assert!(shown == text || shown.ends_with(ELLIPSIS), "{shown}");
                if shown != text {
                    let visible = shown.trim_end_matches(ELLIPSIS);
                    assert!(text.starts_with(visible));
                    assert!(width(visible) + 12. <= (step as f32).max(12.) || visible.is_empty());
                    assert!(!visible.ends_with(' '));
                }
                previous = previous.max(shown.len());
            }
            assert!(previous >= text.len());
        }
    }

    #[test]
    fn the_overview_case_keeps_whole_characters() {
        // The width at which gpui produced a 39-byte run inside "索".
        let text = "立绘索引之后自动建立 Live2D 索引";
        let shown = cut(text, width("立绘索引之后自动建立 Live2") + 12.);
        assert_eq!(shown, "立绘索引之后自动建立 Live2…");
        assert!(text.is_char_boundary(shown.len() - ELLIPSIS.len()));
    }
}
