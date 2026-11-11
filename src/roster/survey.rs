//! Sprite name survey: which name prefixes form a per-character series
//! (`Avatar_Head_*`, `Icon_*` …) that could serve as avatars.

use crate::{
    portrait::index::{NAME_PREFIX, SpriteRef},
    story::{CachePolicy, fingerprint, resolve_data_root, resource_version},
};
use anyhow::{Context, Result};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
    time::Instant,
};

pub const CACHE_VERSION: u32 = 1;
/// A series must cover at least this share of characters with portraits.
pub const COVERAGE_THRESHOLD: f64 = 0.8;
const SAMPLE_LIMIT: usize = 6;
const REPORT_LIMIT: usize = 25;

pub fn cache_path() -> Option<PathBuf> {
    crate::paths::data_dir().map(|dir| dir.join("sprite-names.json"))
}

#[derive(Serialize, Deserialize)]
pub struct CacheFile {
    pub version: u32,
    pub fingerprint: String,
    pub data_root: PathBuf,
    pub scan_elapsed_ms: u128,
    pub warnings: usize,
    pub sprites: Vec<SpriteRef>,
}

pub struct SpriteNames {
    pub fingerprint: String,
    pub bundle_root: PathBuf,
    pub bundles: Vec<PathBuf>,
    pub sprites: Vec<SpriteRef>,
    pub from_cache: bool,
    pub scan_elapsed_ms: u128,
    pub warnings: usize,
}

fn load_cache(
    path: &Path,
    policy: CachePolicy,
    fingerprint: &str,
    data_root: &Path,
) -> Option<CacheFile> {
    let file: CacheFile = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    (file.version == CACHE_VERSION
        && policy.accepts(&file.fingerprint, fingerprint)
        && file.data_root == data_root)
        .then_some(file)
}

/// Lists every Sprite name in the game's bundles, cached by the same
/// fingerprint as the other indexes. Returns `Ok(None)` when cancelled.
pub fn load_or_scan(game: &Path, cancel: &AtomicBool) -> Result<Option<SpriteNames>> {
    load_with_policy(game, CachePolicy::Current, cancel)
}

/// Lists the Sprite names as `policy` allows, rescanning otherwise. The
/// result carries the fingerprint of the files it was scanned from, so
/// avatars made from a stale list stay valid until it is replaced.
pub fn load_with_policy(
    game: &Path,
    policy: CachePolicy,
    cancel: &AtomicBool,
) -> Result<Option<SpriteNames>> {
    let data_root = resolve_data_root(game)?;
    let bundle_root = data_root.join("AssetBundles_Windows");
    let bundles = crate::story::index::physical_bundles(&bundle_root)?;
    let version = resource_version(game, &data_root);
    let fingerprint = fingerprint(&version, &data_root, &bundles)?;
    let cache = cache_path();
    let cached = cache
        .as_deref()
        .filter(|_| policy != CachePolicy::Rebuild)
        .and_then(|path| load_cache(path, policy, &fingerprint, &data_root));
    if let Some(file) = cached {
        return Ok(Some(SpriteNames {
            fingerprint: file.fingerprint,
            bundle_root,
            bundles,
            sprites: file.sprites,
            from_cache: true,
            scan_elapsed_ms: file.scan_elapsed_ms,
            warnings: file.warnings,
        }));
    }
    let started = Instant::now();
    let spec = crate::portrait::index::ScanSpec {
        matches: &|_| false,
        collect_names: true,
    };
    let output =
        crate::portrait::index::scan_sprites(&bundle_root, &bundles, &spec, cancel, &|_, _| {});
    if output.cancelled {
        return Ok(None);
    }
    let file = CacheFile {
        version: CACHE_VERSION,
        fingerprint,
        data_root,
        scan_elapsed_ms: started.elapsed().as_millis(),
        warnings: output.warnings.len(),
        sprites: output.sprite_names,
    };
    if let Some(path) = &cache {
        crate::portrait::write_atomically(path, &serde_json::to_vec(&file)?).context(tr!(
            "无法写入 Sprite 名称缓存",
            "Cannot write the sprite name cache"
        ))?;
    }
    Ok(Some(SpriteNames {
        fingerprint: file.fingerprint,
        bundle_root,
        bundles,
        sprites: file.sprites,
        from_cache: false,
        scan_elapsed_ms: file.scan_elapsed_ms,
        warnings: file.warnings,
    }))
}

/// Where a character name occurs in a sprite name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NameHit {
    pub prefix: String,
    /// Canonical resource name of the character.
    pub character: String,
    /// Text after the character name (`SSR01`, `_Blur` …).
    pub rest: String,
}

/// Finds the first character name in `sprite` that starts a word (string
/// start, after a non-alphanumeric, or a capital letter) and is not
/// followed by a lower-case letter, so `Peri` does not match `Peritya`.
/// `names` maps lower-cased names and aliases to canonical resource names;
/// the longest name wins at a position.
pub fn find_character(sprite: &str, names: &HashMap<String, String>) -> Option<NameHit> {
    let lower = sprite.to_ascii_lowercase();
    let bytes = sprite.as_bytes();
    for start in 0..bytes.len() {
        if !sprite.is_char_boundary(start) {
            continue;
        }
        let starts_word = start == 0
            || !bytes[start - 1].is_ascii_alphanumeric()
            || bytes[start].is_ascii_uppercase();
        if !starts_word || !bytes[start].is_ascii_alphabetic() {
            continue;
        }
        let best = names
            .iter()
            .filter(|(name, _)| {
                lower[start..].starts_with(name.as_str())
                    && bytes
                        .get(start + name.len())
                        .is_none_or(|next| !next.is_ascii_lowercase())
            })
            .max_by_key(|(name, _)| name.len());
        if let Some((name, character)) = best {
            return Some(NameHit {
                prefix: sprite[..start].to_owned(),
                character: character.clone(),
                rest: sprite[start + name.len()..].to_owned(),
            });
        }
    }
    None
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct PrefixStats {
    pub prefix: String,
    pub sprites: usize,
    pub characters: usize,
    /// Characters with portraits that this series covers.
    pub portrait_characters_covered: usize,
    pub coverage: f64,
    pub samples: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Survey {
    pub sprites_total: usize,
    pub sprites_with_character: usize,
    pub portrait_characters: usize,
    pub threshold: f64,
    /// Prefixes by covered characters, most first (top entries only).
    pub prefixes: Vec<PrefixStats>,
    /// The best series meeting the threshold, if any.
    pub chosen_prefix: Option<String>,
    pub decision: String,
    pub from_cache: bool,
    pub scan_elapsed_ms: u128,
    pub scan_warnings: usize,
}

/// Preferred avatar series, closest crop first.
const SERIES_PREFERENCE: [&str; 3] = ["Avatar_Head_", "Avatar_Bust_", "Avatar_Half_"];

fn is_avatar_series(prefix: &str) -> bool {
    prefix.len() > "Avatar_".len()
        && prefix[.."Avatar_".len()].eq_ignore_ascii_case("Avatar_")
        && !prefix.eq_ignore_ascii_case(NAME_PREFIX)
}

fn series_rank(prefix: &str) -> usize {
    SERIES_PREFERENCE
        .iter()
        .position(|p| p.eq_ignore_ascii_case(prefix))
        .unwrap_or(SERIES_PREFERENCE.len())
}

/// Rank of a sprite within a series for one character: the base sprite
/// (`…SSR`, `…SR` or nothing after the name) first, then numbered
/// variants (`…SSR01`), then anything else (`…Dorm`, `…SSRUP`, `…_Spine`).
pub fn variant_rank(rest: &str) -> (u8, usize) {
    let rarity = ["SSR", "SR"]
        .iter()
        .find_map(|r| rest.strip_prefix(r))
        .unwrap_or(rest);
    let rank = if rarity.is_empty() {
        0
    } else if rarity.bytes().all(|b| b.is_ascii_digit()) {
        1
    } else {
        2
    };
    (rank, rest.len())
}

/// Groups sprite names by the text before the character name. Only
/// `Avatar_*` series other than the full-body portraits can be chosen;
/// among those meeting the threshold, `Avatar_Head_` is preferred.
pub fn survey(
    sprites: &[SpriteRef],
    names: &HashMap<String, String>,
    portrait_characters: &BTreeSet<String>,
) -> Survey {
    struct Group {
        sprites: usize,
        characters: BTreeSet<String>,
        samples: Vec<String>,
    }
    let mut groups: BTreeMap<String, Group> = BTreeMap::new();
    let mut with_character = 0;
    let hits: Vec<Option<NameHit>> = sprites
        .par_iter()
        .map(|sprite| find_character(&sprite.name, names))
        .collect();
    for (sprite, hit) in sprites.iter().zip(hits) {
        let Some(hit) = hit else {
            continue;
        };
        with_character += 1;
        let group = groups.entry(hit.prefix).or_insert_with(|| Group {
            sprites: 0,
            characters: BTreeSet::new(),
            samples: Vec::new(),
        });
        group.sprites += 1;
        if group.characters.insert(hit.character) && group.samples.len() < SAMPLE_LIMIT {
            group.samples.push(sprite.name.clone());
        }
    }
    let denominator = portrait_characters.len().max(1) as f64;
    let mut prefixes: Vec<PrefixStats> = groups
        .into_iter()
        .map(|(prefix, group)| {
            let covered = group
                .characters
                .iter()
                .filter(|c| portrait_characters.contains(*c))
                .count();
            PrefixStats {
                prefix,
                sprites: group.sprites,
                characters: group.characters.len(),
                portrait_characters_covered: covered,
                coverage: (covered as f64 / denominator * 1000.0).round() / 1000.0,
                samples: group.samples,
            }
        })
        .collect();
    prefixes.sort_by(|a, b| {
        b.portrait_characters_covered
            .cmp(&a.portrait_characters_covered)
            .then(b.characters.cmp(&a.characters))
            .then(a.prefix.cmp(&b.prefix))
    });
    // Only character-image series (`Avatar_*` other than the full-body
    // portraits) are candidates; skill, item and banner series that also
    // carry character names are reported but never chosen.
    let mut candidates: Vec<&PrefixStats> = prefixes
        .iter()
        .filter(|p| is_avatar_series(&p.prefix))
        .collect();
    candidates.sort_by_key(|p| series_rank(&p.prefix));
    let chosen = candidates
        .iter()
        .find(|p| p.coverage >= COVERAGE_THRESHOLD)
        .map(|p| p.prefix.clone());
    let best = candidates
        .iter()
        .max_by(|a, b| a.coverage.total_cmp(&b.coverage))
        .copied();
    let decision = match (&chosen, best) {
        (Some(prefix), _) => tr!("使用现成头像系列 {prefix}*", "Using the existing avatar series {prefix}*", prefix = prefix),
        (None, Some(best)) => tr!("没有覆盖 ≥{:.0}% 有立绘角色的系列（最高为 {}* 的 {:.1}%），使用整身立绘 Alpha 自动裁切", "No series covers at least {:.0}% of characters with portraits (the highest is {}* at {:.1}%); cropping automatically from the full-body portrait's alpha",
            COVERAGE_THRESHOLD * 100.0,
            best.prefix,
            best.coverage * 100.0),
        (None, None) => tr!("没有找到含角色名的 Avatar_ 系列，使用整身立绘 Alpha 自动裁切", "No Avatar_ series contains character names; cropping automatically from the full-body portrait's alpha").into(),
    };
    prefixes.truncate(REPORT_LIMIT);
    Survey {
        sprites_total: sprites.len(),
        sprites_with_character: with_character,
        portrait_characters: portrait_characters.len(),
        threshold: COVERAGE_THRESHOLD,
        prefixes,
        chosen_prefix: chosen,
        decision,
        ..Survey::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::portrait::index::{Container, NodeRef};

    #[test]
    fn stale_sprite_names_are_only_loaded_when_accepted() {
        let directory =
            std::env::temp_dir().join(format!("echo-archive-survey-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("sprite-names.json");
        let root = Path::new("root");
        let file = CacheFile {
            version: CACHE_VERSION,
            fingerprint: "old".to_owned(),
            data_root: PathBuf::from("root"),
            scan_elapsed_ms: 0,
            warnings: 0,
            sprites: Vec::new(),
        };
        std::fs::write(&path, serde_json::to_vec(&file).unwrap()).unwrap();
        assert!(load_cache(&path, CachePolicy::AcceptStale, "new", root).is_some());
        assert!(load_cache(&path, CachePolicy::Current, "new", root).is_none());
        assert!(load_cache(&path, CachePolicy::AcceptStale, "new", Path::new("x")).is_none());
        std::fs::remove_dir_all(directory).unwrap();
    }

    fn names() -> HashMap<String, String> {
        [
            ("peri", "Peri"),
            ("peritya", "Peritya"),
            ("tololo", "Tololo"),
            ("krolik", "Charolic"),
            ("charolic", "Charolic"),
        ]
        .into_iter()
        .map(|(a, b)| (a.to_owned(), b.to_owned()))
        .collect()
    }

    #[test]
    fn finds_character_names_at_word_starts() {
        let names = names();
        let hit = find_character("Avatar_Head_TololoSSR01", &names).unwrap();
        assert_eq!(
            (
                hit.prefix.as_str(),
                hit.character.as_str(),
                hit.rest.as_str()
            ),
            ("Avatar_Head_", "Tololo", "SSR01")
        );
        assert_eq!(
            find_character("Icon_PerityaSSR", &names).unwrap().character,
            "Peritya"
        );
        assert_eq!(
            find_character("Icon_PeriSSR", &names).unwrap().character,
            "Peri"
        );
        assert_eq!(
            find_character("IconKrolik", &names).unwrap().character,
            "Charolic"
        );
        // Lower-case continuation and mid-word occurrences do not match.
        assert_eq!(find_character("Icon_Peripheral", &names), None);
        assert_eq!(find_character("Xtololo", &names), None);
    }

    #[test]
    fn survey_chooses_only_series_above_threshold() {
        let sprite = |name: &str| SpriteRef {
            name: name.into(),
            source: NodeRef {
                container: Container::default(),
                node: String::new(),
            },
            path_id: 0,
        };
        let sprites: Vec<_> = [
            "Avatar_Whole_TololoSSR",
            "Avatar_Whole_PeriSSR",
            "Avatar_Whole_CharolicSR",
            "Avatar_Head_Tololo",
            "Avatar_Head_Peri",
            "Avatar_Head_Charolic",
            "Avatar_Card_Tololo",
            "Skill_Tololo",
            "Skill_Peri",
            "Skill_Charolic",
            "UI_Button",
        ]
        .into_iter()
        .map(sprite)
        .collect();
        let with_portraits: BTreeSet<String> = ["Tololo", "Peri", "Charolic"]
            .into_iter()
            .map(String::from)
            .collect();
        let result = survey(&sprites, &names(), &with_portraits);
        assert_eq!(result.sprites_with_character, 10);
        assert_eq!(result.chosen_prefix.as_deref(), Some("Avatar_Head_"));
        let card = result
            .prefixes
            .iter()
            .find(|p| p.prefix == "Avatar_Card_")
            .unwrap();
        assert_eq!((card.sprites, card.portrait_characters_covered), (1, 1));
        // `Skill_` covers everyone but is not an avatar series.
        let without_head: Vec<_> = sprites
            .into_iter()
            .filter(|s| !s.name.starts_with("Avatar_Head_"))
            .collect();
        let result = survey(&without_head, &names(), &with_portraits);
        assert_eq!(result.chosen_prefix, None);
        assert!(
            result.decision.contains("Avatar_Card_"),
            "{}",
            result.decision
        );
    }

    #[test]
    fn base_sprites_rank_before_variants() {
        let mut rests = ["SSR01UP", "SSR_Spine", "Dorm", "SSR01", "SSR", "SSRUP"];
        rests.sort_by_key(|rest| variant_rank(rest));
        assert_eq!(
            rests,
            ["SSR", "SSR01", "Dorm", "SSRUP", "SSR01UP", "SSR_Spine"]
        );
        assert_eq!(variant_rank(""), (0, 0));
        assert_eq!(variant_rank("SR03"), (1, 4));
    }
}
