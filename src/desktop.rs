//! Small, read-only index of freedesktop launchers. Built once for a live
//! session; lookups never touch disk or run a launcher command.
use crate::config::{AppInfo, clean_class};
use std::{
    borrow::Cow,
    collections::{HashMap, HashSet, hash_map::Entry},
    env, fs,
    io::Read,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
};

const MAX_FILE: u64 = 128 * 1024;
const MAX_STARTUP_BYTES: u64 = 16 * 1024 * 1024;
const MAX_DIR_ENTRIES: usize = 16_384;

#[derive(Clone, Debug, Default)]
pub struct DesktopIndex {
    apps: Vec<AppInfo>,
    // None marks a key with conflicting metadata, not an absent key.
    ids: HashMap<String, Option<usize>>,
    classes: HashMap<String, Option<usize>>,
}

impl DesktopIndex {
    pub fn discover() -> Self {
        let home = absolute("HOME");
        let data_home =
            absolute("XDG_DATA_HOME").or_else(|| home.as_ref().map(|p| p.join(".local/share")));
        let mut roots: Vec<_> = data_home.iter().cloned().collect();
        let data_dirs = env::var_os("XDG_DATA_DIRS")
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| "/usr/local/share:/usr/share".into());
        roots.extend(env::split_paths(&data_dirs).filter(|p| p.is_absolute()));
        // Export paths are not always included in a compositor's XDG_DATA_DIRS.
        if let Some(data_home) = data_home {
            roots.push(data_home.join("flatpak/exports/share"));
        }
        if let Some(home) = home {
            roots.push(home.join(".local/share/flatpak/exports/share"));
        }
        roots.extend([
            PathBuf::from("/var/lib/flatpak/exports/share"),
            PathBuf::from("/var/lib/snapd/desktop"),
        ]);
        let mut index = Self::default();
        let mut seen_roots = HashSet::new();
        let mut scan = Scan::default();
        for root in roots {
            if seen_roots.insert(root.clone()) {
                scan.visit(&mut index, &root.join("applications"), "", 0);
            }
        }
        index
    }

    pub fn lookup(&self, key: &str) -> Option<&AppInfo> {
        let id = key.strip_suffix(".desktop").unwrap_or(key);
        // Prefer a direct desktop ID over an indirect window-class match.
        let position = self
            .ids
            .get(id)
            .or_else(|| self.classes.get(key))
            .copied()
            .flatten()?;
        self.apps.get(position)
    }

    fn insert(&mut self, id: &str, file: Launcher<'_>) {
        if file.hidden || file.kind != "Application" {
            return;
        }
        let (category, icon) = classify(&file).unwrap_or(("Other", "app"));
        let name: String = value(file.name)
            .trim()
            .chars()
            .filter(|c| {
                !c.is_control() && !matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
            })
            .take(72)
            .collect();
        let position = self.apps.len();
        self.apps.push(AppInfo {
            name,
            category: category.into(),
            icon: icon.into(),
        });
        record(&mut self.ids, clean_class(id), position, &self.apps);
        record(
            &mut self.classes,
            clean_class(&value(file.class)),
            position,
            &self.apps,
        );
    }
}

fn record(
    matches: &mut HashMap<String, Option<usize>>,
    key: String,
    position: usize,
    apps: &[AppInfo],
) {
    if key.is_empty() {
        return;
    }
    match matches.entry(key) {
        Entry::Vacant(slot) => {
            slot.insert(Some(position));
        }
        Entry::Occupied(mut slot) => {
            // Case differences or a shared StartupWMClass can collapse distinct
            // launchers onto one key. Only resolve if their metadata agrees.
            if slot
                .get()
                .is_some_and(|previous| apps[previous] != apps[position])
            {
                slot.insert(None);
            }
        }
    }
}

#[derive(Default)]
struct Scan {
    seen_ids: HashSet<String>,
    entries: usize,
    bytes: u64,
}

impl Scan {
    fn visit(&mut self, index: &mut DesktopIndex, dir: &Path, prefix: &str, depth: u8) {
        let Ok(iter) = fs::read_dir(dir) else {
            return;
        };
        for entry in iter.flatten() {
            if self.entries >= MAX_DIR_ENTRIES || self.bytes >= MAX_STARTUP_BYTES {
                return;
            }
            self.entries += 1;
            let Ok(name) = entry.file_name().into_string() else {
                continue;
            };
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                // Real directories only here; Flatpak launcher file symlinks are fine.
                if depth < 5 {
                    self.visit(index, &entry.path(), &format!("{prefix}{name}-"), depth + 1);
                }
                continue;
            }
            if !kind.is_file() && !kind.is_symlink() {
                continue;
            }
            let Some(stem) = name.strip_suffix(".desktop") else {
                continue;
            };
            // XDG shadowing uses the original, case-sensitive desktop file ID.
            // Only normalize after that, for comparison with stored window IDs.
            let id = format!("{prefix}{stem}");
            if id.is_empty() || self.seen_ids.contains(&id) {
                continue;
            }
            let Some(text) = self.read_launcher(&entry.path()) else {
                continue;
            };
            let Some(launcher) = Launcher::parse(&text) else {
                continue;
            };
            // The first XDG file with this ID wins, including Hidden=true masks.
            index.insert(&id, launcher);
            self.seen_ids.insert(id);
        }
    }

    fn read_launcher(&mut self, path: &Path) -> Option<String> {
        // Only regular files, even when following a launcher symlink. Nonblocking
        // open prevents a malformed FIFO from stalling session startup.
        let file = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(path)
            .ok()?;
        let meta = file.metadata().ok()?;
        if !meta.is_file() || meta.len() > MAX_FILE {
            return None;
        }
        let limit = (MAX_STARTUP_BYTES - self.bytes).min(MAX_FILE + 1);
        let mut bytes = Vec::new();
        let read = file.take(limit).read_to_end(&mut bytes);
        self.bytes += bytes.len() as u64;
        if read.is_err() || bytes.len() as u64 >= limit {
            return None;
        }
        String::from_utf8(bytes).ok()
    }
}

fn absolute(variable: &str) -> Option<PathBuf> {
    env::var_os(variable)
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
}

#[derive(Default)]
struct Launcher<'a> {
    kind: &'a str,
    hidden: bool,
    name: &'a str,
    class: &'a str,
    categories: &'a str,
    generic_name: &'a str,
    mime: &'a str,
}

impl<'a> Launcher<'a> {
    fn parse(text: &'a str) -> Option<Self> {
        let mut entry = Self::default();
        let mut found = false;
        for line in text.trim_start_matches('\u{feff}').lines().map(str::trim) {
            if line.starts_with('[') && line.ends_with(']') {
                if found {
                    break;
                }
                found = line == "[Desktop Entry]";
                continue;
            }
            if !found || line.starts_with('#') {
                continue;
            }
            if let Some((key, v)) = line.split_once('=') {
                let v = v.trim();
                match key.trim() {
                    "Type" => entry.kind = v,
                    "Hidden" => entry.hidden = v.eq_ignore_ascii_case("true"),
                    "Name" => entry.name = v,
                    "StartupWMClass" => entry.class = v,
                    "Categories" => entry.categories = v,
                    "GenericName" => entry.generic_name = v,
                    "MimeType" => entry.mime = v,
                    _ => {} // In particular, never interpret Exec, URLs, or arguments.
                }
            }
        }
        found.then_some(entry)
    }
}

fn value(s: &str) -> Cow<'_, str> {
    if !s.contains('\\') {
        return Cow::Borrowed(s);
    }
    let mut result = String::with_capacity(s.len());
    let mut items = s.chars();
    while let Some(c) = items.next() {
        if c != '\\' {
            result.push(c);
            continue;
        }
        match items.next() {
            Some('s') => result.push(' '),
            Some('n') => result.push('\n'),
            Some('t') => result.push('\t'),
            Some('r') => result.push('\r'),
            Some('\\') => result.push('\\'),
            Some(other) => {
                result.push('\\');
                result.push(other);
            }
            None => result.push('\\'),
        }
    }
    Cow::Owned(result)
}

fn classify(file: &Launcher<'_>) -> Option<(&'static str, &'static str)> {
    let tags = |wanted: &[&str]| {
        file.categories.split(';').any(|part| {
            wanted
                .iter()
                .any(|tag| part.trim().eq_ignore_ascii_case(tag))
        })
    };
    // Specific declarations win over broad launch-menu categories such as
    // Network, Utility, or AudioVideo. Network alone doesn't mean a browser.
    if tags(&["WebBrowser"]) {
        return Some(("Browsing", "globe"));
    }
    if tags(&[
        "Chat",
        "InstantMessaging",
        "IRCClient",
        "Email",
        "Telephony",
        "VideoConference",
        "X-Messaging",
    ]) {
        return Some(("Communication", "message"));
    }
    if tags(&["TerminalEmulator"]) {
        return Some(("Utilities", "terminal"));
    }
    if tags(&["FileManager"]) {
        return Some(("Utilities", "folder"));
    }
    if tags(&[
        "Development",
        "IDE",
        "Building",
        "Debugger",
        "RevisionControl",
        "WebDevelopment",
    ]) {
        return Some(("Productivity", "code"));
    }
    if tags(&[
        "Graphics",
        "2DGraphics",
        "3DGraphics",
        "Photography",
        "AudioVideoEditing",
        "Publishing",
    ]) {
        return Some(("Productivity", "brush"));
    }
    if tags(&[
        "Office",
        "TextEditor",
        "WordProcessor",
        "Spreadsheet",
        "Presentation",
        "Calendar",
        "Education",
        "Science",
    ]) {
        return Some(("Productivity", "book"));
    }
    if tags(&["Game", "Amusement"]) {
        return Some(("Entertainment", "app"));
    }
    if tags(&["Video", "TV"]) {
        return Some(("Entertainment", "film"));
    }
    if tags(&["Audio", "Music"]) {
        return Some(("Entertainment", "music"));
    }
    if tags(&["Player"]) {
        return Some(("Entertainment", "film"));
    }
    let mime = |wanted: &str| {
        file.mime
            .split(';')
            .any(|m| m.trim().eq_ignore_ascii_case(wanted))
    };
    let generic = value(file.generic_name);
    // These fallbacks shouldn't override a specific category, e.g. a mail
    // client or an HTML editor can also declare URL and HTML handlers.
    if generic.eq_ignore_ascii_case("Web Browser")
        || generic.eq_ignore_ascii_case("Internet Browser")
        || (mime("text/html") && mime("x-scheme-handler/http") && mime("x-scheme-handler/https"))
    {
        return Some(("Browsing", "globe"));
    }
    if tags(&["AudioVideo"]) {
        return Some(("Entertainment", "film"));
    }
    if tags(&[
        "Utility",
        "Settings",
        "System",
        "FileTools",
        "Archiving",
        "Security",
        "RemoteAccess",
        "FileTransfer",
        "PackageManager",
        "Monitor",
    ]) {
        return Some(("Utilities", "app"));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_reads_desktop_entry_and_matches_id_or_startup_class() {
        let text = "\u{feff}[Desktop Entry]\r\nType=Application\r\nName=Paper\\sPlane\r\nName[de]=Anderer Name\r\nStartupWMClass=PLANE_GUI\r\nCategories=Network;WebBrowser;Utility;\r\n[Desktop Action Fake]\r\nName=Not the app\r\n";
        let mut index = DesktopIndex::default();
        index.insert("org.example.plane", Launcher::parse(text).unwrap());
        let plane = index.lookup("plane_gui").unwrap();
        assert_eq!(
            (plane.name.as_str(), plane.category.as_str()),
            ("Paper Plane", "Browsing")
        );
        assert_eq!(index.lookup("org.example.plane.desktop"), Some(plane));
    }

    #[test]
    fn strong_categories_win_without_network_or_name_guessing() {
        let determine = |categories| {
            classify(&Launcher {
                categories,
                ..Launcher::default()
            })
        };
        assert_eq!(determine("Network;"), None);
        assert_eq!(
            determine("Network;Email;Office;"),
            Some(("Communication", "message"))
        );
        assert_eq!(
            determine("System;TerminalEmulator;Development;"),
            Some(("Utilities", "terminal"))
        );
        assert_eq!(
            determine("AudioVideo;AudioVideoEditing;"),
            Some(("Productivity", "brush"))
        );
        let file = Launcher {
            mime: "text/html;x-scheme-handler/http;x-scheme-handler/https;",
            ..Launcher::default()
        };
        assert_eq!(classify(&file), Some(("Browsing", "globe")));
    }

    #[test]
    fn conflicts_do_not_classify_by_ambiguous_window_class() {
        let mut index = DesktopIndex::default();
        for (id, categories) in [("chat", "Chat;"), ("web", "WebBrowser;")] {
            index.insert(
                id,
                Launcher {
                    kind: "Application",
                    class: "something-generic",
                    name: id,
                    categories,
                    ..Launcher::default()
                },
            );
        }
        assert!(index.lookup("something-generic").is_none());
        assert_eq!(index.lookup("web").unwrap().category, "Browsing");
    }
}
