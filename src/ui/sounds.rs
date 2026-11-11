//! Music and sound effects: everything that is not a character's own voice,
//! filtered by category, merged into one row per resource.

use super::{
    EchoArchive,
    character::{LangFilter, page_padding},
    export::{ExportKind, ExportPanel},
    fmt_count,
    state::RowStatus,
    theme::*,
    widgets::{
        action_bar, check_glyph, chip, empty_state, ghost_button, mono_note, one_line,
        primary_button, row_frame, search_field, selected_label,
    },
};
use crate::catalog::{Category, Language, SourceInfo};
use gpui::{
    AnyElement, Context, FontWeight, InteractiveElement, IntoElement, ParentElement,
    ScrollStrategy, SharedString, StatefulInteractiveElement, Styled, UniformListScrollHandle,
    Window, div, prelude::FluentBuilder, px, uniform_list,
};
use gpui_component::scroll::ScrollableElement;
use std::{
    collections::{BTreeMap, BTreeSet},
    rc::Rc,
};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SoundFilter {
    Story,
    Music,
    Sound,
    Other,
    /// Character-category packs no character claims.
    UnassignedVoice,
}

impl SoundFilter {
    const ALL: [Self; 5] = [
        Self::Story,
        Self::Music,
        Self::Sound,
        Self::Other,
        Self::UnassignedVoice,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Story => tr!("剧情语音包", "Story voice packs"),
            Self::Music => tr!("背景音乐", "Music"),
            Self::Sound => tr!("音效与环境音", "SFX and ambience"),
            Self::Other => tr!("其他", "Other"),
            Self::UnassignedVoice => tr!("未归类角色语音", "Unassigned character voices"),
        }
    }

    fn category(self) -> Category {
        match self {
            Self::Story => Category::Story,
            Self::Music => Category::Music,
            Self::Sound => Category::Sound,
            Self::Other => Category::Other,
            Self::UnassignedVoice => Category::Character,
        }
    }
}

pub struct SoundsView {
    pub filter: SoundFilter,
    pub query: String,
    pub lang: LangFilter,
    pub selected: BTreeSet<String>,
    pub scroll: UniformListScrollHandle,
}

impl Default for SoundsView {
    fn default() -> Self {
        Self {
            filter: SoundFilter::Music,
            query: String::new(),
            lang: LangFilter::All,
            selected: BTreeSet::new(),
            scroll: UniformListScrollHandle::new(),
        }
    }
}

/// One resource: all its CN/JP and AWB/ACB files.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SoundRow {
    pub key: String,
    pub sources: Vec<usize>,
    pub languages: String,
    pub packaging: String,
    pub bytes: u64,
}

/// Sources of a category (or the given unassigned list) as merged rows,
/// sorted by name; rows with no file in `lang` are left out.
pub fn sound_rows(
    sources: &[SourceInfo],
    filter: SoundFilter,
    unassigned_voice: &[usize],
    query: &str,
    lang: LangFilter,
) -> Vec<SoundRow> {
    let needle = query.trim().to_lowercase();
    let picked: Vec<usize> = if filter == SoundFilter::UnassignedVoice {
        unassigned_voice.to_vec()
    } else {
        sources
            .iter()
            .enumerate()
            .filter(|(_, source)| source.category == filter.category())
            .map(|(index, _)| index)
            .collect()
    };
    let mut grouped: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for index in picked {
        if let Some(source) = sources.get(index) {
            grouped.entry(source.key.as_str()).or_default().push(index);
        }
    }
    grouped
        .into_iter()
        .filter(|(key, indices)| {
            indices.iter().any(|&i| lang.accepts(sources[i].language))
                && (needle.is_empty() || key.to_lowercase().contains(&needle))
        })
        .map(|(key, indices)| {
            let languages: BTreeSet<Language> =
                indices.iter().map(|&i| sources[i].language).collect();
            let kinds: BTreeSet<&str> = indices.iter().map(|&i| sources[i].kind.label()).collect();
            let has = |language| languages.contains(&language);
            SoundRow {
                key: key.to_owned(),
                languages: match (has(Language::Cn), has(Language::Jp)) {
                    (true, true) => "CN · JP".to_owned(),
                    (true, false) => "CN".to_owned(),
                    (false, true) => "JP".to_owned(),
                    (false, false) => tr!("通用", "Common").to_owned(),
                },
                packaging: kinds.into_iter().collect::<Vec<_>>().join(" + "),
                bytes: indices.iter().map(|&i| sources[i].bytes).sum(),
                sources: indices,
            }
        })
        .collect()
}

fn format_size(bytes: u64) -> String {
    const MIB: f64 = 1024. * 1024.;
    let value = bytes as f64;
    if value >= MIB {
        format!("{:.1} MB", value / MIB)
    } else {
        format!("{:.0} KB", value / 1024.)
    }
}

const ROW_HEIGHT: f32 = 60.;

impl EchoArchive {
    fn unassigned_voice(&self) -> Vec<usize> {
        self.roster
            .as_ref()
            .map(|data| data.roster.unassigned.voice.clone())
            .unwrap_or_default()
    }

    fn filter_count(&self, filter: SoundFilter) -> usize {
        if filter == SoundFilter::UnassignedVoice {
            return self.unassigned_voice().len();
        }
        self.state
            .sources
            .iter()
            .filter(|source| source.category == filter.category())
            .count()
    }

    fn sound_status(&self, row: &SoundRow) -> Option<RowStatus> {
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
        statuses
            .iter()
            .find(|s| {
                matches!(
                    s,
                    RowStatus::Decoding | RowStatus::Queued | RowStatus::Failed
                )
            })
            .or_else(|| statuses.first())
            .copied()
    }

    pub(super) fn render_sounds(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        if !self.state.is_scanned() {
            return empty_state(tr!("音频尚未扫描。", "Audio has not been scanned yet."));
        }
        let main_width = f32::from(window.viewport_size().width) - self.nav_width(window);
        let pad = page_padding(main_width);
        let view = &self.sounds;
        let (filter, lang) = (view.filter, view.lang);
        let rows = Rc::new(sound_rows(
            &self.state.sources,
            filter,
            &self.unassigned_voice(),
            &view.query,
            lang,
        ));
        let chips: Vec<AnyElement> = SoundFilter::ALL
            .into_iter()
            .map(|option| {
                let count = self.filter_count(option);
                chip(
                    SharedString::from(format!("sound-{option:?}")),
                    filter == option,
                    true,
                )
                .child(option.label())
                .child(
                    div()
                        .text_size(px(11.5))
                        .text_color(hsla(TEXT_FAINT))
                        .child(fmt_count(count)),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.sounds.filter = option;
                    this.sounds.selected.clear();
                    this.sounds.scroll.scroll_to_item(0, ScrollStrategy::Top);
                    cx.notify();
                }))
                .into_any_element()
            })
            .collect();
        let languages = [
            (LangFilter::All, "sound-lang-all", tr!("全部", "All")),
            (LangFilter::Cn, "sound-lang-cn", "CN"),
            (LangFilter::Jp, "sound-lang-jp", "JP"),
        ]
        .into_iter()
        .map(|(option, id, label)| {
            chip(id, lang == option, true)
                .child(label)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.sounds.lang = option;
                    cx.notify();
                }))
        });
        let header = div()
            .flex()
            .flex_col()
            .flex_shrink_0()
            .gap(px(14.))
            .px(px(pad))
            .pt(px(28.))
            .pb(px(8.))
            .child(
                div()
                    .text_size(px(22.))
                    .font_weight(FontWeight::BOLD)
                    .child(tr!("音乐与音效", "Music & SFX")),
            )
            .child(div().flex().flex_wrap().gap(px(4.)).children(chips))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .child(search_field(&self.sounds_search, 34., cx).w(px(280.)))
                    .child(div().flex().gap(px(4.)).children(languages)),
            );
        let list: AnyElement = if rows.is_empty() {
            empty_state(tr!(
                "没有符合筛选条件的音频包。",
                "No audio packs match the filter."
            ))
        } else {
            let items = rows.clone();
            div()
                .id("sound-list-area")
                .size_full()
                .relative()
                .child(
                    uniform_list(
                        "sound-rows",
                        rows.len(),
                        cx.processor(move |this, range: std::ops::Range<usize>, _, cx| {
                            range
                                .map(|index| this.render_sound_row(&items[index], pad, cx))
                                .collect()
                        }),
                    )
                    .track_scroll(&self.sounds.scroll)
                    .size_full()
                    .pb(px(90.)),
                )
                .vertical_scrollbar(&self.sounds.scroll)
                .into_any_element()
        };
        let selected = self.sounds.selected.len();
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
                        ghost_button("sound-clear", tr!("取消选择", "Clear selection")).on_click(
                            cx.listener(|this, _, _, cx| {
                                this.sounds.selected.clear();
                                cx.notify();
                            }),
                        ),
                    )
                    .child(
                        primary_button("sound-export", tr!("导出…", "Export…"), true)
                            .on_click(cx.listener(|this, _, _, cx| this.open_sound_export(cx))),
                    ),
            )
        });
        div()
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .child(header)
            .child(div().flex_1().min_h_0().child(list))
            .children(bar)
            .into_any_element()
    }

    fn render_sound_row(&self, row: &SoundRow, pad: f32, cx: &mut Context<Self>) -> AnyElement {
        let selected = self.sounds.selected.contains(&row.key);
        let key = row.key.clone();
        let status = self.sound_status(row);
        let status_color = match status {
            Some(RowStatus::Done | RowStatus::Reused) => SUCCESS_TEXT,
            Some(RowStatus::Failed) => DANGER,
            Some(RowStatus::Warning(_)) => WARNING,
            _ => TEXT_MUTED,
        };
        div()
            .px(px(pad))
            .child(
                row_frame(
                    SharedString::from(format!("sound-{key}")),
                    selected,
                    ROW_HEIGHT,
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
                                .child(one_line(row.key.clone())),
                        )
                        .child(mono_note(if row.sources.len() == 1 {
                            tr!("{} 个文件 · {}", "{} file · {}", 1, format_size(row.bytes))
                        } else {
                            tr!(
                                "{} 个文件 · {}",
                                "{} files · {}",
                                row.sources.len(),
                                format_size(row.bytes)
                            )
                        })),
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
                    let selected = &mut this.sounds.selected;
                    if !selected.remove(&key) {
                        selected.insert(key.clone());
                    }
                    cx.notify();
                })),
            )
            .into_any_element()
    }

    fn open_sound_export(&mut self, cx: &mut Context<Self>) {
        let lang = self.sounds.lang;
        let selected = &self.sounds.selected;
        let filter = self.sounds.filter;
        let unassigned = self.unassigned_voice();
        let sources: Vec<SourceInfo> =
            sound_rows(&self.state.sources, filter, &unassigned, "", lang)
                .into_iter()
                .filter(|row| selected.contains(&row.key))
                .flat_map(|row| row.sources)
                .map(|index| self.state.sources[index].clone())
                .filter(|source| lang.accepts(source.language))
                .collect();
        let count = selected.len();
        self.open_export(
            ExportPanel {
                kind: ExportKind::Audio {
                    sources,
                    character: None,
                },
                title: tr!(
                    "导出 {} 个音频包",
                    "Export {} audio packs",
                    fmt_count(count)
                ),
                subtitle: format!("{} · {}", filter.label(), lang.label()),
                destination: if filter == SoundFilter::UnassignedVoice {
                    Vec::new()
                } else {
                    vec![filter.category().folder().to_owned()]
                },
                thumbs: Vec::new(),
                agree: false,
                scope: None,
            },
            cx,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn sources(names: &[&str]) -> Vec<SourceInfo> {
        names
            .iter()
            .map(|name| SourceInfo::from_path(Path::new(name), 2048).unwrap())
            .collect()
    }

    #[test]
    fn merges_languages_and_containers_into_one_row() {
        let list = sources(&[
            "VO_Chapter_00_EP1_CN.awb",
            "VO_Chapter_00_EP1_JP.awb",
            "VO_Chapter_00_EP1_JP.acb",
            "BGM_Agent_Battle.awb",
        ]);
        let rows = sound_rows(&list, SoundFilter::Story, &[], "", LangFilter::All);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].key, "VO_Chapter_00_EP1");
        assert_eq!(rows[0].languages, "CN · JP");
        assert_eq!(rows[0].packaging, "ACB + AWB");
        assert_eq!(rows[0].sources.len(), 3);
        assert_eq!(rows[0].bytes, 6144);
    }

    #[test]
    fn filters_by_category_language_and_keyword() {
        let list = sources(&[
            "VO_Chapter_00_EP1_CN.awb",
            "VO_Chapter_00_EP1_JP.awb",
            "BGM_Agent_Battle.awb",
            "VO_Odd_Thing_CN.awb",
        ]);
        let story = |lang, query| sound_rows(&list, SoundFilter::Story, &[], query, lang);
        assert_eq!(story(LangFilter::Jp, "").len(), 1);
        assert_eq!(story(LangFilter::All, "chapter_00").len(), 1);
        assert!(story(LangFilter::All, "does-not-exist").is_empty());
        // Common packs stay visible under a language filter.
        let music = sound_rows(
            &list,
            SoundFilter::Music,
            &[],
            "  agent_BATTLE ",
            LangFilter::Jp,
        );
        assert_eq!(music.len(), 1);
        assert_eq!(music[0].languages, "通用");
        // Unassigned voice lists exactly the given packs.
        let voice = sound_rows(
            &list,
            SoundFilter::UnassignedVoice,
            &[3],
            "",
            LangFilter::All,
        );
        assert_eq!(voice.len(), 1);
        assert_eq!(voice[0].key, "VO_Odd_Thing");
    }
}
