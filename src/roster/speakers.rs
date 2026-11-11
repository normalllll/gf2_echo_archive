//! Story speaker → character matching (exact Chinese names only) and the
//! speaker coverage report.

use serde::Serialize;
use std::collections::{BTreeMap, HashMap};

/// Speakers that are never a character: unknown / masked names and the
/// player-name placeholders (`###`, and `{}` filled in at run time).
pub const NON_CHARACTER_SPEAKERS: [&str; 7] =
    ["？？", "？？？", "??", "???", "未知对象", "###", "{}"];
pub const COMMANDER_SPEAKERS: [&str; 1] = ["指挥官"];
pub const TOP_UNMATCHED: usize = 50;
/// A speaker binds to a resource shown on at least this many of its lines …
pub const BIND_MIN_LINES: usize = 20;
/// … that are also at least this share (percent) of its keyed lines.
pub const BIND_MIN_PERCENT: usize = 70;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpeakerMatch {
    Character(usize),
    NonCharacter,
    Unmatched,
    NoSpeaker,
}

/// Exact-name lookup: Chinese official names and Chinese aliases.
pub struct SpeakerLookup {
    names: HashMap<String, usize>,
}

/// True when a name contains anything besides ASCII (catalogue aliases are
/// mostly English resource spellings that never appear as speakers).
pub fn is_chinese_name(name: &str) -> bool {
    !name.trim().is_empty() && !name.is_ascii()
}

impl SpeakerLookup {
    /// `characters` yields (character index, official name, aliases, bound
    /// speakers). Bound speakers match whatever script they are written in.
    pub fn new<'a>(
        characters: impl IntoIterator<Item = (usize, &'a str, &'a [String], &'a [String])>,
    ) -> Self {
        let mut names = HashMap::new();
        for (index, official, aliases, bound) in characters {
            for name in std::iter::once(official).chain(aliases.iter().map(String::as_str)) {
                if is_chinese_name(name) {
                    names.entry(name.to_owned()).or_insert(index);
                }
            }
            for speaker in bound {
                names.entry(speaker.clone()).or_insert(index);
            }
        }
        Self { names }
    }

    pub fn classify(&self, speaker: Option<&str>) -> SpeakerMatch {
        let Some(speaker) = speaker.filter(|s| !s.is_empty()) else {
            return SpeakerMatch::NoSpeaker;
        };
        if is_non_character(speaker) {
            return SpeakerMatch::NonCharacter;
        }
        match self.names.get(speaker) {
            Some(index) => SpeakerMatch::Character(*index),
            None => SpeakerMatch::Unmatched,
        }
    }
}

pub fn is_non_character(speaker: &str) -> bool {
    NON_CHARACTER_SPEAKERS.contains(&speaker)
        || COMMANDER_SPEAKERS.contains(&speaker)
        || speaker.chars().all(|c| matches!(c, '?' | '？'))
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct NameCount {
    pub name: String,
    pub lines: usize,
}

/// A resource with the speakers bound to it, most lines first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResourceBinding {
    pub resource: String,
    pub speakers: Vec<NameCount>,
}

/// Binds speakers to resources from (speaker, resource) evidence, one pair
/// per story line. Empty and non-character speakers give no evidence. A
/// speaker binds to at most one resource; the share threshold being above
/// one half guarantees it. Resources compare case-insensitively and keep
/// the first spelling in sorted order; the result is sorted by resource.
pub fn bind<'a>(evidence: impl IntoIterator<Item = (&'a str, &'a str)>) -> Vec<ResourceBinding> {
    let mut pairs: BTreeMap<(&str, &str), usize> = BTreeMap::new();
    for (speaker, resource) in evidence {
        if !speaker.is_empty() && !is_non_character(speaker) {
            *pairs.entry((speaker, resource)).or_default() += 1;
        }
    }
    let mut totals: HashMap<&str, usize> = HashMap::new();
    let mut counts: BTreeMap<(String, &str), (&str, usize)> = BTreeMap::new();
    for ((speaker, resource), lines) in pairs {
        *totals.entry(speaker).or_default() += lines;
        counts
            .entry((resource.to_ascii_lowercase(), speaker))
            .or_insert((resource, 0))
            .1 += lines;
    }
    let mut bound: BTreeMap<String, ResourceBinding> = BTreeMap::new();
    for ((key, speaker), (resource, lines)) in counts {
        if lines >= BIND_MIN_LINES && lines * 100 >= totals[speaker] * BIND_MIN_PERCENT {
            bound
                .entry(key)
                .or_insert_with(|| ResourceBinding {
                    resource: resource.to_owned(),
                    speakers: Vec::new(),
                })
                .speakers
                .push(NameCount {
                    name: speaker.to_owned(),
                    lines,
                });
        }
    }
    let mut result: Vec<ResourceBinding> = bound.into_values().collect();
    for binding in &mut result {
        binding
            .speakers
            .sort_by(|a, b| b.lines.cmp(&a.lines).then_with(|| a.name.cmp(&b.name)));
    }
    result
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct SpeakerReport {
    pub lines: usize,
    pub distinct_speakers: usize,
    pub matched_speakers: usize,
    pub matched_lines: usize,
    pub non_character_speakers: usize,
    pub non_character_lines: usize,
    pub unmatched_speakers: usize,
    pub unmatched_lines: usize,
    pub lines_without_speaker: usize,
    /// Unmatched names by line count, most first (top 50).
    pub top_unmatched: Vec<NameCount>,
}

/// Accumulates line classifications into the report.
#[derive(Default)]
pub struct ReportBuilder {
    counts: BTreeMap<String, (SpeakerMatch, usize)>,
    lines: usize,
    without_speaker: usize,
}

impl ReportBuilder {
    pub fn add(&mut self, speaker: Option<&str>, result: SpeakerMatch) {
        self.lines += 1;
        match (speaker, result) {
            (Some(name), SpeakerMatch::Character(_))
            | (Some(name), SpeakerMatch::NonCharacter)
            | (Some(name), SpeakerMatch::Unmatched) => {
                self.counts.entry(name.to_owned()).or_insert((result, 0)).1 += 1;
            }
            _ => self.without_speaker += 1,
        }
    }

    pub fn finish(self) -> SpeakerReport {
        let mut report = SpeakerReport {
            lines: self.lines,
            distinct_speakers: self.counts.len(),
            lines_without_speaker: self.without_speaker,
            ..SpeakerReport::default()
        };
        let mut unmatched = Vec::new();
        for (name, (result, lines)) in self.counts {
            match result {
                SpeakerMatch::Character(_) => {
                    report.matched_speakers += 1;
                    report.matched_lines += lines;
                }
                SpeakerMatch::NonCharacter => {
                    report.non_character_speakers += 1;
                    report.non_character_lines += lines;
                }
                SpeakerMatch::Unmatched => {
                    report.unmatched_speakers += 1;
                    report.unmatched_lines += lines;
                    unmatched.push(NameCount { name, lines });
                }
                SpeakerMatch::NoSpeaker => {}
            }
        }
        unmatched.sort_by(|a, b| b.lines.cmp(&a.lines).then_with(|| a.name.cmp(&b.name)));
        unmatched.truncate(TOP_UNMATCHED);
        report.top_unmatched = unmatched;
        report
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lookup() -> SpeakerLookup {
        let aliases = vec!["Krolik".to_owned(), "克罗".to_owned()];
        SpeakerLookup::new([
            (0, "克罗丽科", aliases.as_slice(), &[][..]),
            (1, "托洛洛", &[][..], &[][..]),
        ])
    }

    /// Evidence with `lines` pairs for each (speaker, resource, lines).
    fn binding(evidence: &[(&'static str, &'static str, usize)]) -> Vec<ResourceBinding> {
        bind(
            evidence
                .iter()
                .flat_map(|&(s, r, lines)| std::iter::repeat_n((s, r), lines)),
        )
    }

    fn bound_pairs(result: &[ResourceBinding]) -> Vec<(String, String, usize)> {
        result
            .iter()
            .flat_map(|b| {
                b.speakers
                    .iter()
                    .map(|s| (b.resource.clone(), s.name.clone(), s.lines))
            })
            .collect()
    }

    #[test]
    fn binding_needs_twenty_lines() {
        assert!(binding(&[("美玲", "Mayling", 19)]).is_empty());
        assert_eq!(
            bound_pairs(&binding(&[("美玲", "Mayling", 20)])),
            [("Mayling".to_owned(), "美玲".to_owned(), 20)]
        );
    }

    #[test]
    fn binding_needs_seventy_percent_of_the_speakers_evidence() {
        // 70 of 100 is enough, 69 of 100 is not.
        assert_eq!(
            bound_pairs(&binding(&[("甲", "A", 70), ("甲", "B", 30)])),
            [("A".to_owned(), "甲".to_owned(), 70)]
        );
        assert!(binding(&[("甲", "A", 69), ("甲", "B", 31)]).is_empty());
        // A split speaker stays unbound, however many lines each side has.
        assert!(binding(&[("朱诺", "ots", 386), ("朱诺", "blackgroza", 316)]).is_empty());
        // Scattered evidence counts against the share.
        assert!(
            binding(&[
                ("甲", "A", 20),
                ("甲", "B", 5),
                ("甲", "C", 5),
                ("甲", "D", 5)
            ])
            .is_empty()
        );
    }

    #[test]
    fn binding_ignores_masked_speakers_and_case() {
        assert!(binding(&[("未知对象", "A", 50), ("？？", "A", 50), ("", "A", 50)]).is_empty());
        let result = binding(&[("甲", "Alf", 15), ("甲", "ALF", 15)]);
        assert_eq!(
            bound_pairs(&result),
            [("ALF".to_owned(), "甲".to_owned(), 30)]
        );
        // One binding per resource, its speakers by evidence.
        let result = binding(&[("乙", "A", 20), ("甲", "A", 40), ("丙", "A", 40)]);
        assert_eq!(result.len(), 1);
        let names: Vec<&str> = result[0].speakers.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["丙", "甲", "乙"]);
    }

    #[test]
    fn bound_speakers_match_in_any_script() {
        let bound = vec!["M200".to_owned()];
        let lookup = SpeakerLookup::new([(0, "夏安", &[][..], bound.as_slice())]);
        assert_eq!(lookup.classify(Some("M200")), SpeakerMatch::Character(0));
        assert_eq!(lookup.classify(Some("m200")), SpeakerMatch::Unmatched);
    }

    #[test]
    fn matches_only_exact_chinese_names() {
        let lookup = lookup();
        assert_eq!(
            lookup.classify(Some("克罗丽科")),
            SpeakerMatch::Character(0)
        );
        assert_eq!(lookup.classify(Some("克罗")), SpeakerMatch::Character(0));
        assert_eq!(lookup.classify(Some("托洛洛")), SpeakerMatch::Character(1));
        // No partial, padded or ASCII-alias matches.
        assert_eq!(lookup.classify(Some("克罗丽")), SpeakerMatch::Unmatched);
        assert_eq!(lookup.classify(Some("托洛洛 ")), SpeakerMatch::Unmatched);
        assert_eq!(
            lookup.classify(Some("托洛洛（通讯）")),
            SpeakerMatch::Unmatched
        );
        assert_eq!(lookup.classify(Some("Krolik")), SpeakerMatch::Unmatched);
        assert_eq!(lookup.classify(Some("krolik")), SpeakerMatch::Unmatched);
    }

    #[test]
    fn non_character_speakers_belong_to_nobody() {
        let lookup = lookup();
        for name in ["？？", "？？？", "???", "未知对象", "###", "{}", "指挥官"] {
            assert_eq!(
                lookup.classify(Some(name)),
                SpeakerMatch::NonCharacter,
                "{name}"
            );
        }
        assert_eq!(lookup.classify(None), SpeakerMatch::NoSpeaker);
        assert_eq!(lookup.classify(Some("")), SpeakerMatch::NoSpeaker);
    }

    #[test]
    fn report_ranks_unmatched_names_by_lines() {
        let lookup = lookup();
        let mut builder = ReportBuilder::default();
        for speaker in [
            Some("克罗丽科"),
            Some("士兵"),
            Some("路人"),
            Some("士兵"),
            Some("？？"),
            None,
        ] {
            builder.add(speaker, lookup.classify(speaker));
        }
        let report = builder.finish();
        assert_eq!(report.lines, 6);
        assert_eq!(report.distinct_speakers, 4);
        assert_eq!((report.matched_speakers, report.matched_lines), (1, 1));
        assert_eq!(
            (report.non_character_speakers, report.non_character_lines),
            (1, 1)
        );
        assert_eq!((report.unmatched_speakers, report.unmatched_lines), (2, 3));
        assert_eq!(report.lines_without_speaker, 1);
        assert_eq!(
            report.top_unmatched,
            [
                NameCount {
                    name: "士兵".into(),
                    lines: 2
                },
                NameCount {
                    name: "路人".into(),
                    lines: 1
                },
            ]
        );
    }
}
