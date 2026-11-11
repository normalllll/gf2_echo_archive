//! Settings: game and output directories, data location, index status, the
//! usage notice and version information.

use super::{
    APP_TITLE, EchoArchive, PathTarget,
    state::{LogEntry, disclaimer},
    theme::*,
    widgets::{fmt_count, group_title, mono_note, page_scroll, segment, segmented, soft_button},
};
use crate::{
    discovery::auto_detect_game_directory,
    settings::{LanguageChoice, ThemeChoice},
};
use gpui::{
    AnyElement, Context, Entity, FontWeight, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, Window, div, prelude::FluentBuilder, px,
};
use gpui_component::input::{Input, InputState};
use std::path::PathBuf;

const CONTENT_MAX: f32 = 760.;

/// Days since 1970-01-01 → (year, month, day), proleptic Gregorian calendar.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// `2026-11-08` (UTC) for a Unix timestamp.
pub fn format_date(seconds: u64) -> String {
    let (year, month, day) = civil_from_days((seconds / 86_400) as i64);
    format!("{year:04}-{month:02}-{day:02}")
}

/// A titled card; its rows are separated by hairlines.
fn group(title: &'static str, rows: Vec<gpui::Div>) -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .gap(px(10.))
        .child(group_title(title))
        .child(
            div()
                .flex()
                .flex_col()
                .rounded(px(12.))
                .border_1()
                .border_color(hsla(DIVIDER))
                .children(rows.into_iter().enumerate().map(|(index, row)| {
                    row.when(index > 0, |this| {
                        this.border_t_1().border_color(hsla(DIVIDER))
                    })
                })),
        )
}

/// The one row layout of the page: label and details on the left, controls
/// in a single column on the right.
fn setting_row(
    label: &'static str,
    details: Vec<AnyElement>,
    controls: Vec<AnyElement>,
) -> gpui::Div {
    div()
        .flex()
        .items_center()
        .gap(px(16.))
        .min_h(px(60.))
        .px(px(16.))
        .py(px(12.))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(3.))
                .child(
                    div()
                        .text_size(px(14.))
                        .font_weight(FontWeight::MEDIUM)
                        .child(label),
                )
                .children(details),
        )
        .child(
            div()
                .flex()
                .flex_shrink_0()
                .items_center()
                .gap(px(8.))
                .children(controls),
        )
}

/// Editable path, flush with the label above it.
fn path_field(input: &Entity<InputState>) -> AnyElement {
    mono(
        div().w_full().h(px(20.)).child(
            Input::new(input)
                .appearance(false)
                .px_0()
                .py_0()
                .h(px(20.))
                .text_size(px(SIZE_MONO)),
        ),
    )
    .into_any_element()
}

fn note(text: impl Into<SharedString>, color: Color) -> gpui::Div {
    div()
        .text_size(px(SIZE_CONTROL))
        .line_height(px(20.))
        .text_color(hsla(color))
        .child(text.into())
}

/// One index row: name on the left, state aligned with the controls.
fn index_row(name: &'static str, status: String, ready: bool) -> gpui::Div {
    div()
        .flex()
        .items_center()
        .gap(px(16.))
        .min_h(px(44.))
        .px(px(16.))
        .py(px(8.))
        .child(
            div()
                .flex_shrink_0()
                .text_size(px(14.))
                .font_weight(FontWeight::MEDIUM)
                .child(name),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_right()
                .text_size(px(SIZE_CONTROL))
                .text_color(hsla(if ready { TEXT_MUTED } else { TEXT_FAINT }))
                .child(status),
        )
}

impl EchoArchive {
    fn theme_control(&self, cx: &mut Context<Self>) -> gpui::Div {
        let current = self.settings.theme;
        let options = [
            (
                ThemeChoice::System,
                "theme-system",
                tr!("跟随系统", "System"),
            ),
            (ThemeChoice::Light, "theme-light", tr!("浅色", "Light")),
            (ThemeChoice::Dark, "theme-dark", tr!("深色", "Dark")),
        ];
        segmented().children(options.map(|(choice, id, label)| {
            segment(id, choice == current)
                .child(label)
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.set_theme(choice, window, cx);
                }))
        }))
    }

    fn language_control(&self, cx: &mut Context<Self>) -> gpui::Div {
        let current = self.settings.language;
        let options = [
            (
                LanguageChoice::System,
                "language-system",
                tr!("跟随系统", "System"),
            ),
            // Each language is always named in its own language.
            (LanguageChoice::Chinese, "language-chinese", "简体中文"),
            (LanguageChoice::English, "language-english", "English"),
        ];
        segmented().children(options.map(|(choice, id, label)| {
            segment(id, choice == current)
                .child(label)
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.set_language(choice, window, cx);
                }))
        }))
    }

    fn auto_detect_game(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match auto_detect_game_directory() {
            Some(path) => self.set_path(PathTarget::Game, path, window, cx),
            None => {
                self.state.push_log(super::state::LogEntry::warning(
                    tr!(
                        "[配置提示] 未能自动识别游戏目录，请手动选择。",
                        "[Setup] Could not detect the game directory; please choose it manually."
                    )
                    .to_owned(),
                ));
                self.settings_note = Some(
                    tr!(
                        "未能自动识别游戏目录，请手动选择。",
                        "Could not detect the game directory; please choose it manually."
                    )
                    .to_owned(),
                );
                cx.notify();
            }
        }
    }

    /// Where settings and caches live, with a button that opens the folder.
    fn data_location_row(&self, cx: &mut Context<Self>) -> gpui::Div {
        let path = crate::paths::data_dir().map_or_else(
            || {
                tr!(
                    "无法确定程序所在目录",
                    "Cannot determine the program's folder"
                )
                .to_owned()
            },
            |path| path.display().to_string(),
        );
        let open = soft_button("settings-open-data", tr!("打开文件夹", "Open folder"), true)
            .on_click(
                cx.listener(|this, _, _, cx| match crate::paths::ensure_data_dir() {
                    Ok(directory) => cx.open_with_system(&directory),
                    Err(error) => {
                        this.state.push_log(LogEntry::error(tr!(
                            "[数据目录] {error:#}",
                            "[Data folder] {error:#}",
                            error = error
                        )));
                        this.settings_note = Some(format!("{error:#}"));
                        cx.notify();
                    }
                }),
            );
        setting_row(
            tr!("数据位置", "Data location"),
            vec![
                mono_note(path)
                    .text_color(hsla(TEXT_MUTED))
                    .into_any_element(),
            ],
            vec![open.into_any_element()],
        )
    }

    fn revoke_notice(&mut self, cx: &mut Context<Self>) {
        self.settings.notice_accepted_at = None;
        self.state.disclaimer_accepted = false;
        self.save_settings();
        cx.notify();
    }

    pub(super) fn render_settings(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let main_width = f32::from(window.viewport_size().width) - self.nav_width(window);
        let width = (main_width - 96.).clamp(420., CONTENT_MAX);
        let state = &self.state;
        let running = state.running;
        let scanning = state.is_scanning();
        let scanned = state.is_scanned();
        let game = PathBuf::from(state.game_directory.trim());
        let game_ok = !state.game_directory.trim().is_empty() && game.is_dir();

        let audio_status = if scanning {
            tr!("扫描中…", "Scanning…").to_owned()
        } else if scanned {
            let stats = state.scan_stats.as_ref();
            tr!(
                "{} 个音频包 · 纯索引 ACB {} · 重复 {}",
                "{} audio packs · Metadata-only ACB {} · Duplicates {}",
                fmt_count(state.sources.len()),
                fmt_count(stats.map_or(0, |s| s.metadata_only_acb)),
                fmt_count(stats.map_or(0, |s| s.superseded))
            )
        } else {
            tr!("尚未扫描", "Not scanned yet").to_owned()
        };
        let story_status = match state.story_data() {
            Some(data) => {
                let s = &data.index.summary;
                tr!(
                    "{} 份脚本 · {} 条文本 · {} 条有配音",
                    "{} scripts · {} text lines · {} voiced",
                    fmt_count(s.scripts),
                    fmt_count(s.text_lines),
                    fmt_count(s.voiced_lines)
                )
            }
            None => tr!("尚未建立", "Not built yet").to_owned(),
        };
        let portrait_status = match state.portrait_data() {
            Some(data) => {
                let s = &data.index.summary;
                tr!(
                    "{} 张立绘 · {} 个角色",
                    "{} portraits · {} characters",
                    fmt_count(s.portraits),
                    fmt_count(s.characters)
                )
            }
            None => tr!("尚未建立", "Not built yet").to_owned(),
        };
        let live2d_status = match state.live2d_data() {
            Some(data) => {
                let s = &data.index.summary;
                tr!(
                    "{} 个模型 · 完整 {}",
                    "{} models · Complete {}",
                    fmt_count(s.models),
                    fmt_count(s.complete)
                )
            }
            None => tr!("尚未建立", "Not built yet").to_owned(),
        };
        let (has_story, has_portrait, has_live2d) = (
            state.story_data().is_some(),
            state.portrait_data().is_some(),
            state.live2d_data().is_some(),
        );
        let disclaimer_at = self.settings.notice_accepted_at;
        let accepted = state.disclaimer_accepted;
        let hint = self.settings_note.clone();

        let mut game_details = vec![path_field(&self.game_input)];
        if let Some(hint) = hint {
            game_details.push(note(hint, WARNING).into_any_element());
        }
        if !game_ok {
            game_details.push(
                note(
                    tr!("还没有可用的游戏目录。请选择游戏安装目录，然后点击标题栏的“解析”。", "No usable game directory yet. Choose the game installation folder, then click \"Parse\" in the title bar."),
                    WARNING,
                )
                .into_any_element(),
            );
        }
        let game_row = setting_row(
            tr!("游戏目录", "Game directory"),
            game_details,
            vec![
                soft_button("settings-detect", tr!("自动识别", "Detect"), !running)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.auto_detect_game(window, cx);
                    }))
                    .into_any_element(),
                soft_button("settings-pick-game", tr!("更换", "Change"), !running)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.pick_folder(PathTarget::Game, window, cx);
                    }))
                    .into_any_element(),
            ],
        );
        let output_row = setting_row(
            tr!("输出目录", "Output directory"),
            vec![path_field(&self.output_input)],
            vec![
                soft_button("settings-pick-output", tr!("更换", "Change"), !running)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.pick_folder(PathTarget::Output, window, cx);
                    }))
                    .into_any_element(),
            ],
        );

        let notice_status = note(
            match (accepted, disclaimer_at) {
                (true, Some(at)) => tr!("已同意 · {}", "Accepted · {}", format_date(at)),
                (true, None) => tr!("已同意", "Accepted").to_owned(),
                (false, _) => tr!(
                    "尚未同意，首次导出时会询问。",
                    "Not accepted yet; you will be asked on the first export."
                )
                .to_owned(),
            },
            if accepted { SUCCESS_TEXT } else { TEXT_FAINT },
        );
        let mut notice_controls = Vec::new();
        if accepted {
            notice_controls.push(
                soft_button("settings-revoke", tr!("撤销同意", "Withdraw"), true)
                    .on_click(cx.listener(|this, _, _, cx| this.revoke_notice(cx)))
                    .into_any_element(),
            );
        }
        let notice_row = div()
            .flex()
            .flex_col()
            .child(setting_row(
                tr!("使用须知", "Usage notice"),
                vec![notice_status.into_any_element()],
                notice_controls,
            ))
            .child(
                div()
                    .px(px(16.))
                    .pb(px(14.))
                    .child(note(disclaimer(), TEXT_MUTED)),
            );
        let version_row = setting_row(
            tr!("版本", "Version"),
            Vec::new(),
            vec![
                note(
                    format!("{APP_TITLE} {}", env!("CARGO_PKG_VERSION")),
                    TEXT_MUTED,
                )
                .into_any_element(),
            ],
        );

        let content = div()
            .w(px(width))
            .flex()
            .flex_col()
            .gap(px(28.))
            .child(
                div()
                    .text_size(px(22.))
                    .font_weight(FontWeight::BOLD)
                    .child(tr!("设置", "Settings")),
            )
            .child(group(
                tr!("外观", "Appearance"),
                vec![
                    setting_row(
                        tr!("主题", "Theme"),
                        Vec::new(),
                        vec![self.theme_control(cx).into_any_element()],
                    ),
                    setting_row(
                        tr!("语言", "Language"),
                        Vec::new(),
                        vec![self.language_control(cx).into_any_element()],
                    ),
                ],
            ))
            .child(group(
                tr!("文件位置", "Locations"),
                vec![game_row, output_row, self.data_location_row(cx)],
            ))
            .child(group(
                tr!("索引", "Indexes"),
                vec![
                    index_row(tr!("音频扫描", "Audio scan"), audio_status, scanned),
                    index_row(tr!("剧情", "Story"), story_status, has_story),
                    index_row(tr!("立绘", "Portraits"), portrait_status, has_portrait),
                    index_row("Live2D", live2d_status, has_live2d),
                ],
            ))
            .child(group(tr!("关于", "About"), vec![notice_row, version_row]));
        page_scroll(
            "settings",
            &self.settings_scroll,
            div()
                .w_full()
                .flex()
                .justify_center()
                .pt(px(40.))
                .pb(px(48.))
                .child(content),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_unix_dates() {
        assert_eq!(format_date(0), "1970-01-01");
        // 2000-02-29 and a late date.
        assert_eq!(format_date(951_782_400), "2000-02-29");
        assert_eq!(format_date(1_794_096_000), "2026-11-08");
    }
}
