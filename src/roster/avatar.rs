//! Character avatars: a ready-made sprite series when the survey finds one,
//! otherwise an automatic head crop of the full-body portrait; either can
//! be overridden per character in `avatar-overrides.json`.

use super::{
    Roster,
    survey::{NameHit, Survey, find_character, variant_rank},
};
use crate::portrait::{
    self,
    index::{NAME_PREFIX, Portrait, ScanSpec, SpriteRef},
    stable_hash,
};
use anyhow::{Context, Result};
use image::{RgbaImage, imageops};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
};

pub const AVATAR_EDGE: u32 = 192;
/// Bumped whenever the crop or scaling changes, invalidating cached PNGs.
const METHOD_VERSION: u32 = 1;
/// Pixels at or above this alpha count as opaque for the automatic crop.
const OPAQUE_ALPHA: u8 = 128;
/// Square side as a share of the opaque bounding box height.
const HEAD_SHARE: f64 = 0.18;
/// Top band (share of the box height) whose opaque centroid centres the crop.
const CENTRE_BAND: f64 = 0.12;

pub fn avatar_dir() -> Option<PathBuf> {
    crate::paths::data_dir().map(|dir| dir.join("avatars"))
}

pub fn overrides_path() -> Option<PathBuf> {
    crate::paths::data_dir().map(|dir| dir.join("avatar-overrides.json"))
}

/// One manual override: a source sprite, a crop box `[x, y, width,
/// height]` in the sprite's top-down pixels, or both.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Override {
    pub sprite: Option<String>,
    pub rect: Option<[u32; 4]>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Overrides {
    /// Keyed by character resource name (case-insensitive).
    pub characters: BTreeMap<String, Override>,
}

impl Overrides {
    /// Missing file → empty; unreadable file → empty plus a warning.
    pub fn load(path: &Path) -> (Self, Option<String>) {
        match std::fs::read(path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (Self::default(), None),
            Err(error) => (
                Self::default(),
                Some(tr!(
                    "{}：{error}",
                    "{}: {error}",
                    path.display(),
                    error = error
                )),
            ),
            Ok(bytes) => match serde_json::from_slice(&bytes) {
                Ok(overrides) => (overrides, None),
                Err(error) => (
                    Self::default(),
                    Some(tr!(
                        "{} 格式无效，已忽略：{error}",
                        "{} has an invalid format and was ignored: {error}",
                        path.display(),
                        error = error
                    )),
                ),
            },
        }
    }

    pub fn get(&self, resource: &str) -> Option<&Override> {
        self.characters
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(resource))
            .map(|(_, value)| value)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Method {
    /// A sprite from the surveyed avatar series, scaled to fit.
    Series,
    /// Automatic head crop of a full-body portrait.
    AlphaCrop,
    /// Sprite and/or crop box from `avatar-overrides.json`.
    Override,
}

#[derive(Clone, Debug, Serialize)]
pub struct Avatar {
    pub method: Method,
    pub sprite: String,
    /// Crop box `[x, y, width, height]` used, when not the whole sprite.
    pub rect: Option<[u32; 4]>,
    pub path: Option<PathBuf>,
    pub reused: bool,
    pub error: Option<String>,
}

/// Square head box of a full-body portrait: from the top of the opaque
/// bounding box, side 18 % of its height, centred on the opaque centroid
/// of the top 12 % band; clamped to the image.
pub fn head_box(image: &RgbaImage) -> Option<[u32; 4]> {
    let (width, height) = image.dimensions();
    let opaque = |x: u32, y: u32| image.get_pixel(x, y)[3] >= OPAQUE_ALPHA;
    let rows: Vec<u32> = (0..height)
        .filter(|&y| (0..width).any(|x| opaque(x, y)))
        .collect();
    let (&top, &bottom) = (rows.first()?, rows.last()?);
    let box_height = f64::from(bottom - top + 1);
    let side = ((box_height * HEAD_SHARE).round() as u32).clamp(1, width.min(height));
    let band_end = (top + (box_height * CENTRE_BAND).round().max(1.0) as u32).min(height);
    let (mut sum, mut count) = (0u64, 0u64);
    for y in top..band_end {
        for x in 0..width {
            if opaque(x, y) {
                sum += u64::from(x);
                count += 1;
            }
        }
    }
    let centre = (sum as f64 / count.max(1) as f64).round() as i64;
    let left = (centre - i64::from(side) / 2).clamp(0, i64::from(width - side)) as u32;
    let top = top.min(height - side);
    Some([left, top, side, side])
}

/// Crops `rect` (clamped to the image) and scales it into a transparent
/// square of `AVATAR_EDGE`, keeping the aspect ratio.
pub fn render(image: &RgbaImage, rect: Option<[u32; 4]>) -> Result<RgbaImage> {
    let (width, height) = image.dimensions();
    let [x, y, w, h] = rect.unwrap_or([0, 0, width, height]);
    let x = x.min(width.saturating_sub(1));
    let y = y.min(height.saturating_sub(1));
    let w = w.min(width - x);
    let h = h.min(height - y);
    anyhow::ensure!(w > 0 && h > 0, tr!("裁切框为空", "The crop box is empty"));
    let crop = imageops::crop_imm(image, x, y, w, h).to_image();
    let scale = f64::from(AVATAR_EDGE) / f64::from(w.max(h));
    let (sw, sh) = (
        ((f64::from(w) * scale).round() as u32).clamp(1, AVATAR_EDGE),
        ((f64::from(h) * scale).round() as u32).clamp(1, AVATAR_EDGE),
    );
    let scaled = imageops::resize(&crop, sw, sh, imageops::FilterType::Lanczos3);
    let mut canvas = RgbaImage::new(AVATAR_EDGE, AVATAR_EDGE);
    imageops::overlay(
        &mut canvas,
        &scaled,
        i64::from((AVATAR_EDGE - sw) / 2),
        i64::from((AVATAR_EDGE - sh) / 2),
    );
    Ok(canvas)
}

/// What to render for one character before any decoding.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    pub character: usize,
    pub method: Method,
    pub sprite: String,
    /// Fixed crop box; `None` = automatic (head box for full-body
    /// portraits, the whole sprite otherwise).
    pub rect: Option<[u32; 4]>,
}

/// Head sprite series tried in order for characters without one in the
/// chosen series (NPCs, monsters and summons).
const SUPPLEMENTARY_SERIES: [&str; 3] = ["Avatar_Head_CHA_", "Monster_Head_", "Summon_Head_"];

/// Text after the character name in a supplementary series: nothing, a
/// `_…` suffix, `SSR`/`SR` with optional digits, or one or two digits.
/// `Blusphere02` qualifies; `GreenMew` and `U015Support` do not.
fn is_supplementary_rest(rest: &str) -> bool {
    let all_digits = |text: &str| text.bytes().all(|b| b.is_ascii_digit());
    if rest.is_empty() || rest.starts_with('_') {
        return true;
    }
    match ["SSR", "SR"].iter().find_map(|r| rest.strip_prefix(r)) {
        Some(digits) => all_digits(digits),
        None => rest.len() <= 2 && all_digits(rest),
    }
}

fn best_by_variant<'a>(candidates: impl Iterator<Item = (&'a str, String)>) -> Option<&'a str> {
    candidates
        .min_by(|(a, ra), (b, rb)| variant_rank(ra).cmp(&variant_rank(rb)).then(a.cmp(b)))
        .map(|(name, _)| name)
}

/// Picks the source sprite of every character: override, else the chosen
/// series sprite, else a supplementary series sprite, else the base
/// full-body portrait.
pub fn plan(
    roster: &Roster,
    portrait_names: &[String],
    sprites: &[SpriteRef],
    names: &HashMap<String, String>,
    survey: &Survey,
    overrides: &Overrides,
) -> Vec<Plan> {
    let mut series: HashMap<String, Vec<(&str, String)>> = HashMap::new();
    let mut supplementary: [HashMap<String, Vec<(&str, String)>>; SUPPLEMENTARY_SERIES.len()] =
        Default::default();
    let hits: Vec<Option<NameHit>> = sprites
        .par_iter()
        .map(|sprite| find_character(&sprite.name, names))
        .collect();
    for (sprite, hit) in sprites.iter().zip(hits) {
        let Some(NameHit {
            prefix: found,
            character,
            rest,
        }) = hit
        else {
            continue;
        };
        let target = if survey.chosen_prefix.as_ref() == Some(&found) {
            &mut series
        } else if let Some(rank) = SUPPLEMENTARY_SERIES.iter().position(|s| *s == found)
            && is_supplementary_rest(&rest)
        {
            &mut supplementary[rank]
        } else {
            continue;
        };
        target
            .entry(character.to_ascii_lowercase())
            .or_default()
            .push((sprite.name.as_str(), rest));
    }
    roster
        .characters
        .iter()
        .enumerate()
        .filter_map(|(index, character)| {
            let key = character.resource_name.to_ascii_lowercase();
            let series_sprite = series
                .remove(&key)
                .and_then(|list| best_by_variant(list.into_iter()))
                .or_else(|| {
                    supplementary
                        .iter_mut()
                        .find_map(|list| best_by_variant(list.remove(&key)?.into_iter()))
                })
                .map(str::to_owned);
            let portrait = best_by_variant(character.portraits.iter().map(|&i| {
                let name = portrait_names[i].as_str();
                let rest = name
                    .get(NAME_PREFIX.len() + character.resource_name.len()..)
                    .filter(|_| {
                        name.get(..NAME_PREFIX.len() + character.resource_name.len())
                            .is_some_and(|head| {
                                head.eq_ignore_ascii_case(&format!(
                                    "{NAME_PREFIX}{}",
                                    character.resource_name
                                ))
                            })
                    })
                    .unwrap_or(name)
                    .to_owned();
                (name, rest)
            }))
            .map(str::to_owned);
            let default = series_sprite
                .clone()
                .map(|s| (Method::Series, s))
                .or_else(|| portrait.map(|s| (Method::AlphaCrop, s)));
            match overrides.get(&character.resource_name) {
                Some(manual) => {
                    let sprite = manual
                        .sprite
                        .clone()
                        .or_else(|| default.as_ref().map(|(_, s)| s.clone()))?;
                    Some(Plan {
                        character: index,
                        method: Method::Override,
                        sprite,
                        rect: manual.rect,
                    })
                }
                None => default.map(|(method, sprite)| Plan {
                    character: index,
                    method,
                    sprite,
                    rect: None,
                }),
            }
        })
        .collect()
}

fn cache_file(directory: &Path, resource: &str, key: u64) -> PathBuf {
    directory.join(format!("{}-{key:016x}.png", resource.to_ascii_lowercase()))
}

/// Context needed to locate and decode sprites.
pub struct Sources<'a> {
    pub bundle_root: &'a Path,
    pub bundles: &'a [PathBuf],
    pub fingerprint: &'a str,
    pub portraits: &'a [Portrait],
    pub sprites: &'a [SpriteRef],
}

/// Renders (or reuses) every planned avatar into `directory`. Sprites
/// outside the portrait index are resolved with one extra bundle scan,
/// only when some avatar is not cached yet.
pub fn generate(
    roster: &Roster,
    plans: &[Plan],
    sources: &Sources,
    directory: &Path,
    cancel: &AtomicBool,
) -> Result<BTreeMap<usize, Avatar>> {
    std::fs::create_dir_all(directory)
        .with_context(|| tr!("无法创建 {}", "Cannot create {}", directory.display()))?;
    let located: HashMap<&str, &SpriteRef> = sources
        .sprites
        .iter()
        .rev()
        .map(|s| (s.name.as_str(), s))
        .collect();
    let keyed: Vec<(&Plan, Option<u64>)> = plans
        .iter()
        .map(|plan| {
            let key = located.get(plan.sprite.as_str()).map(|sprite| {
                stable_hash(&[
                    &METHOD_VERSION.to_le_bytes(),
                    &AVATAR_EDGE.to_le_bytes(),
                    sources.fingerprint.as_bytes(),
                    sprite.name.as_bytes(),
                    sprite.source.container.bundle.as_bytes(),
                    &sprite.source.container.offset.to_le_bytes(),
                    &sprite.path_id.to_le_bytes(),
                    &serde_json::to_vec(&plan.rect).unwrap_or_default(),
                ])
            });
            (plan, key)
        })
        .collect();
    let missing: BTreeSet<&str> = keyed
        .iter()
        .filter(|(plan, key)| {
            key.is_some_and(|key| {
                !cache_file(
                    directory,
                    &roster.characters[plan.character].resource_name,
                    key,
                )
                .is_file()
            })
        })
        .map(|(plan, _)| plan.sprite.as_str())
        .collect();
    let mut decodable: HashMap<String, Portrait> = sources
        .portraits
        .iter()
        .filter(|p| missing.contains(p.name.as_str()))
        .map(|p| (p.name.clone(), p.clone()))
        .collect();
    let others: BTreeSet<String> = missing
        .iter()
        .filter(|name| !decodable.contains_key(**name))
        .map(|name| (*name).to_owned())
        .collect();
    if !others.is_empty() {
        let spec = ScanSpec {
            matches: &|name| others.contains(name),
            collect_names: false,
        };
        let output = portrait::index::scan_sprites(
            sources.bundle_root,
            sources.bundles,
            &spec,
            cancel,
            &|_, _| {},
        );
        anyhow::ensure!(!output.cancelled, tr!("已取消", "Cancelled"));
        for found in output.portraits {
            decodable.entry(found.name.clone()).or_insert(found);
        }
    }
    // Automatic crop boxes of cached avatars, by PNG file stem.
    let rects_path = directory.join(RECTS_FILE);
    let known_rects: BTreeMap<String, [u32; 4]> = std::fs::read(&rects_path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default();
    let results: Vec<(usize, Avatar)> = keyed
        .par_iter()
        .map(|(plan, key)| {
            let resource = &roster.characters[plan.character].resource_name;
            let mut avatar = Avatar {
                method: plan.method,
                sprite: plan.sprite.clone(),
                rect: plan.rect,
                path: None,
                reused: false,
                error: None,
            };
            let Some(key) = key else {
                avatar.error = Some(tr!(
                    "找不到 Sprite {}",
                    "Cannot find sprite {}",
                    plan.sprite
                ));
                return (plan.character, avatar);
            };
            let path = cache_file(directory, resource, *key);
            if path.is_file() {
                avatar.reused = true;
                avatar.rect = avatar
                    .rect
                    .or_else(|| known_rects.get(&stem(&path)).copied());
                avatar.path = Some(path);
                return (plan.character, avatar);
            }
            match render_plan(plan, sources.bundle_root, decodable.get(&plan.sprite)) {
                Ok((image, rect)) => {
                    avatar.rect = rect;
                    match portrait::encode_png(&image)
                        .and_then(|bytes| portrait::write_atomically(&path, &bytes))
                    {
                        Ok(()) => {
                            remove_stale(directory, resource, &path);
                            avatar.path = Some(path);
                        }
                        Err(error) => avatar.error = Some(format!("{error:#}")),
                    }
                }
                Err(error) => avatar.error = Some(format!("{error:#}")),
            }
            (plan.character, avatar)
        })
        .collect();
    let rects: BTreeMap<String, [u32; 4]> = results
        .iter()
        .filter_map(|(_, avatar)| Some((stem(avatar.path.as_ref()?), avatar.rect?)))
        .collect();
    if rects != known_rects {
        // Only a report detail; a failed write just leaves it unknown.
        let _ = serde_json::to_vec_pretty(&rects)
            .map_err(anyhow::Error::from)
            .and_then(|bytes| portrait::write_atomically(&rects_path, &bytes));
    }
    Ok(results.into_iter().collect())
}

const RECTS_FILE: &str = "rects.json";

fn stem(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn render_plan(
    plan: &Plan,
    bundle_root: &Path,
    portrait: Option<&Portrait>,
) -> Result<(RgbaImage, Option<[u32; 4]>)> {
    let portrait = portrait
        .with_context(|| tr!("无法定位 Sprite {}", "Cannot locate sprite {}", plan.sprite))?;
    let image = portrait::decode(bundle_root, portrait)?;
    let rect = match plan.rect {
        Some(rect) => Some(rect),
        None if plan.sprite.starts_with(NAME_PREFIX) => Some(head_box(&image).context(tr!(
            "立绘没有不透明像素",
            "The portrait has no opaque pixels"
        ))?),
        None => None,
    };
    Ok((render(&image, rect)?, rect))
}

/// Removes older cached avatars of the same character.
fn remove_stale(directory: &Path, resource: &str, keep: &Path) {
    let prefix = format!("{}-", resource.to_ascii_lowercase());
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        let stem = name.strip_suffix(".png").unwrap_or_default();
        let is_same = stem
            .strip_prefix(&prefix)
            .is_some_and(|hash| hash.len() == 16 && hash.bytes().all(|b| b.is_ascii_hexdigit()));
        if is_same && path != keep {
            let _ = std::fs::remove_file(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::Character;
    use super::*;
    use image::Rgba;

    /// 100×200 canvas with an opaque body from y 20 to 179; the top rows
    /// lean right so the centroid is not the canvas centre.
    fn figure() -> RgbaImage {
        let mut image = RgbaImage::new(100, 200);
        for y in 20..180 {
            let (from, to) = if y < 30 { (50, 80) } else { (20, 80) };
            for x in from..to {
                image.put_pixel(x, y, Rgba([200, 100, 50, 255]));
            }
        }
        // Faint glow above the head is ignored.
        image.put_pixel(10, 5, Rgba([255, 255, 255, 40]));
        image
    }

    fn sprite(name: &str) -> SpriteRef {
        use crate::portrait::index::{Container, NodeRef};
        SpriteRef {
            name: name.into(),
            source: NodeRef {
                container: Container::default(),
                node: String::new(),
            },
            path_id: 0,
        }
    }

    fn roster_of(resources: &[&str], portrait_of: &[(usize, usize)]) -> Roster {
        let mut characters: Vec<Character> = resources
            .iter()
            .map(|resource| Character {
                resource_name: (*resource).to_owned(),
                name: (*resource).to_owned(),
                aliases: Vec::new(),
                bound_speakers: Vec::new(),
                origins: Vec::new(),
                voice: Vec::new(),
                portraits: Vec::new(),
                live2d: Vec::new(),
                story_lines: Vec::new(),
            })
            .collect();
        for &(character, portrait) in portrait_of {
            characters[character].portraits.push(portrait);
        }
        Roster {
            characters,
            unassigned: Default::default(),
            speakers: Default::default(),
        }
    }

    fn planned(roster: &Roster, portraits: &[&str], sprites: &[&str]) -> Vec<(String, Method)> {
        let names: HashMap<String, String> = roster
            .characters
            .iter()
            .map(|c| {
                (
                    c.resource_name.to_ascii_lowercase(),
                    c.resource_name.clone(),
                )
            })
            .collect();
        let sprites: Vec<SpriteRef> = sprites.iter().map(|s| sprite(s)).collect();
        let portraits: Vec<String> = portraits.iter().map(|s| (*s).to_owned()).collect();
        let survey = Survey {
            chosen_prefix: Some("Avatar_Head_".to_owned()),
            ..Survey::default()
        };
        plan(
            roster,
            &portraits,
            &sprites,
            &names,
            &survey,
            &Overrides::default(),
        )
        .into_iter()
        .map(|plan| (plan.sprite, plan.method))
        .collect()
    }

    #[test]
    fn supplementary_rest_rule() {
        for rest in ["", "_Blur", "SSR", "SR", "SSR01", "SR3", "1", "02"] {
            assert!(is_supplementary_rest(rest), "{rest:?} should qualify");
        }
        for rest in ["Mew", "015Support", "SSRUP", "SR_x", "123", "2a"] {
            assert!(!is_supplementary_rest(rest), "{rest:?} should not qualify");
        }
    }

    #[test]
    fn supplementary_series_fill_in_by_priority() {
        let roster = roster_of(&["Mayling", "Blusphere", "Green", "U", "Tololo"], &[]);
        let plans = planned(
            &roster,
            &[],
            &[
                // Only the exact supplementary prefixes count.
                "Monster_Head_Boss_Mayling",
                "Summon_Head_Mayling",
                "Monster_Head_Mayling_Blur",
                "Avatar_Head_CHA_MaylingSSR01",
                "Avatar_Head_CHA_MaylingSSR",
                "Monster_Head_Blusphere02",
                "Monster_Head_GreenMew",
                "Monster_Head_U015Support",
                "Monster_Head_Tololo",
                "Avatar_Head_TololoSSR",
            ],
        );
        let sprites: Vec<&str> = plans.iter().map(|(s, _)| s.as_str()).collect();
        // The base variant of the first series wins; `GreenMew` and
        // `U015Support` give no avatar; the chosen series beats the rest.
        assert_eq!(
            sprites,
            [
                "Avatar_Head_CHA_MaylingSSR",
                "Monster_Head_Blusphere02",
                "Avatar_Head_TololoSSR"
            ]
        );
        assert!(plans.iter().all(|(_, method)| *method == Method::Series));
    }

    #[test]
    fn series_beat_the_portrait_crop_which_remains_the_fallback() {
        let roster = roster_of(&["Mayling", "Koreda"], &[(0, 0), (1, 1)]);
        let plans = planned(
            &roster,
            &["Avatar_Whole_MaylingSSR", "Avatar_Whole_KoredaSSR01"],
            &["Summon_Head_Mayling"],
        );
        assert_eq!(
            plans,
            [
                ("Summon_Head_Mayling".to_owned(), Method::Series),
                ("Avatar_Whole_KoredaSSR01".to_owned(), Method::AlphaCrop),
            ]
        );
    }

    #[test]
    fn head_box_follows_the_opaque_top() {
        let image = figure();
        // Height 160 → side round(28.8) = 29; band rows 20..39 (19.2 → 19):
        // rows 20..29 span 50..79, rows 30..38 span 20..79.
        let [x, y, w, h] = head_box(&image).unwrap();
        assert_eq!((y, w, h), (20, 29, 29));
        let sum: u64 = (20..30).map(|_| (50..80).sum::<u64>()).sum::<u64>()
            + (30..39).map(|_| (20..80).sum::<u64>()).sum::<u64>();
        let count = 10 * 30 + 9 * 60;
        let centre = (sum as f64 / count as f64).round() as u32;
        assert_eq!(x, centre - 29 / 2);
        assert!(head_box(&RgbaImage::new(4, 4)).is_none());
    }

    #[test]
    fn head_box_is_clamped_to_the_image() {
        let mut image = RgbaImage::new(40, 40);
        for y in 0..40 {
            image.put_pixel(39, y, Rgba([0, 0, 0, 255]));
        }
        let [x, y, w, h] = head_box(&image).unwrap();
        assert_eq!((w, h), (7, 7));
        assert_eq!((x + w, y), (40, 0));
    }

    #[test]
    fn render_fits_any_box_into_a_transparent_square() {
        let image = figure();
        let square = render(&image, Some([20, 20, 60, 60])).unwrap();
        assert_eq!(square.dimensions(), (AVATAR_EDGE, AVATAR_EDGE));
        assert_eq!(square.get_pixel(96, 96)[3], 255);
        // Whole 100×200 sprite: width scales to 96, padded left and right.
        let whole = render(&image, None).unwrap();
        assert_eq!(whole.get_pixel(5, 96)[3], 0);
        assert_eq!(whole.get_pixel(96, 96)[3], 255);
        // Boxes past the edge are clamped; an empty box is an error.
        assert!(render(&image, Some([90, 190, 50, 50])).is_ok());
        assert!(render(&image, Some([10, 10, 0, 5])).is_err());
    }

    #[test]
    fn overrides_are_case_insensitive_and_tolerate_bad_files() {
        let directory = std::env::temp_dir().join(format!(
            "echo-archive-avatar-overrides-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("avatar-overrides.json");
        assert_eq!(Overrides::load(&path), (Overrides::default(), None));
        std::fs::write(
            &path,
            r#"{"characters":{"Charolic":{"rect":[1,2,3,4]},"Tololo":{"sprite":"Avatar_Head_TololoSSR01"}}}"#,
        )
        .unwrap();
        let (overrides, warning) = Overrides::load(&path);
        assert_eq!(warning, None);
        assert_eq!(overrides.get("charolic").unwrap().rect, Some([1, 2, 3, 4]));
        assert_eq!(
            overrides.get("TOLOLO").unwrap().sprite.as_deref(),
            Some("Avatar_Head_TololoSSR01")
        );
        std::fs::write(&path, b"{not json").unwrap();
        let (overrides, warning) = Overrides::load(&path);
        assert!(overrides.characters.is_empty());
        assert!(warning.is_some());
        std::fs::remove_dir_all(directory).unwrap();
    }
}

/// Local check of the automatic crop on real portraits; writes PNGs to
/// `ECHO_AVATAR_PROBE_DIR`. Skipped without the game directory.
#[cfg(test)]
mod local {
    use super::*;

    #[test]
    #[ignore = "needs GF2_GAME_DIR and ECHO_AVATAR_PROBE_DIR"]
    fn alpha_crop_on_installed_game() {
        let Some(game) = crate::paths::test_game_dir() else {
            return;
        };
        let game = game.as_path();
        let Some(output) = std::env::var_os("ECHO_AVATAR_PROBE_DIR").map(PathBuf::from) else {
            return;
        };
        let index = portrait::load_or_build(game, true, &AtomicBool::new(false), &|_| {})
            .unwrap()
            .unwrap();
        std::fs::create_dir_all(&output).unwrap();
        for name in [
            "Avatar_Whole_CharolicSR",
            "Avatar_Whole_TololoSSR",
            "Avatar_Whole_GrozaSR",
            "Avatar_Whole_LennaSSR",
            "Avatar_Whole_SuomiSSR",
            "Avatar_Whole_KoredaSSR01",
        ] {
            let entry = index
                .entries
                .iter()
                .find(|e| e.portrait.name == name)
                .unwrap();
            let image = portrait::decode(&index.bundle_root, &entry.portrait).unwrap();
            let rect = head_box(&image).unwrap();
            println!("{name} {:?} -> {rect:?}", image.dimensions());
            let png = portrait::encode_png(&render(&image, Some(rect)).unwrap()).unwrap();
            std::fs::write(output.join(format!("{name}.png")), png).unwrap();
        }
    }
}
