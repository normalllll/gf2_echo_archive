//! The single export dialog every page uses: what is exported, where it is
//! saved, the options of that export type and, before the first export,
//! the usage notice.

use super::{
    EchoArchive,
    state::disclaimer,
    theme::*,
    widgets::{
        ghost_button, group_title, icon, primary_button, segment, segmented, soft_button,
        switch_row,
    },
};
use crate::{catalog::SourceInfo, story::Selection};
use gpui::{
    AnyElement, Context, FontWeight, InteractiveElement, IntoElement, ObjectFit, ParentElement,
    SharedString, StatefulInteractiveElement, Styled, StyledImage, Window, div, img,
    prelude::FluentBuilder, px,
};
use std::{path::PathBuf, sync::Arc};

/// Keeps one story line in a filtered (per-character) story export.
pub type LineFilter = Arc<dyn Fn(&crate::story::link::Line) -> bool + Send + Sync>;

pub enum ExportKind {
    /// Audio packs; Character-category packs are grouped under `character`.
    Audio {
        sources: Vec<SourceInfo>,
        character: Option<String>,
    },
    /// One script, one chapter, or one character's lines (`filter`).
    Story {
        selection: Selection,
        filter: Option<LineFilter>,
    },
    Portraits {
        indices: Vec<usize>,
    },
    Live2d {
        indices: Vec<usize>,
    },
}

pub struct ExportPanel {
    pub kind: ExportKind,
    /// For example "Export 2 audio packs".
    pub title: String,
    /// For example "Charolic · CN + JP".
    pub subtitle: String,
    /// Sub-folder of the output directory the files go to.
    pub destination: Vec<String>,
    /// Up to two preview images shown next to the title.
    pub thumbs: Vec<PathBuf>,
    /// Usage notice ticked in this dialog (first export only).
    pub agree: bool,
    /// Offers "this script / whole chapter" for reader exports.
    pub scope: Option<ScopeChoice>,
}

/// The chapter alternative of a single-script story export.
pub struct ScopeChoice {
    pub label: String,
    pub selection: Selection,
    pub chapter: bool,
}

/// Options remembered for the session (the notice and the output
/// directory are kept in settings).
#[derive(Clone, Copy)]
pub struct ExportChoices {
    pub overwrite: bool,
    pub with_audio: bool,
    pub by_speaker: bool,
    pub jpg: bool,
    pub verify_core: bool,
}

impl Default for ExportChoices {
    fn default() -> Self {
        Self {
            overwrite: false,
            with_audio: true,
            by_speaker: true,
            jpg: false,
            verify_core: false,
        }
    }
}

const PANEL_WIDTH: f32 = 520.;

/// Gap between the two parts of the notice sentence; English needs a space.
fn notice_gap() -> f32 {
    match crate::i18n::language() {
        crate::i18n::Language::Chinese => 0.,
        crate::i18n::Language::English => 4.,
    }
}

impl EchoArchive {
    pub(super) fn open_export(&mut self, panel: ExportPanel, cx: &mut Context<Self>) {
        self.export = Some(panel);
        cx.notify();
    }

    pub(super) fn close_export(&mut self, cx: &mut Context<Self>) {
        self.export = None;
        cx.notify();
    }

    /// Starts the export task and closes the dialog; the task shows up on
    /// the tasks page and as a progress dot in the navigation.
    fn confirm_export(&mut self, cx: &mut Context<Self>) {
        let Some(panel) = self.export.take() else {
            return;
        };
        if !self.state.disclaimer_accepted {
            if !panel.agree {
                self.export = Some(panel);
                return;
            }
            self.state.disclaimer_accepted = true;
            self.settings.accept_notice_now();
            self.save_settings();
        }
        let choices = self.export_choices;
        self.state.overwrite = choices.overwrite;
        let chapter = panel
            .scope
            .filter(|scope| scope.chapter)
            .map(|scope| scope.selection);
        match panel.kind {
            ExportKind::Audio { sources, character } => {
                if let Some(receiver) = self.state.start_audio_export(sources, character) {
                    self.listen(receiver, cx);
                }
            }
            ExportKind::Story { selection, filter } => {
                self.start_story_export(chapter.unwrap_or(selection), filter, cx);
            }
            ExportKind::Portraits { indices } => {
                self.start_portrait_export(indices, choices.jpg, cx);
            }
            ExportKind::Live2d { indices } => {
                self.start_live2d_export(indices, choices.verify_core, cx);
            }
        }
        cx.notify();
    }

    fn pick_output_for_export(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.pick_folder(super::PathTarget::Output, window, cx);
    }

    pub(super) fn render_export_panel(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let panel = self.export.as_ref()?;
        let choices = self.export_choices;
        let accepted = self.state.disclaimer_accepted;
        let running = self.state.running;
        let can_export = !running && (accepted || panel.agree);
        let mut destination = PathBuf::from(self.state.output_directory.trim());
        let whole_chapter = panel.scope.as_ref().filter(|scope| scope.chapter);
        match whole_chapter {
            Some(scope) => {
                destination.push("Story");
                destination.push(scope.selection.label());
            }
            None => {
                for part in &panel.destination {
                    destination.push(part);
                }
            }
        }
        let title = whole_chapter
            .map(|scope| tr!("导出剧情台词 · {}", "Export story lines · {}", scope.label))
            .unwrap_or_else(|| panel.title.clone());

        let thumb_count = panel.thumbs.len().min(2) as f32;
        let thumbs = div()
            .w(px(44. + 32. * (thumb_count - 1.).max(0.)))
            .flex()
            .flex_shrink_0()
            .children(
                panel
                    .thumbs
                    .iter()
                    .take(2)
                    .enumerate()
                    .map(|(position, path)| {
                        div()
                            .size(px(44.))
                            .flex_shrink_0()
                            .when(position > 0, |this| this.ml(px(-12.)))
                            .rounded(px(10.))
                            .overflow_hidden()
                            .bg(hsla(BG_FIELD_DEEP))
                            .border_2()
                            .border_color(hsla(BG_DIALOG))
                            .child(
                                img(path.clone())
                                    .size_full()
                                    .rounded(px(8.))
                                    .object_fit(ObjectFit::Cover),
                            )
                    }),
            );

        let mut options: Vec<AnyElement> = match &panel.kind {
            ExportKind::Audio { .. } => vec![self.overwrite_row(choices.overwrite, cx)],
            ExportKind::Story { .. } => vec![
                switch_row(
                    "export-with-audio",
                    tr!("同时导出语音", "Also export voices"),
                    Some(tr!("需要先扫描音频；按完整 cue 名关联 WAV", "Needs an audio scan first; WAVs are linked by full cue name").into()),
                    choices.with_audio,
                    true,
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.export_choices.with_audio = !this.export_choices.with_audio;
                    cx.notify();
                }))
                .into_any_element(),
                switch_row(
                    "export-by-speaker",
                    tr!("按说话人分文件夹", "Separate folders by speaker"),
                    Some(tr!("把关联到的 WAV 另存到 by-speaker/<说话人>/", "Also saves the linked WAVs to by-speaker/<speaker>/").into()),
                    choices.with_audio && choices.by_speaker,
                    choices.with_audio,
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    if this.export_choices.with_audio {
                        this.export_choices.by_speaker = !this.export_choices.by_speaker;
                        cx.notify();
                    }
                }))
                .into_any_element(),
            ],
            ExportKind::Portraits { .. } => vec![
                switch_row(
                    "export-jpg",
                    tr!("同时生成白底 JPG 预览", "Also create JPG previews on white"),
                    Some(tr!("方便在不支持透明的地方查看", "For places that do not show transparency").into()),
                    choices.jpg,
                    true,
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.export_choices.jpg = !this.export_choices.jpg;
                    cx.notify();
                }))
                .into_any_element(),
                self.overwrite_row(choices.overwrite, cx),
            ],
            ExportKind::Live2d { .. } => vec![
                switch_row(
                    "export-core",
                    tr!("Cubism Core 校验", "Cubism Core check"),
                    Some(tr!("用游戏自带的 Live2DCubismCore.dll 载入模型并检查参数", "Loads the model with the game's Live2DCubismCore.dll and checks its parameters").into()),
                    choices.verify_core,
                    true,
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.export_choices.verify_core = !this.export_choices.verify_core;
                    cx.notify();
                }))
                .into_any_element(),
                self.overwrite_row(choices.overwrite, cx),
            ],
        };

        // A character export has one voice folder; by-speaker folders add nothing.
        if matches!(
            &panel.kind,
            ExportKind::Story {
                filter: Some(_),
                ..
            }
        ) {
            options.truncate(1);
        }

        if let Some(scope) = &panel.scope {
            let chapter = scope.chapter;
            options.insert(
                0,
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .h(px(44.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(px(14.))
                            .child(tr!("导出范围", "Export scope")),
                    )
                    .child(
                        segmented()
                            .child(
                                segment("scope-script", !chapter)
                                    .child(tr!("本段", "This part"))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        if let Some(scope) =
                                            this.export.as_mut().and_then(|p| p.scope.as_mut())
                                        {
                                            scope.chapter = false;
                                        }
                                        cx.notify();
                                    })),
                            )
                            .child(
                                segment("scope-chapter", chapter)
                                    .child(tr!("整章", "Whole chapter"))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        if let Some(scope) =
                                            this.export.as_mut().and_then(|p| p.scope.as_mut())
                                        {
                                            scope.chapter = true;
                                        }
                                        cx.notify();
                                    })),
                            ),
                    )
                    .into_any_element(),
            );
        }

        let notice = (!accepted).then(|| {
            let agree = panel.agree;
            div()
                .id("export-notice")
                .flex()
                .gap(px(12.))
                .px(px(16.))
                .py(px(14.))
                .rounded(px(12.))
                .bg(hsla(BG_NOTICE))
                .cursor_pointer()
                .on_click(cx.listener(|this, _, _, cx| {
                    if let Some(panel) = &mut this.export {
                        panel.agree = !panel.agree;
                        cx.notify();
                    }
                }))
                .child(div().pt(px(2.)).child(super::widgets::check_glyph(agree)))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(px(6.))
                        .text_size(px(13.))
                        .line_height(px(21.))
                        .child(
                            div()
                                .flex()
                                .gap(px(notice_gap()))
                                .child(tr!("我已阅读并同意", "I have read and accept the"))
                                .child(div().text_color(hsla(ACCENT)).child(tr!("使用须知", "usage notice"))),
                        )
                        .child(
                            div()
                                .text_size(px(SIZE_GROUP))
                                .line_height(px(19.))
                                .text_color(hsla(TEXT_SECONDARY))
                                .child(disclaimer()),
                        )
                        .child(
                            div()
                                .text_size(px(SIZE_GROUP))
                                .text_color(hsla(TEXT_MUTED))
                                .child(tr!("只在第一次导出时询问，同意后不再显示。", "Asked only on the first export; not shown again after you accept.")),
                        ),
                )
        });

        let dialog = div()
            .id("export-dialog")
            .occlude()
            .w(px(PANEL_WIDTH))
            .max_h_full()
            .flex()
            .flex_col()
            .gap(px(22.))
            .p(px(28.))
            .rounded(px(18.))
            .bg(hsla(BG_DIALOG))
            .shadow(vec![gpui::BoxShadow {
                color: hsla_alpha(SHADOW, 0.6),
                offset: gpui::point(px(0.), px(28.)),
                blur_radius: px(70.),
                spread_radius: px(0.),
                inset: false,
            }])
            .on_click(|_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(14.))
                    .when(!panel.thumbs.is_empty(), |this| this.child(thumbs))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(px(3.))
                            .child(
                                div()
                                    .text_size(px(SIZE_DIALOG_TITLE))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(title),
                            )
                            .child(
                                div()
                                    .text_size(px(SIZE_CONTROL))
                                    .text_color(hsla(TEXT_MUTED))
                                    .child(super::widgets::one_line(panel.subtitle.clone())),
                            ),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .child(group_title(tr!("保存到", "Save to")))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(10.))
                            .h(px(42.))
                            .pl(px(14.))
                            .pr(px(6.))
                            .rounded(px(10.))
                            .bg(hsla(BG_FIELD_DEEP))
                            .child(icon("app/folder.svg", 16., TEXT_MUTED))
                            .child(
                                div()
                                    .id("export-destination")
                                    .flex_1()
                                    .min_w_0()
                                    .child(mono(
                                        div()
                                            .text_size(px(SIZE_GROUP))
                                            .text_color(hsla(TEXT_SECONDARY))
                                            .child(super::widgets::one_line(
                                                destination.display().to_string(),
                                            )),
                                    ))
                                    .tooltip({
                                        let text: SharedString =
                                            destination.display().to_string().into();
                                        move |window, cx| {
                                            gpui_component::tooltip::Tooltip::new(text.clone())
                                                .build(window, cx)
                                        }
                                    }),
                            )
                            .child(
                                soft_button(
                                    "export-change-output",
                                    tr!("更改", "Change"),
                                    !running,
                                )
                                .h(px(30.))
                                .px(px(12.))
                                .rounded(px(7.))
                                .text_size(px(SIZE_CONTROL))
                                .on_click(cx.listener(
                                    |this, _, window, cx| {
                                        this.pick_output_for_export(window, cx);
                                    },
                                )),
                            ),
                    ),
            )
            .child(div().flex().flex_col().gap(px(4.)).children(options))
            .children(notice)
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_end()
                    .gap(px(10.))
                    .when(running, |this| {
                        this.child(
                            div()
                                .flex_1()
                                .text_size(px(SIZE_GROUP))
                                .text_color(hsla(WARNING))
                                .child(tr!(
                                    "有任务正在运行，完成后再导出。",
                                    "A task is running; export when it finishes."
                                )),
                        )
                    })
                    .child(
                        ghost_button("export-cancel", tr!("取消", "Cancel"))
                            .h(px(40.))
                            .on_click(cx.listener(|this, _, _, cx| this.close_export(cx))),
                    )
                    .child(
                        primary_button("export-confirm", tr!("导出", "Export"), can_export)
                            .h(px(40.))
                            .px(px(22.))
                            .rounded(px(10.))
                            .when(can_export, |this| {
                                this.on_click(cx.listener(|this, _, _, cx| this.confirm_export(cx)))
                            }),
                    ),
            );

        Some(
            div()
                .id("export-scrim")
                .absolute()
                .inset_0()
                .occlude()
                .flex()
                .items_center()
                .justify_center()
                .p(px(24.))
                .bg(hsla_alpha(SCRIM, 0.66))
                .on_click(cx.listener(|this, _, _, cx| this.close_export(cx)))
                .child(dialog)
                .into_any_element(),
        )
    }

    fn overwrite_row(&self, overwrite: bool, cx: &mut Context<Self>) -> AnyElement {
        div()
            .flex()
            .items_center()
            .gap(px(12.))
            .h(px(44.))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_size(px(14.))
                    .child(tr!("已有同名文件时", "When a file already exists")),
            )
            .child(
                segmented()
                    .child(
                        segment("overwrite-skip", !overwrite)
                            .child(tr!("内容相同则跳过", "Skip if identical"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.export_choices.overwrite = false;
                                cx.notify();
                            })),
                    )
                    .child(
                        segment("overwrite-all", overwrite)
                            .child(tr!("全部覆盖", "Overwrite all"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.export_choices.overwrite = true;
                                cx.notify();
                            })),
                    ),
            )
            .into_any_element()
    }
}
