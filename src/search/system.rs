//! System actions by keyword (SUPERSEARCH.md 3, "system"): lock, sleep,
//! restart, shut down, sign out, empty the Recycle Bin, Task Manager and
//! the Run dialog. Restart, shut down, sign out and emptying the bin need
//! a second Enter.

use super::{Hit, HitAction, HitIcon, Provider, ProviderId, Query, keywords};
use crate::power::PowerAction;

/// At most this many system rows per search.
const MAX_HITS: usize = 4;

/// Opens the classic Run dialog (Win+R) through the shell.
const RUN_DIALOG: &str = "shell:::{2559a1f3-21d7-11d4-bdaf-00c04f60b9f0}";

struct Entry {
    title: &'static str,
    subtitle: &'static str,
    keywords: &'static [&'static str],
    glyph: char,
    action: fn() -> HitAction,
    confirm: Option<&'static str>,
}

const ENTRIES: &[Entry] = &[
    Entry {
        title: "Lock",
        subtitle: "Lock the PC",
        keywords: &["lock", "lock pc", "lock computer"],
        glyph: '\u{E72E}',
        action: || HitAction::Power(PowerAction::Lock),
        confirm: None,
    },
    Entry {
        title: "Sleep",
        subtitle: "Put the PC to sleep",
        keywords: &["sleep", "suspend"],
        glyph: '\u{E708}',
        action: || HitAction::Power(PowerAction::Sleep),
        confirm: None,
    },
    Entry {
        title: "Restart",
        subtitle: "Restart the PC",
        keywords: &["restart", "reboot"],
        glyph: '\u{E777}',
        action: || HitAction::Power(PowerAction::Restart),
        confirm: Some("Press Enter again to restart"),
    },
    Entry {
        title: "Shut down",
        subtitle: "Turn the PC off",
        keywords: &["shut down", "shutdown", "power off", "turn off"],
        glyph: '\u{E7E8}',
        action: || HitAction::Power(PowerAction::ShutDown),
        confirm: Some("Press Enter again to shut down"),
    },
    Entry {
        title: "Sign out",
        subtitle: "Sign out of Windows",
        keywords: &[
            "sign out", "signout", "log out", "logout", "log off", "logoff",
        ],
        glyph: '\u{E748}',
        action: || HitAction::SignOut,
        confirm: Some("Press Enter again to sign out"),
    },
    Entry {
        title: "Empty Recycle Bin",
        subtitle: "Delete everything in the Recycle Bin",
        keywords: &[
            "empty recycle bin",
            "recycle bin",
            "empty trash",
            "trash",
            "empty bin",
        ],
        glyph: '\u{E74D}',
        action: || HitAction::EmptyRecycleBin,
        confirm: Some("Press Enter again to empty the Recycle Bin"),
    },
    Entry {
        title: "Task Manager",
        subtitle: "See running apps and performance",
        keywords: &["task manager", "taskmgr"],
        glyph: '\u{E9D9}',
        action: || HitAction::open("taskmgr.exe"),
        confirm: None,
    },
    Entry {
        title: "Run…",
        subtitle: "The Run dialog (Win+R)",
        keywords: &["run", "run dialog"],
        glyph: '\u{E756}',
        action: || HitAction::Open {
            target: "explorer.exe".into(),
            args: RUN_DIALOG.into(),
        },
        confirm: None,
    },
];

pub struct SystemProvider;

impl Provider for SystemProvider {
    fn id(&self) -> ProviderId {
        ProviderId::System
    }

    fn query(&self, q: &Query, out: &mut Vec<Hit>) {
        for (score, e) in keywords::best(ENTRIES, &q.lower, MAX_HITS, |e| e.keywords) {
            out.push(Hit {
                provider: ProviderId::System,
                title: e.title.to_string(),
                subtitle: e.subtitle.to_string(),
                icon: HitIcon::Glyph(e.glyph),
                action: (e.action)(),
                score,
                confirm: e.confirm,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn find(s: &str) -> Vec<Hit> {
        let mut out = Vec::new();
        SystemProvider.query(&Query::new(s, false), &mut out);
        out
    }

    #[test]
    fn keywords_find_actions() {
        assert_eq!(find("lock")[0].action, HitAction::Power(PowerAction::Lock));
        assert_eq!(
            find("reboot")[0].action,
            HitAction::Power(PowerAction::Restart)
        );
        assert_eq!(
            find("empty recycle bin")[0].action,
            HitAction::EmptyRecycleBin
        );
        assert_eq!(find("trash")[0].action, HitAction::EmptyRecycleBin);
        assert_eq!(find("log out")[0].action, HitAction::SignOut);
        assert_eq!(find("Task Manager")[0].title, "Task Manager");
        assert!(find("x").is_empty());
        assert!(find("notepad").is_empty());
    }

    #[test]
    fn dangerous_ones_ask_first() {
        for s in ["restart", "shutdown", "sign out", "empty recycle bin"] {
            assert!(find(s)[0].confirm.is_some(), "{s}");
        }
        for s in ["lock", "sleep", "task manager", "run"] {
            assert!(find(s)[0].confirm.is_none(), "{s}");
        }
    }
}
