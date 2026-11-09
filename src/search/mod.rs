//! Super search (SUPERSEARCH.md): one box that finds apps, does math,
//! converts units, runs system actions, opens Settings pages, controls
//! music and Glint itself.
//!
//! Every kind of result comes from a *provider* (`Provider`). For each
//! keystroke:
//! 1. `parse` looks for a provider prefix (`=12*8`, or "<prefix> " typed
//!    before the text). With one, only that provider runs.
//! 2. Otherwise every provider that joins the default search runs.
//! 3. `merge` sorts the hits by score, keeps the best few, and groups them
//!    by provider (with a small header per group when there are several).
//!
//! All of this is plain Rust without Windows calls, so it is unit-tested.
//! The providers here are all "fast" ones: they finish in microseconds on
//! the UI thread, and nothing runs in the background.
//!
//! Safety rule: providers only *describe* actions (`HitAction`). Nothing
//! runs until the user presses Enter, and dangerous actions ask twice.

pub mod apps;
pub mod calc;
pub mod glint;
mod keywords;
pub mod music;
pub mod settings;
pub mod system;
pub mod units;

use crate::media::MediaCommand;
use crate::power::PowerAction;
use crate::theme::ThemeId;

pub use apps::{AppsProvider, Item, app_hit, step};
pub use calc::CalcProvider;
pub use glint::GlintProvider;
pub use music::{MusicProvider, NowPlaying};
pub use settings::SettingsProvider;
pub use system::SystemProvider;
pub use units::UnitsProvider;

/// At most this many result rows.
pub const MAX_ROWS: usize = 10;
/// A section header is half as tall as a row. Rows + headers never take
/// more room than `MAX_ROWS` rows, so the menu never outgrows its window.
const HEADER_COST: usize = 1;
const ROW_COST: usize = 2;

/// Who made a result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProviderId {
    Apps,
    Calc,
    Units,
    System,
    Settings,
    Glint,
    Music,
}

impl ProviderId {
    /// The name in section headers (and, later, in the prefix chip).
    pub fn title(self) -> &'static str {
        match self {
            Self::Apps => "Apps",
            Self::Calc => "Calculator",
            Self::Units => "Units",
            Self::System => "System",
            Self::Settings => "Settings",
            Self::Glint => "Glint",
            Self::Music => "Music",
        }
    }
}

/// The picture on the left of a row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HitIcon {
    /// The app's own icon (by its AppsFolder ID).
    App(String),
    /// A character of the Segoe Fluent Icons font.
    Glyph(char),
}

/// Something Glint itself can do from the search box.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GlintCommand {
    OpenSettings,
    ReloadConfig,
    Quit,
    Theme(ThemeId),
}

/// What Enter does. Only a description: main.rs carries it out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HitAction {
    /// Start the app with this AppsFolder ID.
    Launch(String),
    /// Put this text on the clipboard (calc and unit results).
    Copy(String),
    /// Open a URI or program through the shell (`ms-settings:display`,
    /// `devmgmt.msc`, `control.exe` + `printers`).
    Open {
        target: String,
        args: String,
    },
    Power(PowerAction),
    SignOut,
    EmptyRecycleBin,
    Media(MediaCommand),
    Glint(GlintCommand),
}

impl HitAction {
    /// Opens `target` without arguments.
    pub fn open(target: &str) -> Self {
        Self::Open {
            target: target.to_string(),
            args: String::new(),
        }
    }

    /// The word in the selected row's hint ("Enter  Open").
    pub fn verb(&self) -> &'static str {
        match self {
            Self::Launch(_) | Self::Open { .. } => "Open",
            Self::Copy(_) => "Copy",
            Self::Media(_) => "Play",
            Self::Power(_) | Self::SignOut | Self::EmptyRecycleBin | Self::Glint(_) => "Run",
        }
    }
}

/// One result row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    pub provider: ProviderId,
    pub title: String,
    /// The smaller second line (what it is, the path, the track...).
    pub subtitle: String,
    pub icon: HitIcon,
    pub action: HitAction,
    /// 0–1000, higher is better (SUPERSEARCH.md 2.3).
    pub score: u16,
    /// Dangerous: the first Enter only asks this ("Press Enter again to
    /// restart"), the second one runs it.
    pub confirm: Option<&'static str>,
}

impl Hit {
    /// Stays the same while the row means the same thing (to keep the
    /// selection on it while typing).
    pub fn same_target(&self, other: &Hit) -> bool {
        self.provider == other.provider && self.action == other.action
    }
}

/// Scores, so every provider ranks on the same scale (SUPERSEARCH.md 2.3):
/// calc/units > exact app name > exact keyword > app prefix > keyword
/// prefix > other app matches > keyword word start > fuzzy app.
pub mod score {
    pub const ANSWER: u16 = 1000;
    pub const APP_EXACT: u16 = 950;
    pub const KEYWORD_EXACT: u16 = 900;
    pub const APP_PREFIX: u16 = 800;
    pub const KEYWORD_PREFIX: u16 = 700;
    pub const APP_WORD: u16 = 650;
    pub const KEYWORD_WORD: u16 = 600;
    pub const APP_FUZZY: u16 = 500;
}

/// The search text a provider gets (prefix already removed).
#[derive(Debug, Clone)]
pub struct Query<'a> {
    /// Trimmed.
    pub text: &'a str,
    /// Lowercase, with runs of spaces collapsed to one.
    pub lower: String,
    /// The user typed this provider's prefix (so it should answer even
    /// to things it normally ignores, like a bare number in calc).
    pub scoped: bool,
}

impl<'a> Query<'a> {
    pub fn new(text: &'a str, scoped: bool) -> Self {
        let text = text.trim();
        let lower = text
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase();
        Self {
            text,
            lower,
            scoped,
        }
    }
}

/// A source of results.
pub trait Provider {
    fn id(&self) -> ProviderId;
    /// Typing this first runs only this provider. A word prefix needs a
    /// space after it ("calc 2+2"); a symbol prefix doesn't ("=2+2").
    fn prefix(&self) -> Option<&str> {
        None
    }
    /// Does it join the search when no prefix is typed?
    fn matches_without_prefix(&self) -> bool {
        true
    }
    /// Adds its hits for `q` to `out` (best first, any number).
    fn query(&self, q: &Query, out: &mut Vec<Hit>);
}

/// Which provider (index into `providers`) a prefix picked, and the text
/// after it. The longest matching prefix wins (">>" before ">").
pub fn parse<'a>(input: &'a str, providers: &[&dyn Provider]) -> (Option<usize>, &'a str) {
    let start = input.trim_start();
    let mut best: Option<(usize, usize)> = None; // (provider, prefix length)
    for (i, p) in providers.iter().enumerate() {
        let Some(prefix) = p.prefix().filter(|s| !s.is_empty()) else {
            continue;
        };
        let Some(head) = start.get(..prefix.len()) else {
            continue;
        };
        if !head.eq_ignore_ascii_case(prefix) {
            continue;
        }
        let rest = &start[prefix.len()..];
        let word = prefix.chars().any(char::is_alphanumeric);
        // "calc 2" yes, "calculator" no; "=2" yes.
        if word && !rest.starts_with(' ') {
            continue;
        }
        if best.is_none_or(|(_, len)| prefix.len() > len) {
            best = Some((i, prefix.len()));
        }
    }
    match best {
        Some((i, len)) => (Some(i), &start[len..]),
        None => (None, input),
    }
}

/// What the result list shows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Results {
    /// Best first, grouped by provider.
    pub hits: Vec<Hit>,
    /// `headers[i]` = a section header goes above row `i`.
    pub headers: Vec<bool>,
    /// The provider a prefix picked (only it ran).
    pub scope: Option<ProviderId>,
}

#[cfg(test)]
impl Results {
    pub fn header_count(&self) -> usize {
        self.headers.iter().filter(|&&h| h).count()
    }
}

/// Runs the search for `input` over `providers` (in tie-break order).
pub fn search(input: &str, providers: &[&dyn Provider]) -> Results {
    let (scope, text) = parse(input, providers);
    let q = Query::new(text, scope.is_some());
    let mut all: Vec<(usize, Hit)> = Vec::new();
    let mut out = Vec::new();
    for (i, p) in providers.iter().enumerate() {
        let runs = match scope {
            Some(s) => s == i,
            None => p.matches_without_prefix(),
        };
        if !runs || (q.lower.is_empty() && scope.is_none()) {
            continue;
        }
        out.clear();
        p.query(&q, &mut out);
        all.extend(out.drain(..).map(|h| (i, h)));
    }
    let mut results = merge(all);
    results.scope = scope.map(|i| providers[i].id());
    results
}

/// Best first; ties go to the earlier provider, then to the provider's own
/// order. Keeps what fits in `MAX_ROWS` rows (headers count as half a row),
/// then groups the rows by provider, groups ordered by their best row.
fn merge(mut all: Vec<(usize, Hit)>) -> Results {
    // Stable sort: equal scores keep provider order, then emission order.
    all.sort_by(|a, b| b.1.score.cmp(&a.1.score).then(a.0.cmp(&b.0)));
    // The same thing twice (the "Task Manager" app and the "Task Manager"
    // system action): only the better one stays. (Two apps with the same
    // name both stay: they are different apps.)
    let mut seen: Vec<(usize, String)> = Vec::new();
    all.retain(|(p, h)| {
        let title = h.title.to_lowercase();
        if seen.iter().any(|(q, t)| q != p && *t == title) {
            return false;
        }
        seen.push((*p, title));
        true
    });
    all.truncate(MAX_ROWS);
    // Drop the weakest rows until rows + headers fit.
    loop {
        let groups = group_count(&all);
        let headers = if groups > 1 { groups } else { 0 };
        if all.len() * ROW_COST + headers * HEADER_COST <= MAX_ROWS * ROW_COST {
            break;
        }
        all.pop();
    }
    // Group: providers in the order of their best (first) row.
    let mut order: Vec<usize> = Vec::new();
    for (p, _) in &all {
        if !order.contains(p) {
            order.push(*p);
        }
    }
    let several = order.len() > 1;
    let mut hits = Vec::with_capacity(all.len());
    let mut headers = Vec::with_capacity(all.len());
    for p in order {
        let mut first = true;
        for (_, hit) in all.iter().filter(|(q, _)| *q == p) {
            headers.push(several && first);
            first = false;
            hits.push(hit.clone());
        }
    }
    Results {
        hits,
        headers,
        scope: None,
    }
}

fn group_count(all: &[(usize, Hit)]) -> usize {
    let mut seen: Vec<usize> = Vec::new();
    for (p, _) in all {
        if !seen.contains(p) {
            seen.push(*p);
        }
    }
    seen.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Gives fixed hits for any query.
    struct Fixed {
        id: ProviderId,
        prefix: Option<&'static str>,
        default: bool,
        hits: Vec<(&'static str, u16)>,
    }

    impl Provider for Fixed {
        fn id(&self) -> ProviderId {
            self.id
        }
        fn prefix(&self) -> Option<&str> {
            self.prefix
        }
        fn matches_without_prefix(&self) -> bool {
            self.default
        }
        fn query(&self, _q: &Query, out: &mut Vec<Hit>) {
            for (title, score) in &self.hits {
                out.push(hit(self.id, title, *score));
            }
        }
    }

    fn hit(provider: ProviderId, title: &str, score: u16) -> Hit {
        Hit {
            provider,
            title: title.to_string(),
            subtitle: String::new(),
            icon: HitIcon::Glyph('x'),
            action: HitAction::Copy(title.to_string()),
            score,
            confirm: None,
        }
    }

    fn fixed(id: ProviderId, hits: Vec<(&'static str, u16)>) -> Fixed {
        Fixed {
            id,
            prefix: None,
            default: true,
            hits,
        }
    }

    fn titles(r: &Results) -> Vec<&str> {
        r.hits.iter().map(|h| h.title.as_str()).collect()
    }

    #[test]
    fn prefixes() {
        let calc = Fixed {
            prefix: Some("="),
            ..fixed(ProviderId::Calc, vec![])
        };
        let sys = Fixed {
            prefix: Some("sys"),
            ..fixed(ProviderId::System, vec![])
        };
        let gt = Fixed {
            prefix: Some(">"),
            ..fixed(ProviderId::Glint, vec![])
        };
        let gtgt = Fixed {
            prefix: Some(">>"),
            ..fixed(ProviderId::Music, vec![])
        };
        let all: [&dyn Provider; 4] = [&calc, &sys, &gt, &gtgt];
        assert_eq!(parse("=2+2", &all), (Some(0), "2+2"));
        assert_eq!(parse("  = 2", &all), (Some(0), " 2"));
        assert_eq!(parse("sys lock", &all), (Some(1), " lock"));
        assert_eq!(parse("SYS lock", &all), (Some(1), " lock"));
        // A word prefix needs a space after it.
        assert_eq!(parse("system", &all), (None, "system"));
        assert_eq!(parse("sys", &all), (None, "sys"));
        // The longest prefix wins.
        assert_eq!(parse(">>npm", &all), (Some(3), "npm"));
        assert_eq!(parse(">dir", &all), (Some(2), "dir"));
        assert_eq!(parse("2+2", &all), (None, "2+2"));
        // Not on a char boundary: no match, no panic.
        assert_eq!(parse("é", &all), (None, "é"));
    }

    #[test]
    fn a_prefix_runs_only_its_provider() {
        let apps = fixed(ProviderId::Apps, vec![("App", 800)]);
        let calc = Fixed {
            prefix: Some("="),
            ..fixed(ProviderId::Calc, vec![("4", 1000)])
        };
        let all: [&dyn Provider; 2] = [&apps, &calc];
        let r = search("=2+2", &all);
        assert_eq!(titles(&r), ["4"]);
        assert_eq!(r.scope, Some(ProviderId::Calc));
        let r = search("2+2", &all);
        assert_eq!(titles(&r), ["4", "App"]);
        assert_eq!(r.scope, None);
    }

    #[test]
    fn hidden_providers_only_run_with_their_prefix() {
        let hidden = Fixed {
            prefix: Some("h"),
            default: false,
            ..fixed(ProviderId::Glint, vec![("H", 900)])
        };
        let all: [&dyn Provider; 1] = [&hidden];
        assert!(search("x", &all).hits.is_empty());
        assert_eq!(titles(&search("h x", &all)), ["H"]);
    }

    #[test]
    fn empty_text_finds_nothing() {
        let apps = fixed(ProviderId::Apps, vec![("App", 800)]);
        let all: [&dyn Provider; 1] = [&apps];
        assert!(search("", &all).hits.is_empty());
        assert!(search("   ", &all).hits.is_empty());
    }

    #[test]
    fn best_first_then_grouped_with_headers() {
        let apps = fixed(ProviderId::Apps, vec![("A1", 800), ("A2", 500)]);
        let sys = fixed(ProviderId::System, vec![("S1", 900), ("S2", 600)]);
        let all: [&dyn Provider; 2] = [&apps, &sys];
        let r = search("x", &all);
        // System has the best row, so its group comes first.
        assert_eq!(titles(&r), ["S1", "S2", "A1", "A2"]);
        assert_eq!(r.headers, [true, false, true, false]);
        // One provider: no headers.
        let all: [&dyn Provider; 1] = [&apps];
        let r = search("x", &all);
        assert_eq!(r.headers, [false, false]);
    }

    #[test]
    fn ties_go_to_the_earlier_provider() {
        let apps = fixed(ProviderId::Apps, vec![("A", 700)]);
        let sys = fixed(ProviderId::System, vec![("S", 700)]);
        let all: [&dyn Provider; 2] = [&apps, &sys];
        assert_eq!(titles(&search("x", &all)), ["A", "S"]);
        let all: [&dyn Provider; 2] = [&sys, &apps];
        assert_eq!(titles(&search("x", &all)), ["S", "A"]);
    }

    #[test]
    fn the_same_title_from_two_providers_shows_once() {
        let apps = fixed(
            ProviderId::Apps,
            vec![("Task Manager", 950), ("Tasks", 800)],
        );
        let sys = fixed(ProviderId::System, vec![("task manager", 900)]);
        let all: [&dyn Provider; 2] = [&apps, &sys];
        let r = search("x", &all);
        assert_eq!(titles(&r), ["Task Manager", "Tasks"]);
        // Two apps with one name are two apps.
        let twins = fixed(ProviderId::Apps, vec![("Python", 800), ("Python", 800)]);
        let all: [&dyn Provider; 1] = [&twins];
        assert_eq!(search("x", &all).hits.len(), 2);
    }

    #[test]
    fn rows_and_headers_fit_ten_rows() {
        let many: Vec<(&str, u16)> = vec![("A", 800); 12];
        let apps = fixed(ProviderId::Apps, many);
        let all: [&dyn Provider; 1] = [&apps];
        let r = search("x", &all);
        assert_eq!(r.hits.len(), MAX_ROWS);
        assert_eq!(r.header_count(), 0);
        // Two groups: 2 headers = one row's room, so 9 rows.
        let calc = fixed(ProviderId::Calc, vec![("C", 1000)]);
        let all: [&dyn Provider; 2] = [&apps, &calc];
        let r = search("x", &all);
        assert_eq!(r.hits.len(), 9);
        assert_eq!(r.header_count(), 2);
        assert_eq!(r.hits[0].title, "C");
        assert!(r.hits.len() * 2 + r.header_count() <= MAX_ROWS * 2);
    }
}
