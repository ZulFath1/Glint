//! Music controls by keyword (SUPERSEARCH.md 3, "music"): "play",
//! "pause", "next", "prev". Only while some app plays or has paused
//! music; the row shows the current track.

use super::{Hit, HitAction, HitIcon, Provider, ProviderId, Query, keywords};
use crate::media::MediaCommand;

/// What the music provider needs to know about the current session.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NowPlaying {
    pub title: String,
    pub artist: String,
    pub playing: bool,
    pub can_play_pause: bool,
    pub can_previous: bool,
    pub can_next: bool,
}

pub struct MusicProvider<'a> {
    /// None = nothing is playing or paused.
    pub now: Option<&'a NowPlaying>,
}

const PLAY_PAUSE: &[&str] = &["play", "pause", "resume", "play pause", "toggle music"];
const NEXT: &[&str] = &["next", "skip", "next track", "next song"];
const PREVIOUS: &[&str] = &[
    "prev",
    "previous",
    "previous track",
    "previous song",
    "back",
];

impl Provider for MusicProvider<'_> {
    fn id(&self) -> ProviderId {
        ProviderId::Music
    }

    fn query(&self, q: &Query, out: &mut Vec<Hit>) {
        let Some(now) = self.now else {
            return;
        };
        let track = match (now.title.is_empty(), now.artist.is_empty()) {
            (false, false) => format!("{} · {}", now.title, now.artist),
            (false, true) => now.title.clone(),
            _ => "Music".to_string(),
        };
        let (play_title, play_glyph) = if now.playing {
            ("Pause", '\u{E769}')
        } else {
            ("Play", '\u{E768}')
        };
        let controls = [
            (
                PLAY_PAUSE,
                now.can_play_pause,
                play_title,
                play_glyph,
                MediaCommand::PlayPause,
            ),
            (
                NEXT,
                now.can_next,
                "Next track",
                '\u{E893}',
                MediaCommand::Next,
            ),
            (
                PREVIOUS,
                now.can_previous,
                "Previous track",
                '\u{E892}',
                MediaCommand::Previous,
            ),
        ];
        for (words, enabled, title, glyph, command) in controls {
            let Some(score) = keywords::score(&q.lower, words).filter(|_| enabled) else {
                continue;
            };
            out.push(Hit {
                provider: ProviderId::Music,
                title: title.to_string(),
                subtitle: track.clone(),
                icon: HitIcon::Glyph(glyph),
                action: HitAction::Media(command),
                score,
                confirm: None,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now(playing: bool) -> NowPlaying {
        NowPlaying {
            title: "Song".into(),
            artist: "Band".into(),
            playing,
            can_play_pause: true,
            can_previous: true,
            can_next: false,
        }
    }

    fn find(now: Option<&NowPlaying>, s: &str) -> Vec<Hit> {
        let mut out = Vec::new();
        MusicProvider { now }.query(&Query::new(s, false), &mut out);
        out
    }

    #[test]
    fn controls_the_current_track() {
        let playing = now(true);
        let pause = find(Some(&playing), "pause");
        assert_eq!(pause[0].title, "Pause");
        assert_eq!(pause[0].subtitle, "Song · Band");
        assert_eq!(pause[0].action, HitAction::Media(MediaCommand::PlayPause));
        // Typing "play" while it plays offers the toggle as "Pause".
        assert_eq!(find(Some(&playing), "play")[0].title, "Pause");
        assert_eq!(find(Some(&now(false)), "play")[0].title, "Play");
        assert_eq!(
            find(Some(&playing), "prev")[0].action,
            HitAction::Media(MediaCommand::Previous)
        );
    }

    #[test]
    fn nothing_without_music_or_when_disabled() {
        assert!(find(None, "play").is_empty());
        // This app can't skip.
        assert!(find(Some(&now(true)), "next").is_empty());
    }
}
