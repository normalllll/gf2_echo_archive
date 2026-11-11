//! Interface language. Text pairs stay together at the place they are used:
//! `tr!("解析", "Parse")` gives the `&'static str` for the current language,
//! `tr!("{} 个角色", "{} characters", count)` formats it into a `String`.

use crate::settings::LanguageChoice;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Language {
    Chinese,
    English,
}

impl Language {
    /// The matching entry of gpui-component's `locales/ui.yml`.
    pub fn component_locale(self) -> &'static str {
        match self {
            Self::Chinese => "zh-CN",
            Self::English => "en",
        }
    }
}

static ENGLISH: AtomicBool = AtomicBool::new(false);

#[cfg(test)]
thread_local! {
    static TEST_LANGUAGE: std::cell::Cell<Option<Language>> = const { std::cell::Cell::new(None) };
}

pub fn language() -> Language {
    #[cfg(test)]
    if let Some(language) = TEST_LANGUAGE.with(std::cell::Cell::get) {
        return language;
    }
    if ENGLISH.load(Ordering::Relaxed) {
        Language::English
    } else {
        Language::Chinese
    }
}

pub fn set_language(language: Language) {
    ENGLISH.store(language == Language::English, Ordering::Relaxed);
}

/// Runs `body` with `language` on this test's thread only, so tests running
/// at the same time do not affect each other.
#[cfg(test)]
pub fn with_language<T>(language: Language, body: impl FnOnce() -> T) -> T {
    TEST_LANGUAGE.with(|current| current.set(Some(language)));
    let result = body();
    TEST_LANGUAGE.with(|current| current.set(None));
    result
}

/// Language for a saved choice and the system locale.
pub fn resolve_language(choice: LanguageChoice, locale: Option<&str>) -> Language {
    match choice {
        LanguageChoice::Chinese => Language::Chinese,
        LanguageChoice::English => Language::English,
        LanguageChoice::System => {
            let chinese = locale.is_some_and(|locale| {
                locale
                    .get(..2)
                    .is_some_and(|prefix| prefix.eq_ignore_ascii_case("zh"))
            });
            if chinese {
                Language::Chinese
            } else {
                Language::English
            }
        }
    }
}

/// The language for `choice` on this system.
pub fn resolve_for_system(choice: LanguageChoice) -> Language {
    resolve_language(choice, sys_locale::get_locale().as_deref())
}

/// Text without format placeholders; the two-argument `tr!` needs this.
pub const fn is_plain(text: &str) -> bool {
    let bytes = text.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'{' || bytes[index] == b'}' {
            return false;
        }
        index += 1;
    }
    true
}

macro_rules! tr {
    ($zh:literal, $en:literal $(,)?) => {{
        const _: () = assert!(
            $crate::i18n::is_plain($zh) && $crate::i18n::is_plain($en),
            "pass format arguments to tr! when the text has placeholders"
        );
        match $crate::i18n::language() {
            $crate::i18n::Language::Chinese => $zh,
            $crate::i18n::Language::English => $en,
        }
    }};
    ($zh:literal, $en:literal, $($arguments:tt)+) => {
        match $crate::i18n::language() {
            $crate::i18n::Language::Chinese => format!($zh, $($arguments)+),
            $crate::i18n::Language::English => format!($en, $($arguments)+),
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_follows_the_language() {
        with_language(Language::Chinese, || {
            assert_eq!(tr!("解析", "Parse"), "解析");
        });
        with_language(Language::English, || {
            assert_eq!(tr!("解析", "Parse"), "Parse");
        });
    }

    #[test]
    fn placeholders_are_formatted_in_the_selected_language() {
        let count = 12;
        with_language(Language::Chinese, || {
            assert_eq!(
                tr!("已解析 · {} 个角色", "Parsed · {} characters", count),
                "已解析 · 12 个角色"
            );
        });
        with_language(Language::English, || {
            assert_eq!(
                tr!("已解析 · {} 个角色", "Parsed · {} characters", count),
                "Parsed · 12 characters"
            );
        });
    }

    #[test]
    fn named_arguments_can_change_order() {
        with_language(Language::English, || {
            assert_eq!(tr!("{a}{b}", "{b}{a}", a = "1", b = "2"), "21");
        });
    }

    #[test]
    fn a_fixed_choice_ignores_the_locale() {
        for locale in [Some("zh-CN"), Some("en-US"), None] {
            assert_eq!(
                resolve_language(LanguageChoice::Chinese, locale),
                Language::Chinese
            );
            assert_eq!(
                resolve_language(LanguageChoice::English, locale),
                Language::English
            );
        }
    }

    #[test]
    fn following_the_system_uses_chinese_only_for_zh_locales() {
        for (locale, expected) in [
            (Some("zh-CN"), Language::Chinese),
            (Some("zh-Hant-TW"), Language::Chinese),
            (Some("ZH"), Language::Chinese),
            (Some("en-US"), Language::English),
            (Some("ja-JP"), Language::English),
            (Some("z"), Language::English),
            (None, Language::English),
        ] {
            assert_eq!(resolve_language(LanguageChoice::System, locale), expected);
        }
    }
}
