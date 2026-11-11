//! Character entries for the character page and the switch panel, built in
//! the background from whatever is loaded (audio scan, story, portrait and
//! Live2D indexes) with the shared roster rules; plus avatar generation.

use super::{
    EchoArchive, fmt_count,
    live2d::Live2dData,
    portrait::PortraitData,
    state::{LogEntry, ParseStage, ParseStatus, TaskKind, TaskState},
    story::StoryData,
};
use crate::{
    catalog::SourceInfo,
    characters::{CharacterOption, load_character_options},
    i18n::Language,
    roster::{
        self, AssetRef, Inputs, Roster, STORY_PERSON_MIN_LINES, StoryLine,
        avatar::{self, Method, Overrides},
        survey,
    },
    story::CachePolicy,
};
use std::{
    collections::{BTreeSet, HashMap},
    path::{Path, PathBuf},
    sync::{Arc, atomic::AtomicBool},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CharKind {
    /// In `characters.json`.
    Playable,
    /// Found in the scan (audio, portraits, Live2D).
    Other,
    /// Story speaker without a character (story lines only).
    StoryPerson,
    /// Resources no character claims.
    Unassigned,
}

pub struct CharEntry {
    /// Stable key: resource name, speaker name, or [`UNASSIGNED_KEY`].
    pub key: String,
    /// Chinese name (resource name when unknown).
    pub name: String,
    pub resource: Option<String>,
    pub aliases: Vec<String>,
    /// Bound story speakers beyond the name and aliases (search and subtitle
    /// only; `aliases` also feeds audio-pack titles).
    pub speaker_aliases: Vec<String>,
    pub kind: CharKind,
    /// Indexes into the scanned audio sources.
    pub voice: Vec<usize>,
    /// Indexes into the portrait index entries.
    pub portraits: Vec<usize>,
    /// Indexes into the Live2D index entries.
    pub live2d: Vec<usize>,
    /// Indexes into `StoryData::search`, by chapter, script and row.
    pub lines: Vec<usize>,
    /// Speaker names this entry's lines carry (exact match).
    pub speaker_names: Vec<String>,
    /// Official English name, shown instead of `name` in the English
    /// interface. Story speakers have none and keep their Chinese name.
    pub english_name: Option<String>,
}

pub const UNASSIGNED_KEY: &str = "*unassigned";

impl CharEntry {
    /// Name as shown; the unassigned entry has no name of its own.
    pub fn display_name(&self) -> String {
        if self.kind == CharKind::Unassigned {
            return tr!("未归类", "Unassigned").to_owned();
        }
        match (crate::i18n::language(), &self.english_name) {
            (Language::English, Some(english)) => english.clone(),
            _ => self.name.clone(),
        }
    }

    /// The shown name's counterparts: the Chinese name in the English
    /// interface, and the resource name when it differs from both.
    fn other_names(&self) -> Vec<String> {
        let shown = self.display_name();
        let mut names = Vec::new();
        for name in [Some(&self.name), self.resource.as_ref()]
            .into_iter()
            .flatten()
        {
            if *name != shown && !name.is_empty() && !names.contains(name) {
                names.push(name.clone());
            }
        }
        names
    }

    /// Picker tooltip: the shown name with its counterparts.
    pub fn tooltip_text(&self) -> String {
        let shown = self.display_name();
        let others = self.other_names();
        if self.kind == CharKind::Unassigned || others.is_empty() {
            shown
        } else {
            tr!("{}（{}）", "{} ({})", shown, others.join(tr!("，", ", ")))
        }
    }

    /// Case-insensitive match on both names, resource name and aliases.
    pub fn matches(&self, needle: &str) -> bool {
        needle.is_empty()
            || self.display_name().to_lowercase().contains(needle)
            || self.name.to_lowercase().contains(needle)
            || self
                .english_name
                .as_ref()
                .is_some_and(|name| name.to_lowercase().contains(needle))
            || self
                .resource
                .as_ref()
                .is_some_and(|r| r.to_lowercase().contains(needle))
            || self
                .aliases
                .iter()
                .chain(&self.speaker_aliases)
                .any(|a| a.to_lowercase().contains(needle))
    }

    /// Resource name, aliases and kind, for the picker.
    pub fn subtitle(&self) -> String {
        let mut parts = Vec::new();
        if self.kind != CharKind::Unassigned {
            parts.extend(self.other_names());
        }
        let shown = self.display_name();
        let aliases: Vec<&str> = self
            .aliases
            .iter()
            .chain(&self.speaker_aliases)
            .filter(|alias| **alias != shown)
            .map(String::as_str)
            .collect();
        if !aliases.is_empty() {
            parts.push(tr!("别名 {}", "Aliases {}", aliases.join(tr!("、", ", "))));
        }
        match self.kind {
            CharKind::StoryPerson => parts.push(
                tr!(
                    "剧情人物 · 只有剧情台词",
                    "Story character · story lines only"
                )
                .to_owned(),
            ),
            CharKind::Unassigned => parts.push(
                tr!(
                    "没有对应到角色的资源",
                    "Resources not matched to a character"
                )
                .to_owned(),
            ),
            _ => {}
        }
        parts.join(" · ")
    }
}

pub struct RosterData {
    pub entries: Vec<CharEntry>,
    pub by_key: HashMap<String, usize>,
    /// Exact speaker name → entry (for clickable names in the reader).
    pub by_speaker: HashMap<String, usize>,
    /// The shared roster (character indexes are entry indexes).
    pub roster: Roster,
}

impl RosterData {
    pub fn find(&self, key: &str) -> Option<usize> {
        self.by_key.get(&key.to_lowercase()).copied()
    }
}

pub struct RosterInputs {
    pub sources: Arc<Vec<SourceInfo>>,
    pub story: Option<Arc<StoryData>>,
    pub portrait: Option<Arc<PortraitData>>,
    pub live2d: Option<Arc<Live2dData>>,
}

pub fn build(inputs: RosterInputs) -> RosterData {
    let catalog: Vec<CharacterOption> = load_character_options().unwrap_or_default();
    let portraits: Vec<AssetRef> = inputs
        .portrait
        .as_ref()
        .map(|data| {
            data.index
                .entries
                .iter()
                .map(|entry| AssetRef {
                    name: entry.portrait.name.clone(),
                    group: Some(entry.character_resource.clone()),
                })
                .collect()
        })
        .unwrap_or_default();
    let models: Vec<AssetRef> = inputs
        .live2d
        .as_ref()
        .map(|data| {
            data.index
                .entries
                .iter()
                .map(|entry| AssetRef {
                    name: entry.model.name.clone(),
                    // Only `CHA_<resource>_…` names carry a character.
                    group: entry
                        .model
                        .name
                        .get(..4)
                        .filter(|p| p.eq_ignore_ascii_case("CHA_"))
                        .map(|_| entry.character_resource.clone()),
                })
                .collect()
        })
        .unwrap_or_default();
    // Same order as `StoryData::search`, whose positions the roster indexes.
    let lines: Vec<StoryLine> = inputs
        .story
        .as_ref()
        .map(|data| {
            data.search
                .iter()
                .map(|entry| StoryLine {
                    script: data.books[entry.book].book.clone(),
                    speaker: entry.name.clone(),
                    character_key: entry.character_key.clone(),
                })
                .collect()
        })
        .unwrap_or_default();
    let character_keys = inputs
        .story
        .as_ref()
        .map(|data| roster::character_keys(&data.index.character_files))
        .unwrap_or_default();
    let roster = roster::build(&Inputs {
        catalog: &catalog,
        audio: &inputs.sources,
        portraits: &portraits,
        live2d: &models,
        story: &lines,
        character_keys: &character_keys,
    });

    let order = |list: &mut Vec<usize>| {
        if let Some(data) = &inputs.story {
            list.sort_by_key(|&i| (data.search[i].book, data.search[i].row));
        }
    };
    let mut entries: Vec<CharEntry> = roster
        .characters
        .iter()
        .map(|c| {
            let mut lines = c.story_lines.clone();
            order(&mut lines);
            CharEntry {
                key: c.resource_name.clone(),
                name: c.name.clone(),
                resource: Some(c.resource_name.clone()),
                aliases: c.aliases.clone(),
                speaker_aliases: c.speaker_aliases(),
                kind: if c.in_catalog() {
                    CharKind::Playable
                } else {
                    CharKind::Other
                },
                voice: c.voice.clone(),
                portraits: c.portraits.clone(),
                live2d: c.live2d.clone(),
                lines,
                speaker_names: c.speaker_names(),
                // Discovered characters have no official English name; their
                // resource name is the closest.
                english_name: catalog
                    .iter()
                    .find(|option| option.resource_name == c.resource_name)
                    .map(|option| option.english_name.clone())
                    .or_else(|| (!c.name.is_ascii()).then(|| c.resource_name.clone())),
            }
        })
        .collect();

    // Story persons: unmatched speakers with enough CN lines.
    let mut by_name: HashMap<&str, (usize, Vec<usize>)> = HashMap::new();
    for &index in &roster.unassigned.story_lines {
        if let Some(name) = lines[index].speaker.as_deref() {
            let slot = by_name.entry(name).or_default();
            if lines[index].is_cn() {
                slot.0 += 1;
            }
            slot.1.push(index);
        }
    }
    let mut persons: Vec<(&str, usize, Vec<usize>)> = Vec::new();
    // Lines of everyone too minor for an entry stay under the unassigned entry.
    let mut minor_lines: Vec<usize> = Vec::new();
    let mut minor_speakers: Vec<String> = Vec::new();
    for (name, (cn, list)) in by_name {
        if cn >= STORY_PERSON_MIN_LINES {
            persons.push((name, cn, list));
        } else {
            minor_lines.extend(list);
            minor_speakers.push(name.to_owned());
        }
    }
    order(&mut minor_lines);
    minor_speakers.sort();
    persons.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    for (name, _, mut list) in persons {
        order(&mut list);
        entries.push(CharEntry {
            key: name.to_owned(),
            name: name.to_owned(),
            resource: None,
            aliases: Vec::new(),
            speaker_aliases: Vec::new(),
            kind: CharKind::StoryPerson,
            voice: Vec::new(),
            portraits: Vec::new(),
            live2d: Vec::new(),
            lines: list,
            speaker_names: vec![name.to_owned()],
            english_name: None,
        });
    }
    let unassigned = &roster.unassigned;
    entries.push(CharEntry {
        key: UNASSIGNED_KEY.to_owned(),
        name: String::new(),
        resource: None,
        aliases: Vec::new(),
        speaker_aliases: Vec::new(),
        kind: CharKind::Unassigned,
        voice: unassigned.voice.clone(),
        portraits: unassigned.portraits.clone(),
        live2d: unassigned.live2d.clone(),
        lines: minor_lines,
        speaker_names: minor_speakers,
        english_name: None,
    });

    let mut by_key = HashMap::new();
    let mut by_speaker = HashMap::new();
    for (index, entry) in entries.iter().enumerate() {
        by_key.entry(entry.key.to_lowercase()).or_insert(index);
        if entry.kind == CharKind::Unassigned {
            continue;
        }
        for name in &entry.speaker_names {
            by_speaker.entry(name.clone()).or_insert(index);
        }
    }
    RosterData {
        entries,
        by_key,
        by_speaker,
        roster,
    }
}

/// Avatars by lower-case resource name: the game's own head sprites, then
/// manual overrides or an automatic head crop of the full-body portrait.
/// Without any of them the UI shows the first character of the name.
pub type Avatars = HashMap<String, PathBuf>;

pub fn generate_avatars(
    game: &Path,
    data: &RosterData,
    portrait: &PortraitData,
    policy: CachePolicy,
    cancel: &AtomicBool,
) -> Result<Avatars, String> {
    let sprite_names = survey::load_with_policy(game, policy, cancel)
        .map_err(|error| format!("{error:#}"))?
        .ok_or_else(|| tr!("已取消", "Cancelled").to_owned())?;
    let mut names = HashMap::new();
    for character in &data.roster.characters {
        for name in character.resource_names() {
            names
                .entry(name.to_ascii_lowercase())
                .or_insert_with(|| character.resource_name.clone());
        }
    }
    let with_portraits: BTreeSet<String> = data
        .roster
        .characters
        .iter()
        .filter(|c| !c.portraits.is_empty())
        .map(|c| c.resource_name.clone())
        .collect();
    let found = survey::survey(&sprite_names.sprites, &names, &with_portraits);
    let overrides = avatar::overrides_path()
        .map(|path| Overrides::load(&path).0)
        .unwrap_or_default();
    let portrait_names: Vec<String> = portrait
        .index
        .entries
        .iter()
        .map(|e| e.portrait.name.clone())
        .collect();
    // The automatic crop is not shown: a portrait with a scene around the
    // figure crops to background, so those characters keep their initial.
    let plans: Vec<_> = avatar::plan(
        &data.roster,
        &portrait_names,
        &sprite_names.sprites,
        &names,
        &found,
        &overrides,
    )
    .into_iter()
    .filter(|plan| plan.method != Method::AlphaCrop)
    .collect();
    let directory = avatar::avatar_dir().ok_or_else(|| {
        tr!(
            "无法确定程序所在目录",
            "Cannot determine the program's folder"
        )
        .to_owned()
    })?;
    let portrait_list: Vec<_> = portrait
        .index
        .entries
        .iter()
        .map(|e| e.portrait.clone())
        .collect();
    let avatars = avatar::generate(
        &data.roster,
        &plans,
        &avatar::Sources {
            bundle_root: &sprite_names.bundle_root,
            bundles: &sprite_names.bundles,
            fingerprint: &sprite_names.fingerprint,
            portraits: &portrait_list,
            sprites: &sprite_names.sprites,
        },
        &directory,
        cancel,
    )
    .map_err(|error| format!("{error:#}"))?;
    Ok(avatars
        .into_iter()
        .filter_map(|(index, avatar)| {
            Some((
                data.roster.characters[index].resource_name.to_lowercase(),
                avatar.path?,
            ))
        })
        .collect())
}

impl EchoArchive {
    /// Last parse stage: builds the roster and renders the avatars, once the
    /// four indexes are loaded. A failed avatar run is only a warning.
    pub(super) fn start_roster_stage(
        &mut self,
        policy: CachePolicy,
        cx: &mut gpui::Context<Self>,
    ) -> bool {
        let state = &self.state;
        let (Some(story), Some(portrait), Some(live2d)) = (
            state.story_data().cloned(),
            state.portrait_data().cloned(),
            state.live2d_data().cloned(),
        ) else {
            self.state.append_error(tr!(
                "索引尚未全部载入，无法生成角色资料。",
                "The indexes are not all loaded; cannot build the character data."
            ));
            return false;
        };
        let inputs = RosterInputs {
            sources: Arc::new(state.sources.clone()),
            story: Some(story),
            portrait: Some(portrait.clone()),
            live2d: Some(live2d),
        };
        let game = PathBuf::from(state.game_directory.trim());
        // This stage has no worker channel; the previous stage's listener
        // must not report its disconnect against this task.
        self.listener = None;
        self.state.cancel = Arc::new(AtomicBool::new(false));
        let cancel = self.state.cancel.clone();
        self.state.begin_task(
            TaskKind::Roster,
            tr!("生成角色与头像", "Generate characters and avatars").to_owned(),
            tr!("整理角色与头像", "Organize characters and avatars").to_owned(),
            String::new(),
        );
        let task = cx.background_executor().spawn(async move {
            let data = Arc::new(build(inputs));
            let avatars = generate_avatars(&game, &data, &portrait, policy, &cancel);
            (data, avatars)
        });
        cx.spawn(async move |this, cx| {
            let (data, avatars) = task.await;
            let _ = this.update(cx, |this, cx| {
                this.finish_roster_stage(data, avatars, cx);
            });
        })
        .detach();
        true
    }

    fn finish_roster_stage(
        &mut self,
        data: Arc<RosterData>,
        avatars: Result<Avatars, String>,
        cx: &mut gpui::Context<Self>,
    ) {
        self.state.running = false;
        let flow = match self.state.parse {
            ParseStatus::Running {
                stage: ParseStage::Roster,
                flow,
            } => Some(flow),
            _ => None,
        };
        let stopped = self.state.stop_requested();
        let Some(flow) = flow.filter(|_| !stopped) else {
            self.state
                .finish_task(TaskState::Stopped, tr!("已停止", "Stopped").to_owned());
            if flow.is_some() {
                self.state.parse = ParseStatus::NotParsed;
                self.reset_parsed();
            }
            cx.notify();
            return;
        };
        match avatars {
            Ok(avatars) => self.avatars = avatars,
            Err(error) => self.state.push_log(LogEntry::warning(tr!(
                "[头像] 无法生成角色头像：{error}",
                "[Avatars] Cannot generate character avatars: {error}",
                error = error
            ))),
        }
        let characters = data.roster.characters.len();
        self.state.finish_task(
            TaskState::Finished,
            tr!("{} 个角色", "{} characters", fmt_count(characters)),
        );
        self.roster = Some(data);
        // A restore changed nothing the record describes.
        if !flow.restores() {
            self.save_parse_record(characters);
        }
        self.state.complete_parse(characters);
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::with_language;

    fn entry(name: &str, resource: Option<&str>, english: Option<&str>) -> CharEntry {
        CharEntry {
            key: name.to_owned(),
            name: name.to_owned(),
            resource: resource.map(str::to_owned),
            aliases: vec!["Krolik".to_owned()],
            speaker_aliases: Vec::new(),
            kind: CharKind::Playable,
            voice: Vec::new(),
            portraits: Vec::new(),
            live2d: Vec::new(),
            lines: Vec::new(),
            speaker_names: Vec::new(),
            english_name: english.map(str::to_owned),
        }
    }

    #[test]
    fn the_english_interface_shows_the_english_name() {
        let krolik = entry("克罗丽科", Some("Charolic"), Some("Krolik"));
        with_language(Language::Chinese, || {
            assert_eq!(krolik.display_name(), "克罗丽科");
            assert_eq!(krolik.tooltip_text(), "克罗丽科（Charolic）");
        });
        with_language(Language::English, || {
            assert_eq!(krolik.display_name(), "Krolik");
            assert_eq!(krolik.tooltip_text(), "Krolik (克罗丽科, Charolic)");
            // The alias equal to the shown name is not repeated.
            assert_eq!(krolik.subtitle(), "克罗丽科 · Charolic");
            assert!(krolik.matches("克罗"));
        });
    }

    #[test]
    fn a_story_speaker_keeps_the_chinese_name() {
        let speaker = entry("指挥官", None, None);
        with_language(Language::English, || {
            assert_eq!(speaker.display_name(), "指挥官");
        });
    }
}
