//! Palette and type scale of the v2 (character-centred) design.
//!
//! Every colour has a dark and a light value; `hsla` picks the one for the
//! current theme. Text has three tiers: titles (`TEXT_STRONG`/`TEXT`), body
//! (`TEXT`) and grey notes (`TEXT_MUTED`/`TEXT_FAINT`). Technical names use
//! small grey monospace (`mono`).

use crate::settings::ThemeChoice;
use gpui::{App, Hsla, Rgba, WindowAppearance, px, rgb};
use gpui_component::{Theme, ThemeMode};
use std::sync::atomic::{AtomicBool, Ordering};

pub const FONT_FAMILY: &str = "Microsoft YaHei UI";
pub const MONO_FONT_FAMILY: &str = "Consolas";

/// One palette entry: its value in the dark and in the light theme.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Color {
    pub dark: u32,
    pub light: u32,
}

impl Color {
    pub const fn new(dark: u32, light: u32) -> Self {
        Self { dark, light }
    }

    /// A colour that does not change with the theme (shadows, artwork).
    pub const fn same(value: u32) -> Self {
        Self::new(value, value)
    }
}

static LIGHT_THEME: AtomicBool = AtomicBool::new(false);

// Surfaces.
pub const BG_APP: Color = Color::new(0x121417, 0xFFFFFF);
pub const BG_NAV: Color = Color::new(0x0F1114, 0xF5F6F8);
pub const BG_HEADER: Color = Color::new(0x15181C, 0xFFFFFF);
pub const BG_FIELD: Color = Color::new(0x1A1D22, 0xF3F4F6);
pub const BG_FIELD_DEEP: Color = Color::new(0x12151A, 0xE7EAEE);
pub const BG_HOVER: Color = Color::new(0x1A1E24, 0xF0F2F5);
pub const BG_SELECTED: Color = Color::new(0x1A2230, 0xE3EDFD);
pub const BG_READING_SELECTED: Color = Color::new(0x18202B, 0xEAF1FD);
pub const BG_NAV_ACTIVE: Color = Color::new(0x1E2430, 0xE3E9F2);
pub const BG_CHIP_ACTIVE: Color = Color::new(0x26303F, 0xDCE6F6);
pub const BG_CHIP: Color = Color::new(0x232830, 0xE8EBEF);
pub const BG_BAR: Color = Color::new(0x232A36, 0xE9ECF0);
pub const BG_BUTTON: Color = Color::new(0x1E2228, 0xF0F2F5);
pub const BG_BUTTON_HOVER: Color = Color::new(0x2A313B, 0xDDE1E7);
pub const BG_DIALOG: Color = Color::new(0x1B1F25, 0xFFFFFF);
pub const BG_DRAWER: Color = Color::new(0x171A1F, 0xFFFFFF);
pub const BG_NOTICE: Color = Color::new(0x20262F, 0xEEF1F5);
pub const BG_SEGMENT_ON: Color = Color::new(0x2E3846, 0xFFFFFF);
pub const BG_SWITCH_OFF: Color = Color::new(0x343A44, 0xC6CBD3);
pub const BG_INITIAL: Color = Color::new(0x262B33, 0xE3E6EA);
pub const BG_CARD: Color = Color::new(0x1A1D22, 0xF3F4F6);
pub const BG_LOG: Color = Color::new(0x0D0F12, 0xF7F8FA);
pub const BG_TRACK: Color = Color::new(0x2A2F36, 0xE3E6EA);
pub const BG_WARN_PANEL: Color = Color::new(0x2A2418, 0xFEF3DC);
pub const SCRIM: Color = Color::same(0x06080A);
pub const DIVIDER: Color = Color::new(0x262B33, 0xE3E6EA);
pub const SEPARATOR_TEXT: Color = Color::new(0x4A515B, 0xA6ADB7);
pub const FOCUS_RING: Color = Color::new(0x3A4D6B, 0x9DB8E8);

// Text.
pub const TEXT_STRONG: Color = Color::new(0xFFFFFF, 0x0F1216);
pub const TEXT: Color = Color::new(0xE8EAED, 0x1F2328);
pub const TEXT_READING: Color = Color::new(0xD5D9DE, 0x2A3036);
pub const TEXT_SECONDARY: Color = Color::new(0xC9CED5, 0x3C434C);
pub const TEXT_MUTED: Color = Color::new(0xA0A7B1, 0x5B636E);
pub const TEXT_FAINT: Color = Color::new(0x7F8792, 0x68707B);
pub const TEXT_DIM: Color = Color::new(0x6B727C, 0x747B86);
pub const TEXT_DISABLED: Color = Color::new(0x4F5660, 0xA2A9B3);
pub const TEXT_ON_ACCENT: Color = Color::same(0xFFFFFF);

// Accents.
pub const ACCENT: Color = Color::new(0x6EA8FE, 0x2563EB);
pub const ACCENT_HOVER: Color = Color::new(0x9CC3FF, 0x1D4ED8);
// Filled backgrounds that carry `TEXT_ON_ACCENT`.
pub const ACCENT_FILL: Color = Color::new(0x2F6FDE, 0x2563EB);
pub const ACCENT_FILL_HOVER: Color = Color::new(0x2861C4, 0x1D4ED8);
pub const ACCENT_FILL_ACTIVE: Color = Color::new(0x2356AE, 0x1E40AF);
pub const SUCCESS: Color = Color::new(0x5CC48D, 0x15803D);
pub const SUCCESS_TEXT: Color = Color::new(0x7FD3A6, 0x15803D);
pub const WARNING: Color = Color::new(0xF0B35A, 0xB45309);
pub const WARNING_BODY: Color = Color::new(0xD9C9A8, 0x77551B);
pub const DANGER: Color = Color::new(0xF2A3A3, 0xB91C1C);
pub const DANGER_DOT: Color = Color::new(0xE5646E, 0xDC2626);

// Details.
pub const SELECTION: Color = Color::new(0x2F4D7A, 0xBFD4F6);
pub const BG_BUTTON_ACTIVE: Color = Color::new(0x2B3139, 0xD3D8E0);
pub const BG_DANGER: Color = Color::new(0x2A1E20, 0xFDECEC);
pub const BG_DANGER_HOVER: Color = Color::new(0x33232A, 0xFBDCDC);
pub const CHECK_BORDER: Color = Color::new(0x5C636D, 0x9AA1AB);
pub const SWITCH_KNOB_OFF: Color = Color::new(0xC9CED5, 0xFFFFFF);
pub const SCROLLBAR_THUMB: Color = Color::new(0x3A414B, 0xC4C9D1);
pub const SCROLLBAR_THUMB_HOVER: Color = Color::new(0x4A525E, 0xA8AEB8);
pub const SHADOW: Color = Color::same(0x000000);

// Backgrounds behind artwork stay the same in both themes.
pub const ART_BACKDROP_DARK: Color = Color::same(0x1A1D22);
pub const ART_BACKDROP_WHITE: Color = Color::same(0xFFFFFF);
pub const ART_CHECKER_DARK: Color = Color::same(0x22262B);
pub const ART_CHECKER_LIGHT: Color = Color::same(0x2C3036);

// Type scale.
pub const SIZE_HERO: f32 = 30.;
pub const SIZE_DIALOG_TITLE: f32 = 18.;
pub const SIZE_TAB: f32 = 15.;
pub const SIZE_ROW_TITLE: f32 = 14.5;
pub const SIZE_BODY: f32 = 13.5;
pub const SIZE_CONTROL: f32 = 12.5;
pub const SIZE_GROUP: f32 = 12.;
pub const SIZE_MONO: f32 = 11.5;

pub fn hsla(color: Color) -> Hsla {
    let value = if LIGHT_THEME.load(Ordering::Relaxed) {
        color.light
    } else {
        color.dark
    };
    let rgba: Rgba = rgb(value);
    rgba.into()
}

pub fn hsla_alpha(color: Color, alpha: f32) -> Hsla {
    let mut color = hsla(color);
    color.a = alpha;
    color
}

/// Theme to use for a saved choice and the system's current appearance.
pub fn resolve_theme(choice: ThemeChoice, system: WindowAppearance) -> ThemeMode {
    match choice {
        ThemeChoice::Light => ThemeMode::Light,
        ThemeChoice::Dark => ThemeMode::Dark,
        ThemeChoice::System => match system {
            WindowAppearance::Dark | WindowAppearance::VibrantDark => ThemeMode::Dark,
            WindowAppearance::Light | WindowAppearance::VibrantLight => ThemeMode::Light,
        },
    }
}

/// Applies `mode` on top of gpui-component's defaults.
pub fn apply(mode: ThemeMode, cx: &mut App) {
    LIGHT_THEME.store(!mode.is_dark(), Ordering::Relaxed);
    Theme::change(mode, None, cx);
    let theme = Theme::global_mut(cx);
    theme.font_family = FONT_FAMILY.into();
    theme.mono_font_family = MONO_FONT_FAMILY.into();
    theme.font_size = px(13.);
    theme.mono_font_size = px(12.);
    theme.radius = px(8.);
    theme.radius_lg = px(10.);
    theme.shadow = false;

    let colors = &mut theme.colors;
    colors.background = hsla(BG_APP);
    colors.foreground = hsla(TEXT);
    // Inputs are drawn borderless on a filled field.
    colors.border = hsla(BG_FIELD);
    colors.input = hsla(BG_FIELD);
    colors.ring = hsla(FOCUS_RING);
    colors.caret = hsla(ACCENT);
    colors.selection = hsla(SELECTION);
    colors.primary = hsla(ACCENT_FILL);
    colors.primary_hover = hsla(ACCENT_FILL_HOVER);
    colors.primary_active = hsla(ACCENT_FILL_ACTIVE);
    colors.primary_foreground = hsla(TEXT_ON_ACCENT);
    colors.secondary = hsla(BG_BUTTON);
    colors.secondary_hover = hsla(BG_BUTTON_HOVER);
    colors.secondary_active = hsla(BG_BUTTON_ACTIVE);
    colors.secondary_foreground = hsla(TEXT);
    colors.muted = hsla(BG_TRACK);
    colors.muted_foreground = hsla(TEXT_MUTED);
    colors.accent = hsla(BG_CHIP_ACTIVE);
    colors.accent_foreground = hsla(TEXT_STRONG);
    colors.popover = hsla(BG_DIALOG);
    colors.popover_foreground = hsla(TEXT);
    colors.list = hsla(BG_DIALOG);
    colors.list_hover = hsla(BG_BUTTON_HOVER);
    colors.list_active = hsla(BG_CHIP_ACTIVE);
    colors.list_active_border = hsla(ACCENT);
    colors.danger = hsla(DANGER_DOT);
    colors.warning = hsla(WARNING);
    colors.success = hsla(SUCCESS);
    colors.title_bar = hsla(BG_NAV);
    colors.title_bar_border = hsla(BG_NAV);
    colors.progress_bar = hsla(ACCENT);
    colors.scrollbar = gpui::transparent_black();
    colors.scrollbar_thumb = hsla(SCROLLBAR_THUMB);
    colors.scrollbar_thumb_hover = hsla(SCROLLBAR_THUMB_HOVER);
    colors.window_border = hsla(BG_NAV);
}

/// Monospace text with an explicit CJK fallback so mixed lines render consistently.
pub fn mono<E: gpui::Styled>(mut element: E) -> E {
    let style = element.text_style();
    style.font_family = Some(MONO_FONT_FAMILY.into());
    style.font_fallbacks = Some(gpui::FontFallbacks::from_fonts(vec![
        FONT_FAMILY.to_owned(),
    ]));
    element
}

#[cfg(test)]
mod tests {
    use super::*;

    fn luminance(color: u32) -> f64 {
        let channel = |shift: u32| {
            let value = f64::from((color >> shift) & 0xff) / 255.0;
            if value <= 0.04045 {
                value / 12.92
            } else {
                ((value + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * channel(16) + 0.7152 * channel(8) + 0.0722 * channel(0)
    }

    fn contrast(a: u32, b: u32) -> f64 {
        let (a, b) = (luminance(a), luminance(b));
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }

    #[test]
    fn grey_notes_stay_readable() {
        // Grey notes and monospace names: at least 4.5:1 on every surface
        // they appear on, in both themes.
        type Pick = fn(Color) -> u32;
        let palettes: [(&str, Pick); 2] =
            [("dark", |color| color.dark), ("light", |color| color.light)];
        for (name, pick) in palettes {
            let surfaces = [
                BG_APP, BG_NAV, BG_HEADER, BG_DRAWER, BG_DIALOG, BG_CARD, BG_FIELD,
            ];
            for background in surfaces.map(pick) {
                for text in [TEXT_MUTED, TEXT_FAINT].map(pick) {
                    let ratio = contrast(text, background);
                    assert!(
                        ratio >= 4.5,
                        "{name} {text:06x} on {background:06x}: {ratio:.2}"
                    );
                }
            }
            for fill in [ACCENT_FILL, ACCENT_FILL_HOVER, ACCENT_FILL_ACTIVE].map(pick) {
                let ratio = contrast(pick(TEXT_ON_ACCENT), fill);
                assert!(ratio >= 4.5, "{name} on accent {fill:06x}: {ratio:.2}");
            }
        }
    }

    #[test]
    fn a_fixed_choice_ignores_the_system_appearance() {
        for system in [WindowAppearance::Light, WindowAppearance::Dark] {
            assert_eq!(resolve_theme(ThemeChoice::Light, system), ThemeMode::Light);
            assert_eq!(resolve_theme(ThemeChoice::Dark, system), ThemeMode::Dark);
        }
    }

    #[test]
    fn following_the_system_maps_every_appearance() {
        for (system, expected) in [
            (WindowAppearance::Light, ThemeMode::Light),
            (WindowAppearance::VibrantLight, ThemeMode::Light),
            (WindowAppearance::Dark, ThemeMode::Dark),
            (WindowAppearance::VibrantDark, ThemeMode::Dark),
        ] {
            assert_eq!(resolve_theme(ThemeChoice::System, system), expected);
        }
    }
}
