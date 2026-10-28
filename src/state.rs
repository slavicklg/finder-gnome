// SPDX-FileCopyrightText: 2026 Slavick
// SPDX-License-Identifier: MIT

//! Persistent settings and lightweight navigation mementos (never GTK objects).
use gio::prelude::*;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::mpsc,
};

use crate::files::ViewMode;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "uri", rename_all = "snake_case")]
pub enum Location {
    Directory(String),
    Applications,
    Search { root: String, query: String },
    Tag(i64),
}

impl Location {
    pub fn directory(file: &gio::File) -> Self {
        Self::Directory(file.uri().to_string())
    }
    pub fn file(&self) -> Option<gio::File> {
        match self {
            Self::Directory(uri) => {
                let file = gio::File::for_uri(uri);
                crate::files::browsable(&file).then_some(file)
            }
            _ => None,
        }
    }
    pub fn valid(&self) -> bool {
        match self {
            Self::Applications | Self::Tag(_) => true,
            Self::Search { root, .. } => crate::files::safe_uri(root).is_ok(),
            _ => self.file().is_some(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FolderSettings {
    pub mode: ViewMode,
    pub hidden: bool,
    pub sort_field: usize,
    pub descending: bool,
    pub folders_first: bool,
    pub icon_size: i32,
    pub thumbnails: bool,
    pub visible_columns: [bool; 4],
    pub column_widths: [i32; 4],
    pub column_pane_width: i32,
}
impl Default for FolderSettings {
    fn default() -> Self {
        Self {
            mode: ViewMode::Icons,
            hidden: false,
            sort_field: 0,
            descending: false,
            folders_first: true,
            icon_size: 64,
            thumbnails: true,
            visible_columns: [true; 4],
            column_widths: [-1; 4],
            column_pane_width: 260,
        }
    }
}
impl FolderSettings {
    pub fn normalize(&mut self) {
        self.sort_field = self.sort_field.min(3);
        self.icon_size = self.icon_size.clamp(32, 128);
        self.visible_columns[0] = true;
        self.column_pane_width = self.column_pane_width.clamp(180, 600);
        for width in &mut self.column_widths {
            if *width != -1 {
                *width = (*width).clamp(60, 1200);
            }
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub version: u32,
    pub defaults: FolderSettings,
    pub folders: BTreeMap<String, FolderSettings>,
    pub theme: String,
    pub restore_tabs: bool,
    pub recent_servers: Vec<String>,
    pub sidebar_width: i32,
    pub quick_look_width: i32,
    pub quick_look_height: i32,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            version: 1,
            defaults: FolderSettings::default(),
            folders: BTreeMap::new(),
            theme: "system".into(),
            restore_tabs: true,
            recent_servers: Vec::new(),
            sidebar_width: 185,
            quick_look_width: 900,
            quick_look_height: 650,
        }
    }
}
impl Settings {
    pub fn for_file(&self, file: &gio::File) -> FolderSettings {
        self.folders
            .get(file.uri().as_str())
            .unwrap_or(&self.defaults)
            .clone()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ColumnState {
    pub uri: String,
    pub selected: Vec<String>,
    pub scroll: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NavEntry {
    pub location: Location,
    pub last_directory: String,
    pub settings: FolderSettings,
    pub columns: Vec<ColumnState>,
    pub active_column: usize,
    pub horizontal_scroll: f64,
}
impl NavEntry {
    pub fn new(location: Location, last_directory: String, settings: FolderSettings) -> Self {
        Self {
            location,
            last_directory,
            settings,
            columns: Vec::new(),
            active_column: 0,
            horizontal_scroll: 0.0,
        }
    }
}

#[derive(Clone, Default, Debug)]
pub struct NavigationHistory {
    entries: Vec<NavEntry>,
    position: usize,
}
impl NavigationHistory {
    pub fn visit(&mut self, entry: NavEntry) {
        if self
            .entries
            .get(self.position)
            .is_some_and(|e| e.location == entry.location)
        {
            self.entries[self.position] = entry;
            return;
        }
        if !self.entries.is_empty() {
            self.entries.truncate(self.position + 1);
        }
        self.entries.push(entry);
        if self.entries.len() > 200 {
            self.entries.remove(0);
        }
        self.position = self.entries.len() - 1;
    }
    pub fn replace_current(&mut self, entry: NavEntry) {
        if let Some(current) = self.entries.get_mut(self.position) {
            *current = entry;
        }
    }
    pub fn can_back(&self) -> bool {
        !self.entries.is_empty() && self.position > 0
    }
    pub fn can_forward(&self) -> bool {
        self.position + 1 < self.entries.len()
    }
    pub fn step(&mut self, forward: bool) -> Option<NavEntry> {
        if forward && self.can_forward() {
            self.position += 1;
        } else if !forward && self.can_back() {
            self.position -= 1;
        } else {
            return None;
        }
        self.entries.get(self.position).cloned()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Session {
    pub version: u32,
    pub tabs: Vec<NavEntry>,
    pub active: usize,
    pub width: i32,
    pub height: i32,
}
impl Default for Session {
    fn default() -> Self {
        Self {
            version: 1,
            tabs: Vec::new(),
            active: 0,
            width: 1100,
            height: 720,
        }
    }
}

enum Write {
    Data(PathBuf, Vec<u8>),
    Flush(mpsc::SyncSender<()>),
}

pub struct Storage {
    pub settings_path: PathBuf,
    pub session_path: PathBuf,
    sender: mpsc::Sender<Write>,
    #[cfg(test)]
    _test_directory: tempfile::TempDir,
}
impl Storage {
    pub fn new() -> (Self, async_channel::Receiver<String>) {
        #[cfg(not(test))]
        let config = glib::user_config_dir().join("ubuntu-finder");
        #[cfg(not(test))]
        let state = std::env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .unwrap_or_else(|| glib::home_dir().join(".local/state"))
            .join("ubuntu-finder");
        #[cfg(test)]
        let test_directory = tempfile::tempdir().expect("isolated test settings");
        #[cfg(test)]
        let (config, state) = (
            test_directory.path().join("config"),
            test_directory.path().join("state"),
        );
        let (sender, receiver) = mpsc::channel();
        let (errors, output) = async_channel::unbounded();
        std::thread::Builder::new()
            .name("settings-writer".into())
            .spawn(move || {
                for request in receiver {
                    match request {
                        Write::Data(path, bytes) => {
                            if let Err(error) = atomic_write(&path, &bytes) {
                                let _ =
                                    errors.send_blocking(format!("{}: {error}", path.display()));
                            }
                        }
                        Write::Flush(done) => {
                            let _ = done.send(());
                        }
                    }
                }
            })
            .expect("settings worker");
        (
            Self {
                settings_path: config.join("settings.json"),
                session_path: state.join("session.json"),
                sender,
                #[cfg(test)]
                _test_directory: test_directory,
            },
            output,
        )
    }
    pub fn read_settings(&self) -> (Settings, Option<String>) {
        let (mut settings, warning) = load::<Settings>(&self.settings_path);
        settings
            .recent_servers
            .retain(|uri| crate::files::safe_uri(uri).is_ok());
        settings.recent_servers.truncate(10);
        settings.quick_look_width = settings.quick_look_width.clamp(320, 4000);
        settings.quick_look_height = settings.quick_look_height.clamp(240, 2400);
        settings.defaults.normalize();
        for folder in settings.folders.values_mut() {
            folder.normalize();
        }
        if settings.version != 1 {
            return (
                Settings::default(),
                Some(self.settings_path.display().to_string()),
            );
        }
        (settings, warning)
    }
    pub fn read_session(&self) -> (Session, Option<String>) {
        let (mut session, warning) = load::<Session>(&self.session_path);
        if session.version != 1 {
            return (
                Session::default(),
                Some(self.session_path.display().to_string()),
            );
        }
        session.tabs.retain(|tab| tab.location.valid());
        for tab in &mut session.tabs {
            tab.settings.normalize();
        }
        session.active = session.active.min(session.tabs.len().saturating_sub(1));
        session.width = session.width.clamp(680, 4000);
        session.height = session.height.clamp(440, 2400);
        (session, warning)
    }
    pub fn save(&self, settings: &Settings, session: &Session) {
        for (path, data) in [
            (&self.settings_path, serde_json::to_vec_pretty(settings)),
            (&self.session_path, serde_json::to_vec_pretty(session)),
        ] {
            if let Ok(data) = data {
                let _ = self.sender.send(Write::Data(path.clone(), data));
            }
        }
    }
    pub fn flush(&self) {
        let (tx, rx) = mpsc::sync_channel(1);
        if self.sender.send(Write::Flush(tx)).is_ok() {
            let _ = rx.recv_timeout(std::time::Duration::from_secs(2));
        }
    }
}
fn load<T: serde::de::DeserializeOwned + Default>(path: &Path) -> (T, Option<String>) {
    if !path.exists() {
        return (T::default(), None);
    }
    let result = std::fs::metadata(path)
        .ok()
        .filter(|m| m.len() < 8 * 1024 * 1024)
        .and_then(|_| std::fs::read(path).ok())
        .filter(|v| v.len() < 8 * 1024 * 1024)
        .and_then(|v| serde_json::from_slice(&v).ok());
    match result {
        Some(value) => (value, None),
        None => (T::default(), Some(path.display().to_string())),
    }
}
fn atomic_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let parent = path.parent().ok_or(std::io::ErrorKind::InvalidInput)?;
    std::fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(
        ".{}.{}.tmp",
        path.file_name().unwrap_or_default().to_string_lossy(),
        std::process::id()
    ));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&temporary)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    std::fs::rename(temporary, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn entry(path: &str) -> NavEntry {
        NavEntry::new(
            Location::Directory(path.into()),
            path.into(),
            FolderSettings::default(),
        )
    }
    #[test]
    fn history_deduplicates_and_replaces_forward_branch() {
        let mut h = NavigationHistory::default();
        h.visit(entry("file:///a"));
        h.visit(entry("file:///a"));
        assert!(!h.can_back());
        h.visit(entry("file:///b"));
        assert!(h.can_back());
        assert_eq!(
            h.step(false).unwrap().location,
            Location::Directory("file:///a".into())
        );
        assert!(h.can_forward());
        h.visit(entry("file:///c"));
        assert!(!h.can_forward());
        assert_eq!(
            h.step(false).unwrap().location,
            Location::Directory("file:///a".into())
        );
    }
    #[test]
    fn corrupt_settings_fall_back_and_atomic_roundtrip() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("settings.json");
        std::fs::write(&path, "{broken").unwrap();
        assert!(load::<Settings>(&path).1.is_some());
        let legacy: Settings = serde_json::from_str(r#"{"version":1}"#).unwrap();
        assert_eq!(
            (legacy.quick_look_width, legacy.quick_look_height),
            (900, 650)
        );
        let mut settings = Settings {
            quick_look_width: 1120,
            quick_look_height: 780,
            ..Default::default()
        };
        settings.folders.insert(
            "file:///folder%FF".into(),
            FolderSettings {
                mode: ViewMode::Gallery,
                ..Default::default()
            },
        );
        atomic_write(&path, &serde_json::to_vec(&settings).unwrap()).unwrap();
        let (read, warning) = load::<Settings>(&path);
        assert!(warning.is_none());
        assert_eq!((read.quick_look_width, read.quick_look_height), (1120, 780));
        assert_eq!(read.folders["file:///folder%FF"].mode, ViewMode::Gallery);
    }
}
