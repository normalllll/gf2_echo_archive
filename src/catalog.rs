use crate::cri::has_memory_bank;
use serde::Serialize;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};
use walkdir::WalkDir;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub enum Category {
    Character,
    Story,
    Music,
    Sound,
    Other,
}

impl Category {
    pub const ALL: [Self; 5] = [
        Self::Character,
        Self::Story,
        Self::Music,
        Self::Sound,
        Self::Other,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Self::Character => tr!("角色语音", "Character voices"),
            Self::Story => tr!("剧情语音", "Story voices"),
            Self::Music => tr!("背景音乐", "Music"),
            Self::Sound => tr!("音效 / 环境音", "SFX / ambience"),
            Self::Other => tr!("其他音频", "Other audio"),
        }
    }
    pub fn folder(self) -> &'static str {
        match self {
            Self::Character => "Character",
            Self::Story => "Story",
            Self::Music => "BGM",
            Self::Sound => "Sound",
            Self::Other => "Other",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub enum Language {
    Cn,
    Jp,
    Common,
}
impl Language {
    pub fn folder(self) -> &'static str {
        match self {
            Self::Cn => "CN",
            Self::Jp => "JP",
            Self::Common => "Common",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub enum SourceKind {
    Awb,
    Acb,
    Hca,
}
impl SourceKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Awb => "AWB",
            Self::Acb => "ACB",
            Self::Hca => "HCA",
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct SourceInfo {
    pub path: PathBuf,
    pub kind: SourceKind,
    pub category: Category,
    pub language: Language,
    pub key: String,
    pub character: Option<String>,
    pub bytes: u64,
}

impl SourceInfo {
    pub fn from_path(path: &Path, bytes: u64) -> Option<Self> {
        let kind = match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
            "awb" => SourceKind::Awb,
            "acb" => SourceKind::Acb,
            "hca" => SourceKind::Hca,
            _ => return None,
        };
        let stem = path.file_stem()?.to_str()?;
        let (key, suffix_language) = strip_language(stem);
        let language = suffix_language
            .or_else(|| {
                path.parent()?.ancestors().find_map(|p| {
                    match p.file_name()?.to_str()?.to_ascii_uppercase().as_str() {
                        "CN" => Some(Language::Cn),
                        "JP" => Some(Language::Jp),
                        _ => None,
                    }
                })
            })
            .unwrap_or(Language::Common);
        let upper = key.to_ascii_uppercase();
        let mut character = [
            "VO_SERVR_",
            "VO_LOBBY_",
            "VO_BEDROOM_",
            "VO_PERSONALPROFILE_",
            "VO_LOUNGE_",
        ]
        .into_iter()
        .find_map(|prefix| {
            upper.strip_prefix(prefix)?;
            key[prefix.len()..]
                .split('_')
                .next()
                .filter(|name| !name.is_empty())
                .map(str::to_owned)
        });
        let category = if upper.starts_with("VO_CHAPTER")
            || upper.starts_with("VO_EVENT_")
            || upper.starts_with("VO_LEVEL_")
        {
            Category::Story
        } else if upper.starts_with("BGM_") {
            Category::Music
        } else if upper.starts_with("VO_") && !upper.starts_with("VO_TITLE") {
            if character.is_none() {
                character = key[3..]
                    .split('_')
                    .next()
                    .filter(|name| !name.is_empty())
                    .map(str::to_owned);
            }
            Category::Character
        } else if !upper.starts_with("VO_")
            && ["BGE_", "UI_", "SE_", "SFX_", "BATTLE_", "ASMR_"]
                .iter()
                .any(|prefix| upper.starts_with(prefix))
        {
            Category::Sound
        } else {
            Category::Other
        };
        Some(Self {
            path: path.to_owned(),
            kind,
            category,
            language,
            key: key.to_owned(),
            character,
            bytes,
        })
    }

    pub fn matches_character(&self, names: &[String]) -> bool {
        names
            .iter()
            .any(|name| contains_resource_token(&self.key.to_lowercase(), &name.to_lowercase()))
    }
}

fn strip_language(stem: &str) -> (&str, Option<Language>) {
    for (suffix, language) in [("_CN", Language::Cn), ("_JP", Language::Jp)] {
        if let Some(split) = stem.len().checked_sub(suffix.len())
            && stem
                .get(split..)
                .is_some_and(|end| end.eq_ignore_ascii_case(suffix))
        {
            return (&stem[..split], Some(language));
        }
    }
    (stem, None)
}

fn contains_resource_token(value: &str, keyword: &str) -> bool {
    !keyword.is_empty()
        && value.match_indices(keyword).any(|(start, matched)| {
            let end = start + matched.len();
            (start == 0 || !value.as_bytes()[start - 1].is_ascii_alphanumeric())
                && (end == value.len() || !value.as_bytes()[end].is_ascii_alphanumeric())
        })
}

#[derive(Default, Serialize)]
pub struct ScanReport {
    pub sources: Vec<SourceInfo>,
    pub warnings: Vec<String>,
    pub other_files: BTreeMap<String, usize>,
    pub metadata_only_acb: usize,
    pub superseded: usize,
    pub cancelled: bool,
}

pub fn scan_resources(root: &Path, cancel: &AtomicBool) -> ScanReport {
    let mut report = ScanReport::default();
    let mut sources = BTreeMap::new();
    for entry in WalkDir::new(root).follow_links(false) {
        if cancel.load(Ordering::Relaxed) {
            report.cancelled = true;
            break;
        }
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                report.warnings.push(error.to_string());
                continue;
            }
        };
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        let extension = path
            .extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        if !matches!(extension.as_str(), "awb" | "acb" | "hca") {
            if matches!(
                extension.as_str(),
                "bundle" | "bytes" | "usm" | "bnk" | "wem" | "cpk"
            ) {
                *report.other_files.entry(extension).or_default() += 1;
            }
            continue;
        }
        if extension == "acb" {
            match has_memory_bank(path) {
                Ok(true) => {}
                Ok(false) => {
                    report.metadata_only_acb += 1;
                    continue;
                }
                Err(error) => {
                    report.warnings.push(tr!(
                        "{}：{error:#}",
                        "{}: {error:#}",
                        path.display(),
                        error = error
                    ));
                    continue;
                }
            }
        }
        let bytes = match entry.metadata() {
            Ok(metadata) => metadata.len(),
            Err(error) => {
                report.warnings.push(error.to_string());
                continue;
            }
        };
        let Some(source) = SourceInfo::from_path(path, bytes) else {
            continue;
        };
        // The downloaded cache supersedes the bundled title banks. Memory and streaming
        // banks remain distinct because the same cue sheet can own both.
        let identity = (
            source.category,
            source.language,
            source.key.to_lowercase(),
            source.kind,
        );
        match sources.get(&identity) {
            Some(existing) => {
                report.superseded += 1;
                if source_priority(&source) > source_priority(existing) {
                    sources.insert(identity, source);
                }
            }
            None => {
                sources.insert(identity, source);
            }
        }
    }
    report.sources = sources.into_values().collect();
    let mut roster: Vec<_> = report
        .sources
        .iter()
        .filter(|source| source.key.to_ascii_uppercase().starts_with("VO_SERVR_"))
        .filter_map(|source| source.character.clone())
        .collect();
    if let Ok(characters) = crate::characters::load_character_options() {
        for character in characters {
            roster.push(character.resource_name);
            roster.extend(character.aliases);
        }
    }
    roster.sort();
    roster.dedup();
    for source in &mut report.sources {
        if source.category == Category::Character {
            let matches: Vec<_> = roster
                .iter()
                .filter(|name| source.matches_character(&[(*name).clone()]))
                .collect();
            source.character = (matches.len() == 1).then(|| matches[0].clone());
        }
    }
    report
}

fn source_priority(source: &SourceInfo) -> (bool, std::time::SystemTime) {
    let cached = source
        .path
        .components()
        .any(|part| part.as_os_str().eq_ignore_ascii_case("LocalCache"));
    (
        cached,
        source
            .path
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(std::time::UNIX_EPOCH),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn classifies_real_game_resource_names() {
        for (name, category, language) in [
            ("VO_Chapter_00_EP1_CN.awb", Category::Story, Language::Cn),
            (
                "VO_ChapterActivity_120005_JP.awb",
                Category::Story,
                Language::Jp,
            ),
            ("BGM_Agent_Battle.awb", Category::Music, Language::Common),
            ("BGE_AVG_Agent.acb", Category::Sound, Language::Common),
            ("UI_Title.acb", Category::Sound, Language::Common),
            ("VO_ServR_Agent_CN.awb", Category::Character, Language::Cn),
            ("VO_Title_JP.awb", Category::Other, Language::Jp),
        ] {
            let source = SourceInfo::from_path(Path::new(name), 10).unwrap();
            assert_eq!(
                (source.category, source.language),
                (category, language),
                "{name}"
            );
        }
    }
    #[test]
    fn matches_character_tokens_without_overmatching() {
        let source = SourceInfo::from_path(Path::new("VO_Bedroom_Peritya_CN.awb"), 10).unwrap();
        assert!(!source.matches_character(&["Peri".to_owned()]));
        assert!(source.matches_character(&["Peritya".to_owned()]));
        assert!(!source.matches_character(&[String::new()]));
    }
}
