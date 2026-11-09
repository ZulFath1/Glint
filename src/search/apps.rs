//! The apps provider: finding apps by name as you type (CLAUDE.md section
//! 9, "Search results").
//!
//! Pure Rust, no Windows calls, so it is fully unit-tested.
//!
//! Every app's name is prepared once when the app list loads: a lowercase
//! copy (as a list of characters) and a mark on every character that starts
//! a word. Typing then only compares characters; nothing is allocated per
//! app, so ranking a few hundred apps takes microseconds.
//!
//! Ranking: each match gets a *tier* (lower is better), then a score within
//! the tier (higher is better):
//! 1. prefix: the name starts with the query ("note" → Notepad)
//! 2. word start: a later word starts with the query ("code" → Visual
//!    Studio Code), or the query is the words' first letters ("vsc")
//! 3. fuzzy: the query's letters appear in order ("ps" → Photoshop)
//!
//! Ties go to pinned apps, then to apps launched more often, then to the
//! shorter name, then A–Z.

use std::cmp::Ordering;

use super::{Hit, HitAction, HitIcon, Provider, ProviderId, score as points};
use crate::apps::AppInfo;

/// The apps provider, borrowed for one search: `items[i]` is `apps[i]`
/// prepared for matching.
pub struct AppsProvider<'a> {
    pub apps: &'a [AppInfo],
    pub items: &'a [Item],
}

impl Provider for AppsProvider<'_> {
    fn id(&self) -> ProviderId {
        ProviderId::Apps
    }

    fn query(&self, q: &super::Query, out: &mut Vec<Hit>) {
        let query = Query::new(q.text);
        for (s, i) in rank_scored(self.items, &query, super::MAX_ROWS) {
            let (Some(app), Some(item)) = (self.apps.get(i), self.items.get(i)) else {
                continue;
            };
            let score = if item.chars[..] == query.chars[..] {
                points::APP_EXACT
            } else {
                match s.tier {
                    0 => points::APP_PREFIX,
                    1 => points::APP_WORD,
                    _ => points::APP_FUZZY,
                }
            };
            out.push(app_hit(app, item.pinned, score));
        }
    }
}

/// The result row for `app`.
pub fn app_hit(app: &AppInfo, pinned: bool, score: u16) -> Hit {
    Hit {
        provider: ProviderId::Apps,
        title: app.name.clone(),
        subtitle: if pinned { "Pinned app" } else { "App" }.to_string(),
        icon: HitIcon::App(app.id.clone()),
        action: HitAction::Launch(app.id.clone()),
        score,
        confirm: None,
    }
}

/// One app, prepared for searching.
#[derive(Debug, Clone)]
pub struct Item {
    /// The name, lowercased, one entry per character.
    chars: Box<[char]>,
    /// `starts[i]` = character `i` begins a word.
    starts: Box<[bool]>,
    pub pinned: bool,
    pub launches: u32,
}

impl Item {
    pub fn new(name: &str, pinned: bool, launches: u32) -> Self {
        let mut chars = Vec::with_capacity(name.len());
        let mut starts = Vec::with_capacity(name.len());
        let mut prev: Option<char> = None;
        for c in name.chars() {
            // A word starts after a space or symbol, at a digit after a
            // letter, and at a capital inside a word ("PowerShell").
            let start = match prev {
                None => true,
                Some(p) => {
                    (!p.is_alphanumeric() && c.is_alphanumeric())
                        || (p.is_lowercase() && c.is_uppercase())
                        || (p.is_alphabetic() && c.is_ascii_digit())
                }
            };
            // Some characters lowercase to more than one (e.g. 'İ').
            for (k, lower) in c.to_lowercase().enumerate() {
                chars.push(lower);
                starts.push(start && k == 0);
            }
            prev = Some(c);
        }
        Self {
            chars: chars.into(),
            starts: starts.into(),
            pinned,
            launches,
        }
    }

    fn len(&self) -> usize {
        self.chars.len()
    }
}

/// A query, prepared once per keystroke.
#[derive(Debug, Clone, Default)]
pub struct Query {
    /// Lowercase, trimmed, with runs of spaces collapsed.
    chars: Vec<char>,
    /// The same without spaces (for initials and fuzzy matching).
    letters: Vec<char>,
}

impl Query {
    pub fn new(text: &str) -> Self {
        let mut chars = Vec::new();
        for word in text.split_whitespace() {
            if !chars.is_empty() {
                chars.push(' ');
            }
            chars.extend(word.chars().flat_map(char::to_lowercase));
        }
        let letters = chars.iter().copied().filter(|c| *c != ' ').collect();
        Self { chars, letters }
    }

    pub fn is_empty(&self) -> bool {
        self.chars.is_empty()
    }
}

/// How well an app matches. Compared with `better`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Score {
    /// 0 = prefix, 1 = word start, 2 = fuzzy.
    pub tier: u8,
    /// Higher is better (only compared within the same tier).
    pub points: i32,
}

/// Scores one item. `None` = no match.
pub fn score(item: &Item, q: &Query) -> Option<Score> {
    if q.is_empty() {
        return None;
    }
    let name = &item.chars;
    if name.starts_with(&q.chars) {
        return Some(Score { tier: 0, points: 0 });
    }
    // A later word starts with the query. Earlier words count a bit more.
    if let Some(i) = (1..name.len()).find(|&i| item.starts[i] && name[i..].starts_with(&q.chars)) {
        return Some(Score {
            tier: 1,
            points: -(i as i32),
        });
    }
    if q.letters.len() >= 2 && initials_match(item, &q.letters) {
        return Some(Score {
            tier: 1,
            points: -1000,
        });
    }
    fuzzy(item, &q.letters).map(|points| Score { tier: 2, points })
}

/// The query letters are the first letters of words, in order (words may be
/// skipped): "vsc" → "Visual Studio Code", "ps" → "PowerShell".
fn initials_match(item: &Item, letters: &[char]) -> bool {
    let mut want = letters.iter();
    let mut next = want.next();
    for (c, &start) in item.chars.iter().zip(item.starts.iter()) {
        let Some(w) = next else { break };
        if start && c == w {
            next = want.next();
        }
    }
    next.is_none()
}

const FUZZY_WORD_START: i32 = 10;
const FUZZY_NEXT_TO: i32 = 6;
const FUZZY_GAP: i32 = 1;

/// The query letters appear in order, the first one at the start of a word.
/// For each later letter we prefer the next word start (if the rest of the
/// query still fits after it), otherwise the nearest occurrence. Points:
/// word starts and neighbouring letters earn points, skipped characters
/// cost a little.
fn fuzzy(item: &Item, letters: &[char]) -> Option<i32> {
    let name = &item.chars;
    let mut points = 0;
    let mut pos = 0; // next character we may use
    let mut last: Option<usize> = None;
    for (k, &want) in letters.iter().enumerate() {
        if k == 0 {
            // The first letter must start a word ("ps" → Photoshop, but
            // "note" does not match "leNOvo vanTagE": too random).
            let at = (0..name.len())
                .find(|&i| name[i] == want && item.starts[i] && fits(name, &letters[1..], i + 1))?;
            points += FUZZY_WORD_START - FUZZY_GAP * at as i32;
            last = Some(at);
            pos = at + 1;
            continue;
        }
        let nearest = (pos..name.len()).find(|&i| name[i] == want)?;
        let next_to_last = last.is_some_and(|l| nearest == l + 1);
        // Right next to the previous letter, or at a word start: take it.
        // Otherwise a word start further on is better, if the rest of the
        // query still fits after it.
        let at = if next_to_last || item.starts[nearest] {
            nearest
        } else {
            (nearest + 1..name.len())
                .find(|&i| {
                    name[i] == want && item.starts[i] && fits(name, &letters[k + 1..], i + 1)
                })
                .unwrap_or(nearest)
        };
        if item.starts[at] {
            points += FUZZY_WORD_START;
        }
        match last {
            Some(l) if at == l + 1 => points += FUZZY_NEXT_TO,
            Some(l) => points -= FUZZY_GAP * (at - l - 1) as i32,
            None => points -= FUZZY_GAP * at as i32,
        }
        last = Some(at);
        pos = at + 1;
    }
    Some(points)
}

/// `letters` still appear in order in `name[from..]`.
fn fits(name: &[char], letters: &[char], from: usize) -> bool {
    let mut it = name.get(from..).unwrap_or(&[]).iter();
    letters.iter().all(|w| it.any(|c| c == w))
}

/// Is `a` a better result than `b`? (Ordering::Less = `a` goes first.)
fn order(a: (Score, &Item), b: (Score, &Item)) -> Ordering {
    let (sa, ia) = a;
    let (sb, ib) = b;
    sa.tier
        .cmp(&sb.tier)
        .then(sb.points.cmp(&sa.points))
        .then(ib.pinned.cmp(&ia.pinned))
        .then(ib.launches.cmp(&ia.launches))
        .then(ia.len().cmp(&ib.len()))
        .then_with(|| ia.chars.cmp(&ib.chars))
}

/// The best `limit` matches for `q`, best first, as indexes into `items`.
#[cfg(test)]
pub fn rank(items: &[Item], q: &Query, limit: usize) -> Vec<usize> {
    rank_scored(items, q, limit)
        .into_iter()
        .map(|(_, i)| i)
        .collect()
}

/// The same, with each match's score.
pub fn rank_scored(items: &[Item], q: &Query, limit: usize) -> Vec<(Score, usize)> {
    let mut hits: Vec<(Score, usize)> = items
        .iter()
        .enumerate()
        .filter_map(|(i, item)| score(item, q).map(|s| (s, i)))
        .collect();
    let cmp =
        |a: &(Score, usize), b: &(Score, usize)| order((a.0, &items[a.1]), (b.0, &items[b.1]));
    // Only the top few need to be in order.
    if hits.len() > limit && limit > 0 {
        hits.select_nth_unstable_by(limit - 1, cmp);
        hits.truncate(limit);
    }
    hits.sort_unstable_by(cmp);
    hits.truncate(limit);
    hits
}

/// Moves a highlight up (-1) or down (+1) through `len` rows, wrapping
/// around at both ends.
pub fn step(selected: usize, len: usize, delta: isize) -> usize {
    if len == 0 {
        return 0;
    }
    (selected as isize + delta).rem_euclid(len as isize) as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    fn items(names: &[&str]) -> Vec<Item> {
        names.iter().map(|n| Item::new(n, false, 0)).collect()
    }

    fn names<'a>(all: &[&'a str], items: &[Item], q: &str) -> Vec<&'a str> {
        rank(items, &Query::new(q), 8)
            .into_iter()
            .map(|i| all[i])
            .collect()
    }

    #[test]
    fn prefix_beats_word_start_beats_fuzzy() {
        let all = [
            "Snipping Tool",
            "Notepad",
            "OneNote",
            "Sticky Notes",
            "Microsoft Network Tools",
        ];
        let it = items(&all);
        // Notepad (prefix), Sticky Notes (word start), OneNote (camel case
        // "One|Note"), then fuzzy ones.
        let got = names(&all, &it, "note");
        assert_eq!(got[0], "Notepad");
        assert!(got.contains(&"Sticky Notes"));
        assert!(got.contains(&"OneNote"));
        assert_eq!(got.len(), 3, "{got:?}");
    }

    #[test]
    fn calc_and_case_does_not_matter() {
        let all = ["Calculator", "Calendar", "Clock", "Character Map"];
        let it = items(&all);
        assert_eq!(names(&all, &it, "calc"), ["Calculator"]);
        assert_eq!(names(&all, &it, "CALC"), ["Calculator"]);
        assert_eq!(names(&all, &it, "  cal  "), ["Calendar", "Calculator"]);
    }

    #[test]
    fn initials_and_fuzzy() {
        let all = [
            "Adobe Photoshop 2025",
            "Windows PowerShell",
            "Visual Studio Code",
            "Paint",
        ];
        let it = items(&all);
        // "ps": PowerShell by initials (tier 1), Photoshop by fuzzy (tier 2).
        let got = names(&all, &it, "ps");
        assert_eq!(got, ["Windows PowerShell", "Adobe Photoshop 2025"]);
        assert_eq!(names(&all, &it, "vsc"), ["Visual Studio Code"]);
        assert_eq!(names(&all, &it, "code"), ["Visual Studio Code"]);
        assert_eq!(names(&all, &it, "photo")[0], "Adobe Photoshop 2025");
    }

    #[test]
    fn nothing_matches() {
        let all = ["Notepad", "Paint"];
        let it = items(&all);
        assert!(names(&all, &it, "zzzq").is_empty());
        assert!(names(&all, &it, "").is_empty());
        assert!(names(&all, &it, "   ").is_empty());
    }

    #[test]
    fn ties_go_to_pinned_then_launches_then_shorter() {
        let mut it = items(&["Terminal Preview", "Terminal", "Terminal Canary"]);
        let all = ["Terminal Preview", "Terminal", "Terminal Canary"];
        // Same tier and points: the shorter name wins.
        assert_eq!(names(&all, &it, "term")[0], "Terminal");
        // Launch counts beat length.
        it[2].launches = 3;
        assert_eq!(names(&all, &it, "term")[0], "Terminal Canary");
        // Pinned beats launch counts.
        it[0].pinned = true;
        assert_eq!(
            names(&all, &it, "term"),
            ["Terminal Preview", "Terminal Canary", "Terminal"]
        );
    }

    #[test]
    fn fuzzy_prefers_word_starts_and_neighbours() {
        let all = ["Group Policy Editor", "Gpedit Helper"];
        let it = items(&all);
        // Both are matches for "gp". "Gpedit" has the letters side by side
        // at the start: it's a prefix, so it wins.
        assert_eq!(names(&all, &it, "gp")[0], "Gpedit Helper");
        let a = score(&Item::new("Xbox Game Bar", false, 0), &Query::new("xgb"));
        assert_eq!(a.map(|s| s.tier), Some(1));
        let b = score(&Item::new("Some Long Thing", false, 0), &Query::new("sth"));
        assert_eq!(b.map(|s| s.tier), Some(2));
    }

    #[test]
    fn fuzzy_must_start_at_a_word() {
        let q = Query::new("note");
        // n-o-t-e are all there, but "n" starts no word: too random.
        assert_eq!(score(&Item::new("Lenovo Vantage", false, 0), &q), None);
        assert_eq!(
            score(&Item::new("Defragment and Optimize Drives", false, 0), &q),
            None
        );
        // "Node.js website": N starts a word, then o, t, e follow.
        let node = score(&Item::new("Node.js website", false, 0), &q);
        assert_eq!(node.map(|s| s.tier), Some(2));
        // The first letter may be at a later word ("ps" → "Adobe
        // Photoshop"): fuzzy, tier 2.
        let ps = score(&Item::new("Adobe Photoshop", false, 0), &Query::new("ps"));
        assert_eq!(ps.map(|s| s.tier), Some(2));
    }

    #[test]
    fn query_spaces_are_kept_for_prefix() {
        let all = ["Visual Studio Code", "Visual Studio 2022"];
        let it = items(&all);
        assert_eq!(names(&all, &it, "visual  studio c"), ["Visual Studio Code"]);
    }

    #[test]
    fn limit_keeps_the_best() {
        let all: Vec<String> = (0..50).map(|i| format!("App {i:02}")).collect();
        let refs: Vec<&str> = all.iter().map(String::as_str).collect();
        let it = items(&refs);
        let got = names(&refs, &it, "app");
        assert_eq!(got.len(), 8);
        // All are prefix matches of equal length: A–Z.
        assert_eq!(got[0], "App 00");
        assert_eq!(got[7], "App 07");
    }

    #[test]
    fn selection_wraps_around() {
        assert_eq!(step(0, 5, -1), 4);
        assert_eq!(step(4, 5, 1), 0);
        assert_eq!(step(2, 5, 1), 3);
        assert_eq!(step(0, 0, 1), 0);
        assert_eq!(step(0, 1, -1), 0);
    }

    #[test]
    fn two_thousand_apps_rank_quickly() {
        let words = [
            "Microsoft",
            "Visual",
            "Studio",
            "Code",
            "Edge",
            "Paint",
            "Note",
        ];
        let all: Vec<String> = (0..2000)
            .map(|i| {
                format!(
                    "{} {} {}",
                    words[i % 7],
                    words[(i / 7) % 7],
                    words[(i / 49) % 7]
                )
            })
            .collect();
        let it: Vec<Item> = all.iter().map(|n| Item::new(n, false, 0)).collect();
        let start = std::time::Instant::now();
        for q in ["m", "mi", "vsc", "note", "zzz", "e"] {
            let _ = rank(&it, &Query::new(q), 8);
        }
        let per_query = start.elapsed() / 6;
        // Unoptimized test builds are much slower than release; still
        // well under a frame.
        assert!(per_query.as_millis() < 8, "{per_query:?} per query");
    }
}
