//! Windows Settings pages and classic admin tools by keyword
//! (SUPERSEARCH.md 3, "settings"): "bluetooth", "brightness" → Display,
//! "wifi", "device manager"...
//!
//! Each opens through the shell: an `ms-settings:` page, an `.msc`
//! console, or a Control Panel item.

use super::{Hit, HitAction, HitIcon, Provider, ProviderId, Query, keywords};

/// At most this many Settings rows per search.
const MAX_HITS: usize = 4;
/// The gear glyph for Settings pages, and a "window" one for tools.
const SETTINGS_GLYPH: char = '\u{E713}';
const TOOL_GLYPH: char = '\u{E90F}';

struct Page {
    title: &'static str,
    /// What to open, and its arguments ("" = none).
    target: &'static str,
    args: &'static str,
    keywords: &'static [&'static str],
}

const fn page(title: &'static str, uri: &'static str, keywords: &'static [&'static str]) -> Page {
    Page {
        title,
        target: uri,
        args: "",
        keywords,
    }
}

/// Settings pages (`ms-settings:`), then classic tools.
const PAGES: &[Page] = &[
    page(
        "Bluetooth & devices",
        "ms-settings:bluetooth",
        &["bluetooth", "devices", "pair", "headphones"],
    ),
    page(
        "Display",
        "ms-settings:display",
        &[
            "display",
            "screen",
            "brightness",
            "resolution",
            "scale",
            "monitor",
            "hdr",
        ],
    ),
    page(
        "Night light",
        "ms-settings:nightlight",
        &["night light", "blue light", "nightlight"],
    ),
    page(
        "Sound",
        "ms-settings:sound",
        &[
            "sound",
            "audio",
            "volume",
            "speakers",
            "microphone",
            "mic",
            "output device",
        ],
    ),
    page(
        "Wi-Fi",
        "ms-settings:network-wifi",
        &["wifi", "wi-fi", "wireless"],
    ),
    page(
        "Network & internet",
        "ms-settings:network",
        &["network", "internet", "ethernet", "ip address"],
    ),
    page("VPN", "ms-settings:network-vpn", &["vpn"]),
    page(
        "Mobile hotspot",
        "ms-settings:network-mobilehotspot",
        &["hotspot", "mobile hotspot", "tethering"],
    ),
    page(
        "Airplane mode",
        "ms-settings:network-airplanemode",
        &["airplane mode", "flight mode", "aeroplane mode"],
    ),
    page("Proxy", "ms-settings:network-proxy", &["proxy"]),
    page(
        "Power & battery",
        "ms-settings:powersleep",
        &[
            "battery",
            "power",
            "power mode",
            "battery saver",
            "energy saver",
            "sleep settings",
            "screen timeout",
        ],
    ),
    page(
        "Background",
        "ms-settings:personalization-background",
        &["wallpaper", "background", "desktop background"],
    ),
    page(
        "Personalization",
        "ms-settings:personalization",
        &["personalization", "personalize"],
    ),
    page(
        "Colors",
        "ms-settings:colors",
        &[
            "colors",
            "colours",
            "dark mode",
            "light mode",
            "accent color",
            "transparency",
        ],
    ),
    page("Taskbar", "ms-settings:taskbar", &["taskbar"]),
    page(
        "Lock screen",
        "ms-settings:lockscreen",
        &["lock screen", "lockscreen"],
    ),
    page("Fonts", "ms-settings:fonts", &["fonts", "font"]),
    page(
        "Installed apps",
        "ms-settings:appsfeatures",
        &[
            "installed apps",
            "apps",
            "uninstall",
            "programs",
            "remove app",
        ],
    ),
    page(
        "Default apps",
        "ms-settings:defaultapps",
        &[
            "default apps",
            "default browser",
            "open with",
            "file associations",
        ],
    ),
    page(
        "Startup apps",
        "ms-settings:startupapps",
        &["startup apps", "startup", "autostart"],
    ),
    page(
        "Windows Update",
        "ms-settings:windowsupdate",
        &["windows update", "update", "updates"],
    ),
    page(
        "Privacy & security",
        "ms-settings:privacy",
        &["privacy", "permissions", "camera access", "location"],
    ),
    page(
        "Mouse",
        "ms-settings:mousetouchpad",
        &["mouse", "cursor", "pointer speed", "scroll"],
    ),
    page(
        "Touchpad",
        "ms-settings:devices-touchpad",
        &["touchpad", "trackpad", "gestures"],
    ),
    page(
        "Typing",
        "ms-settings:typing",
        &["typing", "keyboard", "autocorrect", "spell"],
    ),
    page(
        "Date & time",
        "ms-settings:dateandtime",
        &["date", "time", "clock", "time zone", "timezone"],
    ),
    page(
        "Language & region",
        "ms-settings:regionlanguage",
        &["language", "region", "input method", "keyboard layout"],
    ),
    page(
        "Storage",
        "ms-settings:storagesense",
        &[
            "storage",
            "disk space",
            "free space",
            "storage sense",
            "cleanup",
        ],
    ),
    page(
        "Notifications",
        "ms-settings:notifications",
        &["notifications", "do not disturb"],
    ),
    page("Focus", "ms-settings:focus", &["focus", "focus session"]),
    page(
        "Multitasking",
        "ms-settings:multitasking",
        &["multitasking", "snap", "snap windows", "alt tab"],
    ),
    page(
        "Clipboard",
        "ms-settings:clipboard",
        &["clipboard", "clipboard history"],
    ),
    page(
        "Accounts",
        "ms-settings:accounts",
        &["accounts", "account", "your info", "profile picture"],
    ),
    page(
        "Sign-in options",
        "ms-settings:signinoptions",
        &[
            "sign-in options",
            "sign in options",
            "password",
            "pin",
            "windows hello",
            "fingerprint",
        ],
    ),
    page(
        "Printers & scanners",
        "ms-settings:printers",
        &["printers", "printer", "scanners", "scanner"],
    ),
    page(
        "Accessibility",
        "ms-settings:easeofaccess",
        &[
            "accessibility",
            "ease of access",
            "narrator",
            "magnifier",
            "text size",
        ],
    ),
    page(
        "About",
        "ms-settings:about",
        &[
            "about",
            "pc name",
            "rename pc",
            "system info",
            "specs",
            "windows version",
        ],
    ),
    page(
        "Recovery",
        "ms-settings:recovery",
        &["recovery", "reset pc", "reset this pc"],
    ),
    page(
        "Activation",
        "ms-settings:activation",
        &["activation", "product key", "activate windows"],
    ),
    page(
        "For developers",
        "ms-settings:developers",
        &["developer mode", "developers", "for developers"],
    ),
    page(
        "Gaming",
        "ms-settings:gaming-gamebar",
        &["gaming", "game bar", "game mode"],
    ),
    // Classic tools.
    page(
        "Control Panel",
        "control.exe",
        &["control panel", "control"],
    ),
    Page {
        title: "Devices and Printers",
        target: "control.exe",
        args: "printers",
        keywords: &["control printers", "devices and printers"],
    },
    page(
        "Device Manager",
        "devmgmt.msc",
        &["device manager", "devmgmt", "drivers"],
    ),
    page("Services", "services.msc", &["services", "services.msc"]),
    page(
        "Registry Editor (admin)",
        "regedit.exe",
        &["registry editor", "registry", "regedit"],
    ),
    page(
        "Disk Management",
        "diskmgmt.msc",
        &["disk management", "partitions", "diskmgmt"],
    ),
    page(
        "Event Viewer",
        "eventvwr.msc",
        &["event viewer", "eventvwr", "logs"],
    ),
    page(
        "Computer Management",
        "compmgmt.msc",
        &["computer management", "compmgmt"],
    ),
    page(
        "Network Connections",
        "ncpa.cpl",
        &["network connections", "network adapters", "ncpa"],
    ),
    page(
        "Environment Variables",
        "rundll32.exe",
        &["environment variables", "env vars", "path variable"],
    ),
    page(
        "Programs and Features",
        "appwiz.cpl",
        &["programs and features", "appwiz"],
    ),
];

/// The arguments for "Environment Variables" (rundll32 opens the dialog).
const ENV_ARGS: &str = "sysdm.cpl,EditEnvironmentVariables";

pub struct SettingsProvider;

impl Provider for SettingsProvider {
    fn id(&self) -> ProviderId {
        ProviderId::Settings
    }

    fn query(&self, q: &Query, out: &mut Vec<Hit>) {
        for (score, p) in keywords::best(PAGES, &q.lower, MAX_HITS, |p| p.keywords) {
            let settings = p.target.starts_with("ms-settings:");
            let args = if p.target == "rundll32.exe" {
                ENV_ARGS
            } else {
                p.args
            };
            out.push(Hit {
                provider: ProviderId::Settings,
                title: p.title.to_string(),
                subtitle: if settings { "Settings" } else { "Windows tool" }.to_string(),
                icon: HitIcon::Glyph(if settings { SETTINGS_GLYPH } else { TOOL_GLYPH }),
                action: HitAction::Open {
                    target: p.target.to_string(),
                    args: args.to_string(),
                },
                score,
                confirm: None,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::score;

    fn find(s: &str) -> Vec<Hit> {
        let mut out = Vec::new();
        SettingsProvider.query(&Query::new(s, false), &mut out);
        out
    }

    fn target(h: &Hit) -> &str {
        match &h.action {
            HitAction::Open { target, .. } => target,
            _ => "",
        }
    }

    #[test]
    fn keywords_and_synonyms() {
        assert_eq!(target(&find("bluetooth")[0]), "ms-settings:bluetooth");
        assert_eq!(target(&find("brightness")[0]), "ms-settings:display");
        assert_eq!(target(&find("wifi")[0]), "ms-settings:network-wifi");
        assert_eq!(target(&find("sound")[0]), "ms-settings:sound");
        assert_eq!(
            target(&find("wallpaper")[0]),
            "ms-settings:personalization-background"
        );
        assert_eq!(target(&find("device manager")[0]), "devmgmt.msc");
        assert_eq!(find("regedit")[0].title, "Registry Editor (admin)");
        assert_eq!(find("bluetooth")[0].score, score::KEYWORD_EXACT);
        assert_eq!(find("blue")[0].score, score::KEYWORD_PREFIX);
        let printers = find("control printers");
        assert_eq!(
            printers[0].action,
            HitAction::Open {
                target: "control.exe".into(),
                args: "printers".into()
            }
        );
    }

    #[test]
    fn every_page_opens_something_known() {
        for p in PAGES {
            assert!(!p.keywords.is_empty(), "{}", p.title);
            assert!(p.keywords.iter().all(|k| *k == k.to_lowercase()));
            let ok = p.target.starts_with("ms-settings:")
                || [".msc", ".exe", ".cpl"]
                    .iter()
                    .any(|e| p.target.ends_with(e));
            assert!(ok, "{}", p.target);
        }
    }

    #[test]
    fn at_most_four() {
        assert!(find("sc").len() <= MAX_HITS);
        assert!(find("zzzz").is_empty());
    }
}
