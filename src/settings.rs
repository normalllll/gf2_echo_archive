//! User settings in `<exe folder>\data\settings.json`. Reading
//! never fails: a missing or damaged file, or a damaged field, falls back to
//! defaults so the app always starts.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::path::{Path, PathBuf};

pub const RECENT_CHARACTERS_LIMIT: usize = 6;

/// Which story scripts the story tab shows by default.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StoryScope {
    /// CN-localised scripts only.
    #[default]
    Cn,
    /// CN and original-region scripts.
    All,
}

/// Interface theme. `System` follows the Windows appearance.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThemeChoice {
    #[default]
    System,
    Light,
    Dark,
}

/// Interface language. `System` follows the Windows locale at startup.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LanguageChoice {
    #[default]
    System,
    Chinese,
    English,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settings {
    pub game_directory: Option<PathBuf>,
    pub output_directory: Option<PathBuf>,
    /// When the usage notice was accepted (Unix seconds).
    pub notice_accepted_at: Option<u64>,
    pub sidebar_collapsed: bool,
    /// Character resource names, most recent first.
    pub recent_characters: Vec<String>,
    pub story_scope: StoryScope,
    pub theme: ThemeChoice,
    pub language: LanguageChoice,
}

pub fn settings_path() -> Option<PathBuf> {
    crate::paths::data_dir().map(|dir| dir.join("settings.json"))
}

fn field<T: DeserializeOwned + Default>(object: &serde_json::Value, name: &str) -> T {
    object
        .get(name)
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok())
        .unwrap_or_default()
}

impl Settings {
    /// Loads from the default location; defaults when unavailable.
    pub fn load() -> Self {
        settings_path()
            .map(|path| Self::load_from(&path))
            .unwrap_or_default()
    }

    /// Reads each field on its own, so one bad value does not discard the
    /// rest; unknown fields are ignored.
    pub fn load_from(path: &Path) -> Self {
        let Some(object) = std::fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
            .filter(serde_json::Value::is_object)
        else {
            return Self::default();
        };
        let mut settings = Self {
            game_directory: field(&object, "game_directory"),
            output_directory: field(&object, "output_directory"),
            notice_accepted_at: field(&object, "notice_accepted_at"),
            sidebar_collapsed: field(&object, "sidebar_collapsed"),
            recent_characters: field(&object, "recent_characters"),
            story_scope: field(&object, "story_scope"),
            theme: field(&object, "theme"),
            language: field(&object, "language"),
        };
        settings.normalize_recent();
        settings
    }

    pub fn save(&self) -> Result<()> {
        self.save_to(&settings_path().context(tr!(
            "无法确定程序所在目录",
            "Cannot determine the program's folder"
        ))?)
    }

    pub fn save_to(&self, path: &Path) -> Result<()> {
        crate::portrait::write_atomically(path, &serde_json::to_vec_pretty(self)?)
            .with_context(|| tr!("无法写入 {}", "Cannot write {}", path.display()))
    }

    /// Moves `resource_name` to the front of the recent list.
    pub fn touch_character(&mut self, resource_name: &str) {
        self.recent_characters.insert(0, resource_name.to_owned());
        self.normalize_recent();
    }

    pub fn accept_notice_now(&mut self) {
        self.notice_accepted_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .map(|time| time.as_secs());
    }

    /// Drops empty and repeated names (case-insensitive, first kept) and
    /// keeps at most six.
    fn normalize_recent(&mut self) {
        let mut seen = std::collections::HashSet::new();
        self.recent_characters
            .retain(|name| !name.trim().is_empty() && seen.insert(name.to_ascii_lowercase()));
        self.recent_characters.truncate(RECENT_CHARACTERS_LIMIT);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let directory = std::env::temp_dir().join(format!(
            "echo-archive-settings-{name}-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        directory
    }

    #[test]
    fn round_trips_through_the_file() {
        let directory = temp_dir("round-trip");
        let path = directory.join("nested").join("settings.json");
        let mut settings = Settings {
            game_directory: Some(r"F:\GF2Exilium".into()),
            output_directory: Some("gf2_echo_archive_output".into()),
            sidebar_collapsed: true,
            story_scope: StoryScope::All,
            theme: ThemeChoice::Light,
            language: LanguageChoice::English,
            ..Settings::default()
        };
        settings.accept_notice_now();
        settings.touch_character("Charolic");
        settings.save_to(&path).unwrap();
        assert_eq!(Settings::load_from(&path), settings);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn missing_or_damaged_files_give_defaults() {
        let directory = temp_dir("damaged");
        let path = directory.join("settings.json");
        assert_eq!(Settings::load_from(&path), Settings::default());
        for bytes in [&b"{oops"[..], b"[1,2]", b"null", b""] {
            std::fs::write(&path, bytes).unwrap();
            assert_eq!(Settings::load_from(&path), Settings::default());
        }
        // A bad field only resets that field; unknown fields are ignored.
        std::fs::write(
            &path,
            r#"{"game_directory":"D:\\Game","sidebar_collapsed":"yes","story_scope":"weird","theme":"neon","language":"klingon","future":1,
               "recent_characters":["Tololo","tololo","","A","B","C","D","E","F"]}"#,
        )
        .unwrap();
        let settings = Settings::load_from(&path);
        assert_eq!(settings.game_directory, Some(r"D:\Game".into()));
        assert!(!settings.sidebar_collapsed);
        assert_eq!(settings.story_scope, StoryScope::Cn);
        assert_eq!(settings.theme, ThemeChoice::System);
        assert_eq!(settings.language, LanguageChoice::System);
        assert_eq!(
            settings.recent_characters,
            ["Tololo", "A", "B", "C", "D", "E"]
        );
        // Saving into an unwritable place reports an error instead of panicking.
        assert!(settings.save_to(&path.join("child.json")).is_err());
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn recent_characters_keep_the_latest_six() {
        let mut settings = Settings::default();
        for name in ["A", "B", "C", "D", "E", "F", "G", "c"] {
            settings.touch_character(name);
        }
        assert_eq!(settings.recent_characters, ["c", "G", "F", "E", "D", "B"]);
    }
}
