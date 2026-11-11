//! Story data derived once from the index, on the worker thread: scripts by
//! group and chapter, every text line for search, per-script line lists, and
//! the text helpers shared by the story views.

use super::theme::FONT_FAMILY;
use crate::story::{StoryIndex, link::Line, sheet_directory};
use gpui::px;
use rayon::prelude::*;
use std::collections::{BTreeMap, HashMap};

pub struct BookInfo {
    pub book: String,
    pub label: String,
    pub directory: String,
    /// First directory under `Assets/Utage/BuildResources/`.
    pub group: String,
    /// The remaining directory path.
    pub chapter: String,
}

/// Who says a line; turned into text when shown, so a language change
/// applies without rebuilding the story data.
pub enum Speaker {
    Named(String),
    Unresolved(String),
    Narrator,
}

impl Speaker {
    /// Speaker as shown: placeholder explained, unresolved IDs named.
    pub fn label(&self) -> String {
        match self {
            Self::Named(name) => display_speaker(name),
            Self::Unresolved(id) => tr!("未解析（Arg1={}）", "Unresolved (Arg1={})", id),
            Self::Narrator => narrator_label().to_owned(),
        }
    }
}

pub struct SearchEntry {
    /// `Arg2`, see [`Line::character_key`].
    pub character_key: Option<String>,
    pub book: usize,
    pub row: i64,
    pub speaker: Speaker,
    /// Resolved speaker name as in the data (roster matching).
    pub name: Option<String>,
    /// Text, followed by ` / <alternate>` when the alternate differs.
    pub text: String,
    /// Length of the default text at the start of `text`.
    pub primary_len: usize,
    /// Resolved CN / JP voices.
    pub cn: bool,
    pub jp: bool,
    haystack: String,
}

impl SearchEntry {
    /// The default text (or the unresolved content ID).
    pub fn primary(&self) -> &str {
        &self.text[..self.primary_len]
    }
}

pub struct StoryData {
    pub index: StoryIndex,
    /// Sorted by group, chapter and label.
    pub books: Vec<BookInfo>,
    pub book_lookup: HashMap<String, usize>,
    pub search: Vec<SearchEntry>,
    /// Search positions of each book's lines, in source order.
    pub book_lines: Vec<Vec<usize>>,
}

impl StoryData {
    pub fn new(mut index: StoryIndex) -> Self {
        let mut directories: BTreeMap<&str, (String, bool)> = BTreeMap::new();
        for grid in &index.grids {
            directories.entry(grid.book.as_str()).or_insert_with(|| {
                (
                    sheet_directory(&grid.sheet),
                    grid.sheet.starts_with(crate::story::SHEET_PREFIX),
                )
            });
        }
        let mut books: Vec<BookInfo> = directories
            .into_iter()
            .map(|(book, (directory, under_prefix))| {
                let (group, chapter) = tree_position(&directory, under_prefix);
                BookInfo {
                    book: book.to_owned(),
                    label: book.strip_suffix(".book").unwrap_or(book).to_owned(),
                    directory,
                    group,
                    chapter,
                }
            })
            .collect();
        books.sort_by(|a, b| {
            (group_rank(&a.group), &a.group, &a.chapter, &a.label).cmp(&(
                group_rank(&b.group),
                &b.group,
                &b.chapter,
                &b.label,
            ))
        });
        let book_lookup: HashMap<String, usize> = books
            .iter()
            .enumerate()
            .map(|(position, info)| (info.book.clone(), position))
            .collect();
        let linked = std::mem::take(&mut index.linked);
        let search: Vec<SearchEntry> = linked
            .lines
            .into_par_iter()
            .filter_map(|line| {
                let &book = book_lookup.get(&line.identity.script)?;
                let speaker = speaker_of(&line);
                let character_key = line.character_key();
                let voiced = |language: &str| {
                    line.voices
                        .iter()
                        .any(|voice| voice.language == language && voice.cue.is_some())
                };
                let (cn, jp) = (voiced("CN"), voiced("JP"));
                let (mut text, _) = text_label(line.text.as_deref(), &line.content_id);
                let primary_len = text.len();
                if let Some(alternate) = line.text_alt.filter(|alt| *alt != text) {
                    text.push_str(" / ");
                    text.push_str(&alternate);
                }
                Some(SearchEntry {
                    book,
                    row: line.identity.row,
                    haystack: format!("{}\n{text}", line.speaker.as_deref().unwrap_or_default())
                        .to_lowercase(),
                    speaker,
                    name: line.speaker,
                    character_key,
                    text,
                    primary_len,
                    cn,
                    jp,
                })
            })
            .collect();
        let mut book_lines = vec![Vec::new(); books.len()];
        for (position, entry) in search.iter().enumerate() {
            book_lines[entry.book].push(position);
        }
        Self {
            index,
            books,
            book_lookup,
            search,
            book_lines,
        }
    }

    /// Up to `limit` matches of a case-insensitive query over speaker and text.
    pub fn find(&self, query: &str, limit: usize) -> Vec<usize> {
        let needle = query.trim().to_lowercase();
        if needle.is_empty() {
            return Vec::new();
        }
        // Labels are matched in the current language, once per search.
        let narrator = narrator_label().to_lowercase().contains(&needle);
        let commander = commander_label().to_lowercase().contains(&needle);
        self.search
            .iter()
            .enumerate()
            .filter(|(_, entry)| {
                entry.haystack.contains(&needle)
                    || match &entry.speaker {
                        Speaker::Narrator => narrator,
                        Speaker::Named(name) => is_player_placeholder(name) && commander,
                        Speaker::Unresolved(_) => {
                            entry.speaker.label().to_lowercase().contains(&needle)
                        }
                    }
            })
            .map(|(position, _)| position)
            .take(limit)
            .collect()
    }
}

/// Reading order of story groups: main story, activities, then the rest by
/// name. `NewAVG_MainStory` and `NewAVG_Activity` belong with their kind.
fn group_rank(group: &str) -> u8 {
    if group.ends_with("MainStory") {
        0
    } else if group.ends_with("Activity") {
        1
    } else if group == OTHER_GROUP {
        3
    } else {
        2
    }
}

/// A line as shown with line breaks: `|` is the story engine's segment mark.
pub fn multi_line_text(text: &str) -> String {
    text.replace('|', "\n")
}

/// A line as shown on one row.
pub fn single_line_text(text: &str) -> String {
    text.replace('|', " ")
}

/// Group of scripts outside a `<group>/<chapter>` directory, and the chapter
/// of scripts directly in the root. Language-independent keys: see
/// [`tree_label`] for what is shown.
pub const OTHER_GROUP: &str = "<other>";
const ROOT_CHAPTER: &str = "<root>";

/// A group or chapter of the script tree as shown.
pub fn tree_label(name: &str) -> String {
    match name {
        OTHER_GROUP => tr!("其他", "Other").to_owned(),
        ROOT_CHAPTER => tr!("根目录", "Root").to_owned(),
        _ => name.to_owned(),
    }
}

/// Places a script directory in the tree: (group, chapter).
fn tree_position(directory: &str, under_prefix: bool) -> (String, String) {
    match directory.split_once('/') {
        Some((group, chapter)) if under_prefix => (group.to_owned(), chapter.to_owned()),
        _ if directory.is_empty() => (OTHER_GROUP.to_owned(), ROOT_CHAPTER.to_owned()),
        _ => (OTHER_GROUP.to_owned(), directory.to_owned()),
    }
}

/// Punctuation that must not start a wrapped line.
const NO_LINE_START: &str = "。，、；：？！）」』》〉】〕…—”’·～.,;:?!)]}%％";

/// Inserts explicit line breaks so wrapped CJK text never starts a line with
/// closing punctuation; gpui's wrapper may break before any CJK character.
/// Only the displayed string changes.
pub fn wrap_for_display(text: &str, width: f32, font_size: f32, cx: &gpui::App) -> String {
    if width <= 20. {
        return text.to_owned();
    }
    let mut wrapper = cx
        .text_system()
        .line_wrapper(gpui::font(FONT_FAMILY), px(font_size));
    let mut output = String::with_capacity(text.len() + 8);
    for (index, paragraph) in text.split('\n').enumerate() {
        if index > 0 {
            output.push('\n');
        }
        let mut rest = paragraph;
        while !rest.is_empty() {
            let boundary = wrapper
                .wrap_line(
                    &[gpui::LineFragment::text(rest)],
                    px(width - 2.),
                    gpui::IndentAdjustment::NoIndent,
                )
                .next()
                .map(|boundary| boundary.ix);
            let Some(mut split) = boundary.filter(|ix| *ix > 0 && *ix < rest.len()) else {
                output.push_str(rest);
                break;
            };
            // Pull up to two characters down with the punctuation.
            for _ in 0..2 {
                let starts_badly = rest[split..]
                    .chars()
                    .next()
                    .is_some_and(|c| NO_LINE_START.contains(c));
                let previous = rest[..split].char_indices().next_back().map(|(ix, _)| ix);
                match previous {
                    Some(ix) if starts_badly && ix > 0 => split = ix,
                    _ => break,
                }
            }
            output.push_str(&rest[..split]);
            output.push('\n');
            rest = &rest[split..];
        }
    }
    output
}

fn is_player_placeholder(name: &str) -> bool {
    name == "###" || name == "{}"
}

fn commander_label() -> &'static str {
    tr!("指挥官", "Commander")
}

pub fn narrator_label() -> &'static str {
    tr!("旁白", "Narrator")
}

/// The player-name placeholders are kept in data; the UI shows the role.
pub fn display_speaker(name: &str) -> String {
    if is_player_placeholder(name) {
        commander_label().to_owned()
    } else {
        name.to_owned()
    }
}

fn speaker_of(line: &Line) -> Speaker {
    match (&line.speaker, &line.speaker_id) {
        (Some(name), _) => Speaker::Named(name.clone()),
        (None, Some(id)) => Speaker::Unresolved(id.to_string()),
        (None, None) => Speaker::Narrator,
    }
}

/// Text shown for a line version; unresolved content names its raw ID.
fn text_label(text: Option<&str>, content_id: &str) -> (String, bool) {
    match text {
        Some(text) => (text.to_owned(), false),
        None if !content_id.is_empty() => (
            tr!(
                "未解析（Text={content_id}）",
                "Unresolved (Text={content_id})",
                content_id = content_id
            ),
            true,
        ),
        None => (String::new(), false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_script_sits_under_a_heading() {
        assert_eq!(
            tree_position("Foreword/ChapterForeword020", true),
            ("Foreword".to_owned(), "ChapterForeword020".to_owned())
        );
        assert_eq!(
            tree_position("Loose", true),
            (OTHER_GROUP.to_owned(), "Loose".to_owned())
        );
        // Sheets outside Assets/Utage/BuildResources/ (e.g. Assets/Utage/Adv.xls).
        assert_eq!(
            tree_position("Assets/Utage", false),
            (OTHER_GROUP.to_owned(), "Assets/Utage".to_owned())
        );
        assert_eq!(tree_position("", true).0, OTHER_GROUP);
    }

    #[test]
    fn groups_read_main_story_first() {
        let mut groups = [
            "Foreword",
            "Activity",
            "NewAVG_MainStory",
            "Ties",
            "MainStory",
            "NewAVG_Activity",
        ];
        groups.sort_by_key(|group| (group_rank(group), *group));
        assert_eq!(
            groups,
            [
                "MainStory",
                "NewAVG_MainStory",
                "Activity",
                "NewAVG_Activity",
                "Foreword",
                "Ties"
            ]
        );
    }

    #[test]
    fn segment_marks_are_only_a_display_concern() {
        assert_eq!(multi_line_text("甲|乙"), "甲\n乙");
        assert_eq!(single_line_text("甲|乙"), "甲 乙");
    }

    #[test]
    fn player_placeholder_is_explained_only_in_the_ui() {
        assert_eq!(display_speaker("###"), "指挥官");
        assert_eq!(display_speaker("{}"), "指挥官");
        assert_eq!(display_speaker("未知对象"), "未知对象");
    }
}
