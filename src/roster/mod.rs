//! Unified character roster across audio, story lines, portraits and
//! Live2D models: which resources belong to which character, what is left
//! unassigned, and how well story speakers are covered.

pub mod avatar;
pub mod load;
pub mod speakers;
pub mod survey;

use crate::{
    catalog::{Category, SourceInfo},
    characters::CharacterOption,
    story::{StoryIndex, index::CharacterFile},
};
use serde::Serialize;
use speakers::{ReportBuilder, ResourceBinding, SpeakerLookup, SpeakerMatch, SpeakerReport};
use std::collections::{BTreeMap, HashMap};

/// Discovered characters need this many CN lines when they have neither
/// voice packs nor portraits; unmatched speakers need as many to be listed
/// as story persons.
pub const STORY_PERSON_MIN_LINES: usize = 200;

/// Where a character entry came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    Catalog,
    Audio,
    Portrait,
    Live2d,
}

#[derive(Clone, Debug, Serialize)]
pub struct Character {
    /// Case-insensitive key; catalogue spelling, else first spelling seen.
    pub resource_name: String,
    /// Official Chinese name, or the resource name when not catalogued.
    pub name: String,
    pub aliases: Vec<String>,
    /// Story speakers bound to this character, most lines first. They match
    /// story lines only, whatever their script, and never audio or sprites.
    #[serde(skip)]
    pub bound_speakers: Vec<String>,
    pub origins: Vec<Origin>,
    /// Indexes into [`Inputs::audio`].
    #[serde(skip)]
    pub voice: Vec<usize>,
    /// Indexes into [`Inputs::portraits`].
    #[serde(skip)]
    pub portraits: Vec<usize>,
    /// Indexes into [`Inputs::live2d`].
    #[serde(skip)]
    pub live2d: Vec<usize>,
    /// Indexes into [`Inputs::story`].
    #[serde(skip)]
    pub story_lines: Vec<usize>,
}

impl Character {
    pub fn in_catalog(&self) -> bool {
        self.origins.contains(&Origin::Catalog)
    }

    /// Resource name and aliases (the audio match names).
    pub fn resource_names(&self) -> Vec<String> {
        std::iter::once(self.resource_name.clone())
            .chain(self.aliases.iter().filter(|a| a.is_ascii()).cloned())
            .collect()
    }

    /// Bound speakers not already shown as the name or an alias.
    pub fn speaker_aliases(&self) -> Vec<String> {
        self.bound_speakers
            .iter()
            .filter(|s| **s != self.name && !self.aliases.contains(s))
            .cloned()
            .collect()
    }

    /// Every speaker name whose lines belong to this character.
    pub fn speaker_names(&self) -> Vec<String> {
        let mut names: Vec<String> = std::iter::once(&self.name)
            .chain(&self.aliases)
            .filter(|name| speakers::is_chinese_name(name))
            .cloned()
            .collect();
        for speaker in &self.bound_speakers {
            if !names.contains(speaker) {
                names.push(speaker.clone());
            }
        }
        names
    }
}

/// A portrait or Live2D model with the character group its module derived
/// (`None` when the module found no character part in the name).
#[derive(Clone, Debug, Default)]
pub struct AssetRef {
    pub name: String,
    pub group: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct StoryLine {
    pub script: String,
    pub speaker: Option<String>,
    /// `Arg2`: the character setting key whose image this line shows.
    pub character_key: Option<String>,
}

impl StoryLine {
    /// CN-localised script (`CN_M01001`) rather than the original-region one.
    pub fn is_cn(&self) -> bool {
        self.script
            .get(..3)
            .is_some_and(|p| p.eq_ignore_ascii_case("CN_"))
    }
}

pub struct Inputs<'a> {
    pub catalog: &'a [CharacterOption],
    pub audio: &'a [SourceInfo],
    pub portraits: &'a [AssetRef],
    pub live2d: &'a [AssetRef],
    pub story: &'a [StoryLine],
    /// Character setting key → resource name ([`character_keys`]).
    pub character_keys: &'a HashMap<String, String>,
}

/// Every story line in link order (the order `story::link` produces).
pub fn story_lines(index: &StoryIndex) -> Vec<StoryLine> {
    index
        .linked
        .lines
        .iter()
        .map(|line| StoryLine {
            character_key: line.character_key(),
            script: line.identity.script.clone(),
            speaker: line.speaker.clone(),
        })
        .collect()
}

/// Resource name in a setting table `FileName`: the text between `CHA_` and
/// the next `_` or `.` (`AVG/37/Character/CHA_Lenten_Suit_01.prefab` →
/// `Lenten`). Files without `CHA_` (`SoundOnly_02.prefab`) have none.
fn resource_of_file(file: &str) -> Option<&str> {
    let lower = file.to_ascii_lowercase();
    let bytes = file.as_bytes();
    let (start, _) = lower
        .match_indices("cha_")
        .find(|(at, _)| *at == 0 || !bytes[at - 1].is_ascii_alphanumeric())?;
    let rest = &file[start + 4..];
    let end = rest.find(['_', '.']).unwrap_or(rest.len());
    (end > 0).then(|| &rest[..end])
}

/// Maps setting keys to resource names, ignoring files without `CHA_`.
pub fn character_keys(files: &[CharacterFile]) -> HashMap<String, String> {
    let mut keys = HashMap::new();
    for row in files {
        if let Some(resource) = resource_of_file(&row.file) {
            keys.entry(row.name.clone())
                .or_insert_with(|| resource.to_owned());
        }
    }
    keys
}

#[derive(Clone, Debug, Default)]
pub struct Unassigned {
    /// Character-voice packs that match no character (or several).
    pub voice: Vec<usize>,
    pub portraits: Vec<usize>,
    pub live2d: Vec<usize>,
    /// Lines whose speaker is a name no character has (non-character
    /// speakers and lines without a speaker are not listed).
    pub story_lines: Vec<usize>,
    /// Audio that never belongs to a character (story chapter packs, BGM,
    /// sound effects …), by category.
    pub other_audio: BTreeMap<Category, usize>,
}

pub struct Roster {
    pub characters: Vec<Character>,
    pub unassigned: Unassigned,
    pub speakers: SpeakerReport,
}

/// A discovered group name becomes a character only when it is a plain
/// identifier (`Cecilia`); compound groups (`NeuralCloud_Clukay`,
/// `Player_FeMale`, `V3_Cat`) stay unassigned instead of being guessed.
fn is_plain_name(name: &str) -> bool {
    !name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// Any audio except story chapter packs can belong to a character
/// (`VO_Charolic`, `C_SR01_Charolic` …).
fn voice_pack(source: &SourceInfo) -> bool {
    source.category != Category::Story
}

/// `VO_ServR_<name>_…` → `<name>` (the audio scan's roster source).
fn servr_name(source: &SourceInfo) -> Option<&str> {
    let key = &source.key;
    key.get(..9)
        .filter(|p| p.eq_ignore_ascii_case("VO_ServR_"))
        .and_then(|_| key[9..].split('_').next())
        .filter(|name| !name.is_empty())
}

struct Builder {
    characters: Vec<Character>,
    by_name: HashMap<String, usize>,
}

impl Builder {
    fn find(&self, name: &str) -> Option<usize> {
        self.by_name.get(&name.to_ascii_lowercase()).copied()
    }

    /// Finds or adds the character for a discovered resource name.
    fn discover(&mut self, name: &str, origin: Origin) -> usize {
        let index = match self.find(name) {
            Some(index) => index,
            None => {
                self.characters.push(Character {
                    resource_name: name.to_owned(),
                    name: name.to_owned(),
                    aliases: Vec::new(),
                    bound_speakers: Vec::new(),
                    origins: Vec::new(),
                    voice: Vec::new(),
                    portraits: Vec::new(),
                    live2d: Vec::new(),
                    story_lines: Vec::new(),
                });
                let index = self.characters.len() - 1;
                self.by_name.insert(name.to_ascii_lowercase(), index);
                index
            }
        };
        let origins = &mut self.characters[index].origins;
        if !origins.contains(&origin) {
            origins.push(origin);
            origins.sort();
        }
        index
    }

    /// Applies the speaker bindings to the catalogue: a resource that is a
    /// catalogue character's own takes its bound speakers as aliases, one
    /// bound to a catalogue character's name folds into that character.
    /// Returns the other resources' speakers, by lower-case resource name.
    fn apply_bindings(&mut self, bindings: &[ResourceBinding]) -> HashMap<String, Vec<String>> {
        let mut catalog_names: HashMap<String, usize> = HashMap::new();
        for (index, character) in self.characters.iter().enumerate() {
            catalog_names.entry(character.name.clone()).or_insert(index);
            for alias in character
                .aliases
                .iter()
                .filter(|a| speakers::is_chinese_name(a))
            {
                catalog_names.entry(alias.clone()).or_insert(index);
            }
        }
        let mut discovered = HashMap::new();
        for binding in bindings {
            let own = self.find(&binding.resource);
            let target = own.or_else(|| {
                binding
                    .speakers
                    .iter()
                    .find_map(|speaker| catalog_names.get(&speaker.name).copied())
            });
            let Some(target) = target else {
                discovered.insert(
                    binding.resource.to_ascii_lowercase(),
                    binding.speakers.iter().map(|s| s.name.clone()).collect(),
                );
                continue;
            };
            if own.is_none() {
                self.characters[target]
                    .aliases
                    .push(binding.resource.clone());
                self.by_name
                    .insert(binding.resource.to_ascii_lowercase(), target);
            }
            for speaker in &binding.speakers {
                let elsewhere = catalog_names
                    .get(&speaker.name)
                    .is_some_and(|&named| named != target);
                if !elsewhere {
                    self.characters[target]
                        .bound_speakers
                        .push(speaker.name.clone());
                }
            }
        }
        discovered
    }
}

fn speaker_lookup(characters: &[Character]) -> SpeakerLookup {
    SpeakerLookup::new(characters.iter().enumerate().map(|(index, c)| {
        (
            index,
            c.name.as_str(),
            c.aliases.as_slice(),
            c.bound_speakers.as_slice(),
        )
    }))
}

pub fn build(inputs: &Inputs) -> Roster {
    let mut builder = Builder {
        characters: Vec::new(),
        by_name: HashMap::new(),
    };
    for option in inputs.catalog {
        let index = builder.characters.len();
        builder.characters.push(Character {
            resource_name: option.resource_name.clone(),
            name: option.official_name.clone(),
            aliases: option.aliases.clone(),
            bound_speakers: Vec::new(),
            origins: vec![Origin::Catalog],
            voice: Vec::new(),
            portraits: Vec::new(),
            live2d: Vec::new(),
            story_lines: Vec::new(),
        });
        for name in std::iter::once(&option.resource_name).chain(&option.aliases) {
            builder
                .by_name
                .entry(name.to_ascii_lowercase())
                .or_insert(index);
        }
    }
    let mut unassigned = Unassigned::default();
    // Before any resource is assigned, so folded-in names claim their assets.
    let bindings = speakers::bind(inputs.story.iter().filter_map(|line| {
        let resource = inputs.character_keys.get(line.character_key.as_deref()?)?;
        Some((line.speaker.as_deref()?, resource.as_str()))
    }));
    let discovered_speakers = builder.apply_bindings(&bindings);

    for (index, asset) in inputs.portraits.iter().enumerate() {
        match asset.group.as_deref() {
            Some(group) if builder.find(group).is_some() || is_plain_name(group) => {
                let character = builder.discover(group, Origin::Portrait);
                builder.characters[character].portraits.push(index);
            }
            _ => unassigned.portraits.push(index),
        }
    }
    for (index, asset) in inputs.live2d.iter().enumerate() {
        match asset.group.as_deref() {
            Some(group) if builder.find(group).is_some() || is_plain_name(group) => {
                let character = builder.discover(group, Origin::Live2d);
                builder.characters[character].live2d.push(index);
            }
            _ => unassigned.live2d.push(index),
        }
    }
    for source in inputs.audio {
        if let Some(name) = servr_name(source) {
            builder.discover(name, Origin::Audio);
        }
    }
    for (resource, speakers) in discovered_speakers {
        if let Some(index) = builder.find(&resource) {
            let character = &mut builder.characters[index];
            character.name = speakers[0].clone();
            character.bound_speakers = speakers;
        }
    }
    // Token match on the resource name or an alias (`matches_character`),
    // story chapter packs excluded.
    let match_names: Vec<Vec<String>> = builder
        .characters
        .iter()
        .map(Character::resource_names)
        .collect();
    for (index, source) in inputs.audio.iter().enumerate() {
        if !voice_pack(source) {
            *unassigned.other_audio.entry(source.category).or_default() += 1;
            continue;
        }
        let mut matched = false;
        for (character, names) in match_names.iter().enumerate() {
            if source.matches_character(names) {
                builder.characters[character].voice.push(index);
                matched = true;
            }
        }
        if !matched {
            if source.category == Category::Character {
                unassigned.voice.push(index);
            } else {
                *unassigned.other_audio.entry(source.category).or_default() += 1;
            }
        }
    }

    // Discovered entries with nothing but a few story lines are not
    // characters; their models and lines fall through to "unassigned".
    let lookup = speaker_lookup(&builder.characters);
    let mut cn_lines = vec![0usize; builder.characters.len()];
    for line in inputs.story.iter().filter(|line| line.is_cn()) {
        if let SpeakerMatch::Character(index) = lookup.classify(line.speaker.as_deref()) {
            cn_lines[index] += 1;
        }
    }
    let mut position = 0;
    builder.characters.retain(|character| {
        let keep = character.in_catalog()
            || !character.voice.is_empty()
            || !character.portraits.is_empty()
            || cn_lines[position] >= STORY_PERSON_MIN_LINES;
        position += 1;
        if !keep {
            unassigned.live2d.extend(&character.live2d);
        }
        keep
    });
    unassigned.live2d.sort_unstable();

    let lookup = speaker_lookup(&builder.characters);
    let mut report = ReportBuilder::default();
    for (index, line) in inputs.story.iter().enumerate() {
        let speaker = line.speaker.as_deref();
        let result = lookup.classify(speaker);
        report.add(speaker, result);
        match result {
            SpeakerMatch::Character(character) => {
                builder.characters[character].story_lines.push(index);
            }
            SpeakerMatch::Unmatched => unassigned.story_lines.push(index),
            SpeakerMatch::NonCharacter | SpeakerMatch::NoSpeaker => {}
        }
    }
    Roster {
        characters: builder.characters,
        unassigned,
        speakers: report.finish(),
    }
}

#[cfg(test)]
impl Roster {
    pub fn find(&self, name: &str) -> Option<&Character> {
        self.characters.iter().find(|c| {
            c.resource_name.eq_ignore_ascii_case(name)
                || c.name == name
                || c.aliases.iter().any(|a| a.eq_ignore_ascii_case(name))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn option(official: &str, resource: &str, aliases: &[&str]) -> CharacterOption {
        CharacterOption {
            official_name: official.into(),
            english_name: resource.into(),
            resource_name: resource.into(),
            aliases: aliases.iter().map(|a| (*a).to_owned()).collect(),
        }
    }

    fn audio(name: &str) -> SourceInfo {
        SourceInfo::from_path(Path::new(name), 1).unwrap()
    }

    fn asset(name: &str, group: Option<&str>) -> AssetRef {
        AssetRef {
            name: name.into(),
            group: group.map(String::from),
        }
    }

    fn line(script: &str, speaker: Option<&str>) -> StoryLine {
        StoryLine {
            script: script.into(),
            speaker: speaker.map(String::from),
            character_key: None,
        }
    }

    /// `count` CN lines of `speaker` showing the image `key`.
    fn keyed(speaker: &str, key: &str, count: usize) -> Vec<StoryLine> {
        vec![
            StoryLine {
                script: "CN_M01001.book".into(),
                speaker: Some(speaker.into()),
                character_key: Some(key.into()),
            };
            count
        ]
    }

    fn keys(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(key, resource)| ((*key).to_owned(), (*resource).to_owned()))
            .collect()
    }

    fn built(
        catalog: &[CharacterOption],
        audio: &[SourceInfo],
        portraits: &[AssetRef],
        live2d: &[AssetRef],
        story: &[StoryLine],
        character_keys: &HashMap<String, String>,
    ) -> Roster {
        build(&Inputs {
            catalog,
            audio,
            portraits,
            live2d,
            story,
            character_keys,
        })
    }

    #[test]
    fn resource_names_come_from_cha_file_names() {
        assert_eq!(
            resource_of_file("AVG/37dailiren/Character/CHA_Lenten_Suit_Mayor_01.prefab"),
            Some("Lenten")
        );
        assert_eq!(resource_of_file("cha_Mayling.prefab"), Some("Mayling"));
        assert_eq!(resource_of_file("AVG/Character/SoundOnly_02.prefab"), None);
        assert_eq!(resource_of_file("AVG/greencat/Xcha_Foo.prefab"), None);
        let files = [
            CharacterFile {
                name: "美玲".into(),
                file: "AVG/CHA_Mayling_01.prefab".into(),
            },
            CharacterFile {
                name: "美玲".into(),
                file: "AVG/CHA_Other_02.prefab".into(),
            },
            CharacterFile {
                name: "声道讯息".into(),
                file: "AVG/SoundOnly_02.prefab".into(),
            },
        ];
        assert_eq!(character_keys(&files), keys(&[("美玲", "Mayling")]));
    }

    #[test]
    fn catalogue_named_speakers_fold_the_resource_in() {
        let catalog = [
            option("埃芙", "Alva", &[]),
            option("巴希达", "Balthilde", &[]),
        ];
        let audio = [audio("VO_ServR_Alf_CN.awb")];
        let portraits = [asset("Avatar_Whole_AlfSSR", Some("Alf"))];
        let live2d = [
            asset("CHA_Alf_01", Some("Alf")),
            asset("CHA_Bathilda_01", Some("Bathilda")),
        ];
        let mut story = keyed("埃芙", "埃芙键", 30);
        story.extend(keyed("AN-94", "安键", 25));
        story.extend(keyed("巴希达", "巴键", 20));
        let roster = built(
            &catalog,
            &audio,
            &portraits,
            &live2d,
            &story,
            &keys(&[("埃芙键", "Alf"), ("安键", "Alf"), ("巴键", "Bathilda")]),
        );
        assert_eq!(roster.characters.len(), 2);
        let alva = roster.find("Alva").unwrap();
        assert_eq!(alva.aliases, ["Alf"]);
        assert_eq!(alva.live2d, [0]);
        assert_eq!(alva.portraits, [0]);
        assert_eq!(alva.voice, [0]);
        // The ASCII speaker matches lines but not audio or sprites.
        assert_eq!(alva.story_lines.len(), 55);
        assert_eq!(alva.resource_names(), ["Alva", "Alf"]);
        assert_eq!(alva.speaker_aliases(), ["AN-94"]);
        assert_eq!(roster.find("Balthilde").unwrap().live2d, [1]);
        assert!(roster.unassigned.live2d.is_empty());
    }

    #[test]
    fn own_resources_take_bound_speakers_as_aliases_only_for_lines() {
        let catalog = [
            option("夏安", "Cheyenne", &["Cheyanne"]),
            option("托洛洛", "Tololo", &[]),
        ];
        let audio = [audio("VO_M200_Lobby_CN.awb")];
        let mut story = keyed("M200", "夏键", 40);
        story.extend(keyed("夏安", "夏键", 30));
        // Named after another catalogue character: not taken as an alias.
        story.extend(keyed("托洛洛", "夏键", 25));
        let roster = built(
            &catalog,
            &audio,
            &[],
            &[],
            &story,
            &keys(&[("夏键", "cheyanne")]),
        );
        let cheyenne = roster.find("Cheyenne").unwrap();
        assert_eq!(cheyenne.bound_speakers, ["M200", "夏安"]);
        assert_eq!(cheyenne.aliases, ["Cheyanne"]);
        assert_eq!(cheyenne.speaker_aliases(), ["M200"]);
        assert_eq!(cheyenne.story_lines.len(), 70);
        assert_eq!(cheyenne.voice, Vec::<usize>::new());
        assert_eq!(roster.find("Tololo").unwrap().story_lines.len(), 25);
    }

    #[test]
    fn discovered_resources_are_renamed_after_their_top_speaker() {
        let portraits = [asset("Avatar_Whole_MaylingSSR", Some("Mayling"))];
        let mut story = keyed("美玲", "美键", 50);
        story.extend(keyed("美玲？", "美键", 30));
        // Too few lines of its own: stays unmatched.
        story.extend(keyed("路人", "美键", 19));
        let roster = built(
            &[],
            &[],
            &portraits,
            &[],
            &story,
            &keys(&[("美键", "Mayling")]),
        );
        let mayling = roster.find("Mayling").unwrap();
        assert_eq!(mayling.name, "美玲");
        assert_eq!(mayling.resource_name, "Mayling");
        assert_eq!(mayling.speaker_aliases(), ["美玲？"]);
        assert_eq!(mayling.speaker_names(), ["美玲", "美玲？"]);
        assert_eq!(mayling.story_lines.len(), 80);
        assert_eq!(mayling.resource_names(), ["Mayling"]);
        assert_eq!(roster.unassigned.story_lines.len(), 19);
    }

    #[test]
    fn generic_discovered_entries_fall_through_to_unassigned() {
        let portraits = [asset("Avatar_Whole_PosedSSR", Some("Posed"))];
        let audio = [audio("VO_ServR_Voiced_CN.awb")];
        let live2d = [
            asset("CHA_Worker_01", Some("Worker")),
            asset("CHA_Worker_02", Some("Worker")),
            asset("CHA_Busy_01", Some("Busy")),
            asset("CHA_Posed_01", Some("Posed")),
            asset("Odd", None),
        ];
        let mut story = keyed("工头", "工键", 62);
        story.extend(keyed("主角", "忙键", STORY_PERSON_MIN_LINES));
        let roster = built(
            &[],
            &audio,
            &portraits,
            &live2d,
            &story,
            &keys(&[("工键", "Worker"), ("忙键", "Busy")]),
        );
        assert!(roster.find("Worker").is_none());
        assert_eq!(roster.unassigned.live2d, [0, 1, 4]);
        assert_eq!(roster.unassigned.story_lines.len(), 62);
        assert_eq!(roster.speakers.top_unmatched[0].name, "工头");
        // Enough CN lines, a portrait or a voice pack each keep an entry.
        let busy = roster.find("Busy").unwrap();
        assert_eq!(
            (busy.name.as_str(), busy.live2d.as_slice()),
            ("主角", &[2][..])
        );
        assert_eq!(busy.story_lines.len(), STORY_PERSON_MIN_LINES);
        assert_eq!(roster.find("Posed").unwrap().live2d, [3]);
        assert_eq!(roster.find("Voiced").unwrap().voice, [0]);
    }

    #[test]
    fn original_region_lines_do_not_keep_a_generic_entry() {
        let live2d = [asset("CHA_Worker_01", Some("Worker"))];
        let story: Vec<StoryLine> = keyed("工头", "工键", 300)
            .into_iter()
            .map(|mut line| {
                line.script = "M01001.book".into();
                line
            })
            .collect();
        let roster = built(&[], &[], &[], &live2d, &story, &keys(&[("工键", "Worker")]));
        assert!(roster.find("Worker").is_none());
        assert_eq!(roster.unassigned.live2d, [0]);
        assert_eq!(roster.unassigned.story_lines.len(), 300);
    }

    fn roster() -> Roster {
        let catalog = [
            option("克罗丽科", "Charolic", &["Krolik"]),
            option("佩里缇亚", "Peritya", &[]),
            option("佩里", "Peri", &[]),
        ];
        let audio = [
            audio("VO_ServR_Charolic_CN.awb"),
            audio("VO_Bedroom_krolik_JP.awb"),
            audio("VO_Lobby_Peritya_CN.awb"),
            audio("VO_ServR_Newgirl_CN.awb"),
            audio("VO_Chapter_01_Charolic_CN.awb"),
            audio("VO_Battle_Nobody_CN.awb"),
            audio("BGM_Agent_Battle.awb"),
            audio("C_SR01_Charolic.acb"),
        ];
        let portraits = [
            asset("Avatar_Whole_CharolicSR", Some("Charolic")),
            asset("Avatar_Whole_CeciliaSSR", Some("Cecilia")),
            asset("Avatar_Whole_NeuralCloud_Lind", Some("NeuralCloud_Lind")),
        ];
        let live2d = [
            asset("CHA_charolic_SR_01", Some("charolic")),
            asset("UI_Something", None),
        ];
        let story = [
            line("CN_M01001", Some("克罗丽科")),
            line("M01001", Some("克罗丽科")),
            line("CN_M01001", Some("佩里")),
            line("CN_M01001", Some("克罗")),
            line("CN_M01001", Some("？？")),
            line("CN_M01001", Some("指挥官")),
            line("CN_M01001", None),
        ];
        let roster = build(&Inputs {
            catalog: &catalog,
            audio: &audio,
            portraits: &portraits,
            live2d: &live2d,
            story: &story,
            character_keys: &HashMap::new(),
        });
        assert!(roster.characters.iter().all(|c| !c.origins.is_empty()));
        roster
    }

    #[test]
    fn assigns_resources_case_insensitively_with_aliases() {
        let roster = roster();
        let charolic = roster.find("charolic").unwrap();
        // ServR, the `krolik` alias and a non-VO pack; not the chapter pack.
        assert_eq!(charolic.voice, [0, 1, 7]);
        assert_eq!(charolic.portraits, [0]);
        assert_eq!(charolic.live2d, [0]);
        assert_eq!(charolic.story_lines, [0, 1]);
        assert_eq!(
            charolic.origins,
            [
                Origin::Catalog,
                Origin::Audio,
                Origin::Portrait,
                Origin::Live2d
            ]
        );
        // `Peri` does not take `Peritya`'s pack.
        assert_eq!(roster.find("Peri").unwrap().voice, Vec::<usize>::new());
        assert_eq!(roster.find("Peritya").unwrap().voice, [2]);
        assert_eq!(roster.find("Peri").unwrap().story_lines, [2]);
    }

    #[test]
    fn discovers_plain_names_and_leaves_the_rest_unassigned() {
        let roster = roster();
        let cecilia = roster.find("Cecilia").unwrap();
        assert!(!cecilia.in_catalog());
        assert_eq!(cecilia.name, "Cecilia");
        let newgirl = roster.find("newgirl").unwrap();
        assert_eq!(
            (newgirl.origins.as_slice(), newgirl.voice.as_slice()),
            (&[Origin::Audio][..], &[3][..])
        );
        assert!(roster.find("NeuralCloud_Lind").is_none());
        assert_eq!(roster.unassigned.portraits, [2]);
        assert_eq!(roster.unassigned.live2d, [1]);
        assert_eq!(roster.unassigned.voice, [5]);
        assert_eq!(
            roster.unassigned.other_audio.get(&Category::Story),
            Some(&1)
        );
        assert_eq!(
            roster.unassigned.other_audio.get(&Category::Music),
            Some(&1)
        );
    }

    #[test]
    fn story_lines_need_exact_names_and_skip_non_characters() {
        let roster = roster();
        // `克罗` is not an alias and is reported as unmatched.
        assert_eq!(roster.unassigned.story_lines, [3]);
        assert_eq!(roster.speakers.top_unmatched[0].name, "克罗");
        assert_eq!(roster.speakers.non_character_lines, 2);
        assert_eq!(roster.speakers.lines_without_speaker, 1);
        let charolic = roster.find("Charolic").unwrap();
        assert_eq!(
            charolic
                .story_lines
                .iter()
                .filter(|&&i| i == 0 || i == 1)
                .count(),
            2
        );
    }

    #[test]
    fn cn_scripts_are_recognised() {
        assert!(line("CN_M01001", None).is_cn());
        assert!(line("cn_x", None).is_cn());
        assert!(!line("M01001", None).is_cn());
    }
}
