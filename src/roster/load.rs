//! Builds the roster from a game directory (building any missing index)
//! and produces the `--characters` report.

use super::{
    AssetRef, Inputs, Origin, Roster,
    avatar::{self, Avatar, Overrides},
    speakers::SpeakerReport,
    survey::{self, Survey},
};
use crate::{
    catalog::{Category, scan_resources},
    characters::load_character_options,
    live2d, portrait, story,
};
use anyhow::{Context, Result};
use serde::Serialize;
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
    time::Instant,
};

const SAMPLE_LIMIT: usize = 10;

#[derive(Serialize)]
pub struct CharacterReport {
    pub resource_name: String,
    pub name: String,
    pub aliases: Vec<String>,
    pub origins: Vec<Origin>,
    pub voice_packs: usize,
    pub portraits: usize,
    pub live2d_models: usize,
    pub story_lines: usize,
    pub story_lines_cn: usize,
    pub story_lines_original: usize,
    pub avatar: Option<Avatar>,
}

#[derive(Serialize, Default)]
pub struct Bucket {
    pub assigned: usize,
    pub unassigned: usize,
    pub unassigned_samples: Vec<String>,
}

#[derive(Serialize)]
pub struct Totals {
    pub characters: usize,
    pub catalog_characters: usize,
    pub discovered_characters: usize,
    pub with_voice: usize,
    pub with_portraits: usize,
    pub with_live2d: usize,
    pub with_story_lines: usize,
    pub with_avatar: usize,
    pub voice_packs: Bucket,
    pub portraits: Bucket,
    pub live2d_models: Bucket,
    pub story_lines: Bucket,
    /// Story lines of non-character speakers or without a speaker.
    pub story_lines_not_characters: usize,
    /// Audio that never belongs to a character, by category.
    pub other_audio: BTreeMap<Category, usize>,
}

#[derive(Serialize)]
pub struct IndexTimes {
    pub audio_ms: u128,
    pub story_ms: u128,
    pub story_from_cache: bool,
    pub portrait_ms: u128,
    pub portrait_from_cache: bool,
    pub live2d_ms: u128,
    pub live2d_from_cache: bool,
    pub sprite_names_ms: u128,
    pub sprite_names_from_cache: bool,
    pub avatars_ms: u128,
    pub total_ms: u128,
}

#[derive(Serialize)]
pub struct Report {
    pub totals: Totals,
    pub characters: Vec<CharacterReport>,
    pub speakers: SpeakerReport,
    pub avatar_survey: Survey,
    pub avatar_directory: Option<PathBuf>,
    pub avatar_overrides: Option<PathBuf>,
    pub warnings: Vec<String>,
    pub timings: IndexTimes,
}

fn samples<'a>(names: impl Iterator<Item = &'a str>) -> Vec<String> {
    names.take(SAMPLE_LIMIT).map(str::to_owned).collect()
}

/// Lower-cased names and aliases → canonical resource name.
fn name_map(roster: &Roster) -> HashMap<String, String> {
    let mut names = HashMap::new();
    for character in &roster.characters {
        for name in character.resource_names() {
            names
                .entry(name.to_ascii_lowercase())
                .or_insert_with(|| character.resource_name.clone());
        }
    }
    names
}

/// Loads every index (from cache when valid), builds the roster and
/// renders avatars into `avatar_directory` (the app cache when `None`).
pub fn report(game: &Path, avatar_directory: Option<&Path>, cancel: &AtomicBool) -> Result<Report> {
    let started = Instant::now();
    let mut warnings = Vec::new();
    let catalog = load_character_options()?;

    let step = Instant::now();
    let audio = scan_resources(game, cancel);
    let audio_ms = step.elapsed().as_millis();
    warnings.extend(audio.warnings.iter().take(SAMPLE_LIMIT).cloned());

    let step = Instant::now();
    let story_index = story::load_or_build(game, true, cancel, &|_| {})?
        .context(tr!("剧情索引已取消", "Story indexing was cancelled"))?;
    let story_ms = step.elapsed().as_millis();
    let lines = super::story_lines(&story_index);
    let character_keys = super::character_keys(&story_index.character_files);

    let step = Instant::now();
    let portrait_index = portrait::load_or_build(game, true, cancel, &|_| {})?
        .context(tr!("立绘索引已取消", "Portrait indexing was cancelled"))?;
    let portrait_ms = step.elapsed().as_millis();
    let portraits: Vec<AssetRef> = portrait_index
        .entries
        .iter()
        .map(|entry| AssetRef {
            name: entry.portrait.name.clone(),
            group: Some(entry.character_resource.clone()),
        })
        .collect();

    let step = Instant::now();
    let live2d_index = live2d::load_or_build(game, true, cancel, &|_| {})?
        .context(tr!("Live2D 索引已取消", "Live2D indexing was cancelled"))?;
    let live2d_ms = step.elapsed().as_millis();
    let models: Vec<AssetRef> = live2d_index
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
        .collect();

    let roster = super::build(&Inputs {
        catalog: &catalog,
        audio: &audio.sources,
        portraits: &portraits,
        live2d: &models,
        story: &lines,
        character_keys: &character_keys,
    });

    let step = Instant::now();
    let sprite_names = survey::load_or_scan(game, cancel)?.context(tr!(
        "Sprite 名称扫描已取消",
        "The sprite name scan was cancelled"
    ))?;
    let sprite_names_ms = step.elapsed().as_millis();
    let names = name_map(&roster);
    let with_portraits: BTreeSet<String> = roster
        .characters
        .iter()
        .filter(|c| !c.portraits.is_empty())
        .map(|c| c.resource_name.clone())
        .collect();
    let mut avatar_survey = survey::survey(&sprite_names.sprites, &names, &with_portraits);
    avatar_survey.from_cache = sprite_names.from_cache;
    avatar_survey.scan_elapsed_ms = sprite_names.scan_elapsed_ms;
    avatar_survey.scan_warnings = sprite_names.warnings;

    let step = Instant::now();
    let overrides_path = avatar::overrides_path();
    let overrides = match &overrides_path {
        Some(path) => {
            let (overrides, warning) = Overrides::load(path);
            warnings.extend(warning);
            overrides
        }
        None => Overrides::default(),
    };
    let portrait_names: Vec<String> = portraits.iter().map(|p| p.name.clone()).collect();
    let plans = avatar::plan(
        &roster,
        &portrait_names,
        &sprite_names.sprites,
        &names,
        &avatar_survey,
        &overrides,
    );
    let directory = avatar_directory
        .map(Path::to_path_buf)
        .or_else(avatar::avatar_dir);
    let portrait_list: Vec<_> = portrait_index
        .entries
        .iter()
        .map(|e| e.portrait.clone())
        .collect();
    let mut avatars = match &directory {
        Some(directory) => avatar::generate(
            &roster,
            &plans,
            &avatar::Sources {
                bundle_root: &sprite_names.bundle_root,
                bundles: &sprite_names.bundles,
                fingerprint: &sprite_names.fingerprint,
                portraits: &portrait_list,
                sprites: &sprite_names.sprites,
            },
            directory,
            cancel,
        )?,
        None => {
            warnings.push(
                tr!(
                    "无法确定程序所在目录，未生成头像",
                    "Cannot determine the program's folder; avatars were not generated"
                )
                .into(),
            );
            BTreeMap::new()
        }
    };
    let avatars_ms = step.elapsed().as_millis();

    let characters: Vec<CharacterReport> = roster
        .characters
        .iter()
        .enumerate()
        .map(|(index, c)| {
            let cn = c.story_lines.iter().filter(|&&i| lines[i].is_cn()).count();
            CharacterReport {
                resource_name: c.resource_name.clone(),
                name: c.name.clone(),
                aliases: c
                    .aliases
                    .iter()
                    .cloned()
                    .chain(c.speaker_aliases())
                    .collect(),
                origins: c.origins.clone(),
                voice_packs: c.voice.len(),
                portraits: c.portraits.len(),
                live2d_models: c.live2d.len(),
                story_lines: c.story_lines.len(),
                story_lines_cn: cn,
                story_lines_original: c.story_lines.len() - cn,
                avatar: avatars.remove(&index),
            }
        })
        .collect();
    let distinct = |lists: &mut dyn Iterator<Item = &Vec<usize>>| {
        lists.flatten().collect::<BTreeSet<_>>().len()
    };
    let unassigned = &roster.unassigned;
    let assigned_lines = distinct(&mut roster.characters.iter().map(|c| &c.story_lines));
    let totals = Totals {
        characters: characters.len(),
        catalog_characters: roster.characters.iter().filter(|c| c.in_catalog()).count(),
        discovered_characters: roster.characters.iter().filter(|c| !c.in_catalog()).count(),
        with_voice: characters.iter().filter(|c| c.voice_packs > 0).count(),
        with_portraits: characters.iter().filter(|c| c.portraits > 0).count(),
        with_live2d: characters.iter().filter(|c| c.live2d_models > 0).count(),
        with_story_lines: characters.iter().filter(|c| c.story_lines > 0).count(),
        with_avatar: characters
            .iter()
            .filter(|c| c.avatar.as_ref().is_some_and(|a| a.path.is_some()))
            .count(),
        voice_packs: Bucket {
            assigned: distinct(&mut roster.characters.iter().map(|c| &c.voice)),
            unassigned: unassigned.voice.len(),
            unassigned_samples: samples(
                unassigned
                    .voice
                    .iter()
                    .map(|&i| audio.sources[i].key.as_str()),
            ),
        },
        portraits: Bucket {
            assigned: distinct(&mut roster.characters.iter().map(|c| &c.portraits)),
            unassigned: unassigned.portraits.len(),
            unassigned_samples: samples(
                unassigned
                    .portraits
                    .iter()
                    .map(|&i| portraits[i].name.as_str()),
            ),
        },
        live2d_models: Bucket {
            assigned: distinct(&mut roster.characters.iter().map(|c| &c.live2d)),
            unassigned: unassigned.live2d.len(),
            unassigned_samples: samples(unassigned.live2d.iter().map(|&i| models[i].name.as_str())),
        },
        story_lines: Bucket {
            assigned: assigned_lines,
            unassigned: unassigned.story_lines.len(),
            unassigned_samples: Vec::new(),
        },
        story_lines_not_characters: lines.len() - assigned_lines - unassigned.story_lines.len(),
        other_audio: unassigned.other_audio.clone(),
    };
    Ok(Report {
        totals,
        characters,
        speakers: roster.speakers,
        avatar_survey,
        avatar_directory: directory,
        avatar_overrides: overrides_path,
        warnings,
        timings: IndexTimes {
            audio_ms,
            story_ms,
            story_from_cache: story_index.summary.from_cache,
            portrait_ms,
            portrait_from_cache: portrait_index.summary.from_cache,
            live2d_ms,
            live2d_from_cache: live2d_index.summary.from_cache,
            sprite_names_ms,
            sprite_names_from_cache: sprite_names.from_cache,
            avatars_ms,
            total_ms: started.elapsed().as_millis(),
        },
    })
}
