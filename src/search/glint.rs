//! Glint's own commands (SUPERSEARCH.md 3, "glint"): open its settings,
//! reload config.toml, quit, and switch the theme ("theme nord").

use super::{GlintCommand, Hit, HitAction, HitIcon, Provider, ProviderId, Query, keywords};
use crate::theme::ThemeId;

const MAX_HITS: usize = 5;

struct Entry {
    title: &'static str,
    subtitle: &'static str,
    keywords: &'static [&'static str],
    glyph: char,
    command: GlintCommand,
}

const ENTRIES: &[Entry] = &[
    Entry {
        title: "Glint settings",
        subtitle: "Open Glint's Settings window",
        keywords: &["glint settings", "glint preferences", "glint options"],
        glyph: '\u{E713}',
        command: GlintCommand::OpenSettings,
    },
    Entry {
        title: "Reload Glint config",
        subtitle: "Read config.toml again",
        keywords: &["glint reload", "reload config", "reload glint", "reload"],
        glyph: '\u{E72C}',
        command: GlintCommand::ReloadConfig,
    },
    Entry {
        title: "Quit Glint",
        subtitle: "Close Glint (start it again from the Start menu)",
        keywords: &["glint quit", "quit glint", "exit glint", "glint exit"],
        glyph: '\u{E711}',
        command: GlintCommand::Quit,
    },
];

const THEME_GLYPH: char = '\u{E790}';

pub struct GlintProvider;

impl Provider for GlintProvider {
    fn id(&self) -> ProviderId {
        ProviderId::Glint
    }

    fn query(&self, q: &Query, out: &mut Vec<Hit>) {
        let mut hits: Vec<(u16, Hit)> = keywords::best(ENTRIES, &q.lower, MAX_HITS, |e| e.keywords)
            .into_iter()
            .map(|(score, e)| {
                (
                    score,
                    hit(e.title.into(), e.subtitle, e.glyph, e.command, score),
                )
            })
            .collect();
        for theme in ThemeId::ALL {
            // "theme nord", "nord theme", "theme tokyo night"; the bare name
            // only as a word match, so it never beats an app.
            let name = theme.name().replace('-', " ");
            let words = [format!("theme {name}"), format!("{name} theme")];
            let score = keywords::score(&q.lower, &words);
            if let Some(score) = score {
                let title = format!("Theme: {}", theme.title());
                hits.push((
                    score,
                    hit(
                        title,
                        "Switch Glint's look",
                        THEME_GLYPH,
                        GlintCommand::Theme(theme),
                        score,
                    ),
                ));
            }
        }
        // Best first; equal scores keep the order above.
        hits.sort_by_key(|h| std::cmp::Reverse(h.0));
        out.extend(hits.into_iter().take(MAX_HITS).map(|(_, h)| h));
    }
}

fn hit(title: String, subtitle: &str, glyph: char, command: GlintCommand, score: u16) -> Hit {
    Hit {
        provider: ProviderId::Glint,
        title,
        subtitle: subtitle.to_string(),
        icon: HitIcon::Glyph(glyph),
        action: HitAction::Glint(command),
        score,
        confirm: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn find(s: &str) -> Vec<Hit> {
        let mut out = Vec::new();
        GlintProvider.query(&Query::new(s, false), &mut out);
        out
    }

    #[test]
    fn commands() {
        assert_eq!(
            find("glint settings")[0].action,
            HitAction::Glint(GlintCommand::OpenSettings)
        );
        assert_eq!(
            find("glint reload")[0].action,
            HitAction::Glint(GlintCommand::ReloadConfig)
        );
        assert_eq!(
            find("quit glint")[0].action,
            HitAction::Glint(GlintCommand::Quit)
        );
        // "glint" lists the commands (and "glint theme").
        assert_eq!(find("glint").len(), 4);
    }

    #[test]
    fn themes() {
        let nord = find("theme nord");
        assert_eq!(
            nord[0].action,
            HitAction::Glint(GlintCommand::Theme(ThemeId::Nord))
        );
        assert_eq!(nord[0].title, "Theme: Nord");
        assert_eq!(
            find("theme tokyo")[0].action,
            HitAction::Glint(GlintCommand::Theme(ThemeId::TokyoNight))
        );
        // "theme" lists all five.
        assert_eq!(find("theme").len(), ThemeId::ALL.len());
        assert_eq!(
            find("gruvbox theme")[0].action,
            HitAction::Glint(GlintCommand::Theme(ThemeId::Gruvbox))
        );
        assert!(find("theme zzz").is_empty());
    }
}
