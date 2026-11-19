use crate::desktop::DesktopIndex;
use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    io::Write,
    path::{Path, PathBuf},
};

pub const SAMPLE: &str = include_str!("../config.example.toml");
pub const CATEGORIES: &[&str] = &[
    "Productivity",
    "Browsing",
    "Communication",
    "Entertainment",
    "Utilities",
    "Other",
];
pub const ICONS: &[&str] = &[
    "code", "globe", "message", "music", "terminal", "folder", "brush", "film", "app", "book",
];

#[derive(Debug, Clone)]
pub struct Paths {
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
}

impl Paths {
    pub fn discover() -> Result<Self> {
        let home = env::var_os("HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .context("HOME must be set to an absolute path")?;
        let xdg = |var, fallback| {
            env::var_os(var)
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
                .unwrap_or_else(|| home.join(fallback))
                .join("irminsul")
        };
        Ok(Self {
            config_dir: xdg("XDG_CONFIG_HOME", ".config"),
            data_dir: xdg("XDG_DATA_HOME", ".local/share"),
        })
    }

    pub fn config(&self) -> PathBuf {
        self.config_dir.join("config.toml")
    }
    pub fn web(&self) -> PathBuf {
        self.config_dir.join("web")
    }
    pub fn db(&self) -> PathBuf {
        self.data_dir.join("usage.sqlite3")
    }
}

pub fn write_new(path: &Path, data: &[u8]) -> Result<bool> {
    let mut file = match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
    {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => return Ok(false),
        Err(e) => return Err(e).with_context(|| format!("creating {}", path.display())),
    };
    file.write_all(data)
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(true)
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub port: u16,
    pub idle_seconds: u32,
    pub respect_idle_inhibitors: bool,
    pub checkpoint_seconds: u64,
    pub retention_days: u32,
    pub daily_goal_minutes: u16,
    pub fallback_probe_seconds: u64,
    pub excluded_apps: BTreeSet<String>,
    pub aliases: BTreeMap<String, String>,
    pub apps: BTreeMap<String, AppOverride>,
    #[serde(skip)]
    pub desktop_index: DesktopIndex,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppOverride {
    pub name: Option<String>,
    pub category: Option<String>,
    pub icon: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppInfo {
    pub name: String,
    pub category: String,
    pub icon: String,
}

pub struct ResolvedApp {
    pub info: AppInfo,
    pub category_source: &'static str,
}

fn category_icon(category: &str) -> &'static str {
    match category {
        "Productivity" => "book",
        "Browsing" => "globe",
        "Communication" => "message",
        "Entertainment" => "film",
        _ => "app",
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            port: 8765,
            idle_seconds: 180,
            respect_idle_inhibitors: false,
            checkpoint_seconds: 60,
            retention_days: 180,
            daily_goal_minutes: 300,
            fallback_probe_seconds: 60,
            excluded_apps: BTreeSet::new(),
            aliases: BTreeMap::new(),
            apps: BTreeMap::new(),
            desktop_index: DesktopIndex::default(),
        }
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        Self::parse(&text).with_context(|| format!("in {}", path.display()))
    }

    pub fn parse(input: &str) -> Result<Self> {
        let mut config: Self = toml::from_str(input)?;
        validate_port(config.port)?;
        ensure!(
            config.idle_seconds <= 14_400,
            "idle_seconds must be 0–14400"
        );
        ensure!(
            (15..=3600).contains(&config.checkpoint_seconds),
            "checkpoint_seconds must be 15–3600"
        );
        ensure!(
            config.retention_days == 0 || (30..=36_500).contains(&config.retention_days),
            "retention_days must be 0 or 30–36500"
        );
        validate_goal(config.daily_goal_minutes)?;
        ensure!(
            config.fallback_probe_seconds == 0
                || (30..=3600).contains(&config.fallback_probe_seconds),
            "fallback_probe_seconds must be 0 or 30–3600"
        );
        config.excluded_apps = config
            .excluded_apps
            .into_iter()
            .map(|id| clean_class(&id))
            .collect();
        config.aliases = config
            .aliases
            .into_iter()
            .map(|(k, v)| (clean_class(&k), clean_class(&v)))
            .collect();
        config.apps = config
            .apps
            .into_iter()
            .map(|(k, v)| (clean_class(&k), v))
            .collect();
        for (alias, dest) in &config.aliases {
            ensure!(
                !alias.is_empty() && !dest.is_empty(),
                "alias keys and values must not be empty"
            );
            ensure!(
                !config.aliases.contains_key(dest),
                "aliases must point directly at a canonical ID ({alias} → {dest})"
            );
        }
        for (key, app) in &mut config.apps {
            ensure!(!key.is_empty(), "app keys must not be empty");
            ensure!(
                app.name.is_some() || app.category.is_some() || app.icon.is_some(),
                "empty override for [{key}]"
            );
            if let Some(name) = &mut app.name {
                *name = name.trim().to_owned();
                ensure!(
                    !name.is_empty()
                        && name.chars().count() <= 72
                        && !name.chars().any(char::is_control),
                    "invalid name for [{key}]"
                );
            }
            if let Some(category) = &app.category {
                ensure!(
                    CATEGORIES.contains(&category.as_str()),
                    "unknown category for [{key}]: {category}"
                );
            }
            if let Some(icon) = &app.icon {
                ensure!(
                    ICONS.contains(&icon.as_str()),
                    "unknown icon for [{key}]: {icon}"
                );
            }
        }
        Ok(config)
    }

    pub fn canonical_class(&self, class: &str) -> Option<String> {
        let raw = clean_class(class);
        if raw.is_empty() || self.excluded_apps.contains(&raw) {
            return None;
        }
        let id = self
            .aliases
            .get(&raw)
            .map(String::as_str)
            .unwrap_or_else(|| builtin_alias(&raw));
        (!self.excluded_apps.contains(id)).then(|| id.to_owned())
    }

    /// Resolve labels for an already-stored ID. Don't apply user aliases again:
    /// those change tracking identities, not the labels of historical records.
    pub fn app(&self, id: &str) -> ResolvedApp {
        let known_id = builtin_alias(id.strip_suffix(".desktop").unwrap_or(id));
        let (mut info, mut category_source) =
            if let Some((name, category, icon)) = builtin_app(known_id) {
                (
                    AppInfo {
                        name: name.into(),
                        category: category.into(),
                        icon: icon.into(),
                    },
                    "built-in",
                )
            } else if let Some(meta) = self.desktop_index.lookup(id) {
                (
                    meta.clone(),
                    if meta.category == "Other" {
                        "unclassified"
                    } else {
                        "desktop-entry"
                    },
                )
            } else {
                (
                    AppInfo {
                        name: String::new(),
                        category: "Other".into(),
                        icon: "app".into(),
                    },
                    "unclassified",
                )
            };
        if info.name.is_empty() {
            info.name = friendly_name(known_id);
        }
        if let Some(rules) = self.apps.get(id) {
            if let Some(name) = &rules.name {
                info.name.clone_from(name);
            }
            if let Some(category) = &rules.category {
                if *category != info.category {
                    info.icon = category_icon(category).into();
                }
                info.category.clone_from(category);
                category_source = "config";
            }
            if let Some(icon) = &rules.icon {
                info.icon.clone_from(icon);
            }
        }
        ResolvedApp {
            info,
            category_source,
        }
    }
}

// Keep the original stable tracking aliases. Launcher metadata and new catalog
// variants improve presentation without introducing new aliases into the DB.
fn builtin_alias(id: &str) -> &str {
    match id {
        "org.mozilla.firefox" | "firefox-bin" => "firefox",
        "com.visualstudio.code" | "visual-studio-code" | "code-oss" => "code",
        "com.spotify.client" => "spotify",
        "com.discordapp.discord" => "discord",
        "md.obsidian.obsidian" => "obsidian",
        "com.mitchellh.ghostty" => "ghostty",
        "com.google.chrome" => "google-chrome",
        "org.chromium.chromium" => "chromium",
        "org.gnome.nautilus" => "nautilus",
        "org.kde.dolphin" => "dolphin",
        "com.brave.browser" | "brave-browser" => "brave",
        "org.telegram.desktop" | "org.telegram.desktop._" => "telegram-desktop",
        "com.slack.slack" => "slack",
        "com.alacritty.alacritty" => "alacritty",
        "org.wezfurlong.wezterm" => "wezterm",
        "com.valvesoftware.steam" => "steam",
        "org.videolan.vlc" => "vlc",
        other => other,
    }
}

fn builtin_app(id: &str) -> Option<(&'static str, &'static str, &'static str)> {
    Some(match id {
        "helium"
        | "helium-browser"
        | "helium-browser-bin"
        | "helium-linux"
        | "helium-browser_helium-browser"
        | "net.imput.helium"
        | "io.github.imputnet.helium"
        | "com.imputnet.helium" => ("Helium", "Browsing", "globe"),
        "firefox" | "firefox-esr" | "firefox-nightly" | "firefox-developer-edition" => {
            ("Firefox", "Browsing", "globe")
        }
        "librewolf" | "io.gitlab.librewolf-community" => ("LibreWolf", "Browsing", "globe"),
        "zen" | "zen-browser" | "app.zen_browser.zen" => ("Zen Browser", "Browsing", "globe"),
        "google-chrome"
        | "google-chrome-stable"
        | "google-chrome-beta"
        | "google-chrome-unstable" => ("Google Chrome", "Browsing", "globe"),
        "chromium"
        | "chromium-browser"
        | "ungoogled-chromium"
        | "com.github.eloston.ungoogledchromium" => ("Chromium", "Browsing", "globe"),
        "brave" | "brave-browser-beta" | "brave-browser-nightly" => ("Brave", "Browsing", "globe"),
        "vivaldi" | "vivaldi-stable" | "vivaldi-snapshot" | "com.vivaldi.vivaldi" => {
            ("Vivaldi", "Browsing", "globe")
        }
        "microsoft-edge"
        | "microsoft-edge-stable"
        | "microsoft-edge-beta"
        | "microsoft-edge-dev"
        | "com.microsoft.edge" => ("Microsoft Edge", "Browsing", "globe"),
        "waterfox" | "net.waterfox.waterfox" => ("Waterfox", "Browsing", "globe"),
        "floorp" | "one.ablaze.floorp" => ("Floorp", "Browsing", "globe"),
        "tor-browser" | "tor-browser-launcher" => ("Tor Browser", "Browsing", "globe"),
        "mullvad-browser" => ("Mullvad Browser", "Browsing", "globe"),
        "epiphany" | "org.gnome.epiphany" => ("GNOME Web", "Browsing", "globe"),
        "qutebrowser" | "org.qutebrowser.qutebrowser" => ("qutebrowser", "Browsing", "globe"),
        "falkon" | "org.kde.falkon" => ("Falkon", "Browsing", "globe"),
        "opera" | "opera-beta" | "com.opera.opera" => ("Opera", "Browsing", "globe"),
        "code" => ("Visual Studio Code", "Productivity", "code"),
        "obsidian" => ("Obsidian", "Productivity", "book"),
        "zed" | "dev.zed.zed" => ("Zed", "Productivity", "code"),
        "gimp" | "org.gimp.gimp" => ("GIMP", "Productivity", "brush"),
        "libreoffice-writer" => ("LibreOffice Writer", "Productivity", "book"),
        "discord" => ("Discord", "Communication", "message"),
        "slack" => ("Slack", "Communication", "message"),
        "telegram-desktop" => ("Telegram", "Communication", "message"),
        "signal" | "org.signal.signal" => ("Signal", "Communication", "message"),
        "spotify" => ("Spotify", "Entertainment", "music"),
        "steam" => ("Steam", "Entertainment", "app"),
        "vlc" => ("VLC", "Entertainment", "film"),
        "mpv" => ("mpv", "Entertainment", "film"),
        "kitty" => ("Kitty", "Utilities", "terminal"),
        "foot" => ("Foot", "Utilities", "terminal"),
        "ghostty" => ("Ghostty", "Utilities", "terminal"),
        "alacritty" => ("Alacritty", "Utilities", "terminal"),
        "wezterm" => ("WezTerm", "Utilities", "terminal"),
        "nautilus" => ("Files", "Utilities", "folder"),
        "thunar" => ("Thunar", "Utilities", "folder"),
        "dolphin" => ("Dolphin", "Utilities", "folder"),
        _ => return None,
    })
}

fn friendly_name(id: &str) -> String {
    let part = id.rsplit('.').find(|s| !s.is_empty()).unwrap_or(id);
    let name = part
        .split(['-', '_'])
        .filter(|s| !s.is_empty())
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                None => String::new(),
                Some(c) => c.to_uppercase().chain(chars).collect::<String>(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ");
    if name.is_empty() { id.to_owned() } else { name }
}

pub(crate) fn clean_class(class: &str) -> String {
    class
        .trim()
        .chars()
        .filter(|c| {
            !c.is_control() && !matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        })
        .take(160)
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

pub fn validate_port(port: u16) -> Result<()> {
    ensure!(port >= 1024, "port must be between 1024 and 65535");
    Ok(())
}

pub fn validate_goal(minutes: u16) -> Result<()> {
    ensure!(
        minutes <= 1440,
        "goal must be 0 (disabled) or 1–1440 minutes"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn example_configuration_is_valid() {
        assert_eq!(Config::parse(SAMPLE).unwrap().port, 8765);
    }

    #[test]
    fn aliases_and_exclusions_apply_case_insensitively() {
        let c =
            Config::parse("excluded_apps=['PRIVate','ghostty']\n[aliases]\n'Code-Insiders'='code'")
                .unwrap();
        assert_eq!(c.canonical_class("Code-Insiders").as_deref(), Some("code"));
        assert_eq!(c.canonical_class("Private"), None);
        assert_eq!(c.canonical_class("com.mitchellh.ghostty"), None);
        assert_eq!(
            c.canonical_class("ORG.MOZILLA.FIREFOX").as_deref(),
            Some("firefox")
        );
    }

    #[test]
    fn helium_variants_keep_their_stored_ids_and_report_as_browsing() {
        let c = Config::default();
        for class in [
            "Helium",
            "helium-browser",
            "helium-browser-bin",
            "net.imput.Helium",
            "io.github.imputnet.Helium.desktop",
        ] {
            let id = c.canonical_class(class).unwrap();
            assert_eq!(id, class.to_ascii_lowercase());
            let app = c.app(&id);
            assert_eq!(app.info.name, "Helium");
            assert_eq!(app.info.category, "Browsing");
            assert_eq!(app.info.icon, "globe");
            assert_eq!(app.category_source, "built-in");
        }
        assert_eq!(c.app("helium-helper").info.category, "Other");
    }

    #[test]
    fn manual_overrides_and_existing_names_and_categories_win() {
        let c = Config::parse("[apps]\nHELIUM={category='Other'}\ncode={name='Work editor'}\nkitty={name='My Kitty',category='Utilities',icon='terminal'}").unwrap();
        let helium = c.app("helium");
        assert_eq!(helium.info.name, "Helium");
        assert_eq!(helium.info.category, "Other");
        assert_eq!(helium.category_source, "config");
        let code = c.app("code");
        assert_eq!(code.info.name, "Work editor");
        assert_eq!(code.info.category, "Productivity");
        assert_eq!(code.category_source, "built-in");
        let kitty = c.app("kitty");
        assert_eq!(kitty.info.name, "My Kitty");
        assert_eq!(kitty.info.category, "Utilities");
        assert_eq!(kitty.info.icon, "terminal");
        for (id, category) in [
            ("firefox", "Browsing"),
            ("dolphin", "Utilities"),
            ("discord", "Communication"),
            ("spotify", "Entertainment"),
        ] {
            assert_eq!(c.app(id).info.category, category);
        }
    }

    #[test]
    fn rejects_bad_settings() {
        assert!(Config::parse("checkpoint_seconds=1").is_err());
        assert!(Config::parse("daily_goal_minutes=2000").is_err());
        assert!(Config::parse("[aliases]\na='b'\nb='a'").is_err());
        assert!(Config::parse("[apps]\na={category='wrong'}").is_err());
        assert!(Config::parse("[apps]\na={}").is_err());
    }
}
