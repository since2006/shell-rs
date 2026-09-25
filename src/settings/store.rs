use std::{
    fs, io,
    path::{Path, PathBuf},
};

use gpui_kit::*;

use super::AppSettings;

#[derive(Clone, Debug)]
pub enum SettingsStoreEvent {
    /// A change stayed in memory but could not be written.
    PersistFailed(SharedString),
}

/// The settings, shared by the settings page and the workspace that applies
/// them. Like `SessionStore`, memory is the source of truth and every change
/// is written through to disk at once; a failed write keeps the change and
/// says so.
pub struct SettingsStore {
    settings: AppSettings,
    /// `None` keeps everything in memory.
    path: Option<PathBuf>,
}

impl EventEmitter<SettingsStoreEvent> for SettingsStore {}

impl SettingsStore {
    /// Settings that are never written anywhere: for UI tests, and for a
    /// machine without a data directory.
    pub fn in_memory() -> Self {
        Self {
            settings: AppSettings::default(),
            path: None,
        }
    }

    /// Read the settings saved at `path`. A missing file means the defaults.
    /// An unreadable one also gives the defaults, together with the message
    /// to show; the file stays as it is until the next change replaces it.
    pub fn load(path: PathBuf) -> (Self, Option<SharedString>) {
        let loaded = match fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|error| error.to_string()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(AppSettings::default()),
            Err(error) => Err(error.to_string()),
        };
        let (settings, problem) = match loaded {
            Ok(settings) => (AppSettings::normalized(settings), None),
            Err(error) => (
                AppSettings::default(),
                Some(format!("无法读取设置，本次使用默认设置：{error}").into()),
            ),
        };
        (
            Self {
                settings,
                path: Some(path),
            },
            problem,
        )
    }

    pub fn settings(&self) -> AppSettings {
        self.settings.clone()
    }

    /// Change the settings, write them and tell observers.
    pub fn update(&mut self, change: impl FnOnce(&mut AppSettings), cx: &mut Context<Self>) {
        let Some(written) = self.update_unnotified(change) else {
            return;
        };
        cx.notify();
        if let Err(error) = written {
            cx.emit(SettingsStoreEvent::PersistFailed(
                format!("无法保存设置：{error}").into(),
            ));
        }
    }

    /// The in-memory half of `update`: `None` when the change changed
    /// nothing, otherwise how writing it went. Memory keeps the change
    /// either way.
    pub fn update_unnotified(
        &mut self,
        change: impl FnOnce(&mut AppSettings),
    ) -> Option<io::Result<()>> {
        let before = self.settings.clone();
        change(&mut self.settings);
        self.settings = self.settings.clone().normalized();
        if self.settings == before {
            return None;
        }
        Some(match &self.path {
            Some(path) => write(path, &self.settings),
            None => Ok(()),
        })
    }
}

/// Replace the file in one step, so a crash mid-write cannot leave half a
/// file behind.
fn write(path: &Path, settings: &AppSettings) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let json = serde_json::to_vec_pretty(settings).map_err(io::Error::other)?;
    let temporary = path.with_extension("json.tmp");
    fs::write(&temporary, json)?;
    fs::rename(&temporary, path)
}

#[cfg(test)]
mod tests {
    // Not `super::*`: it brings in `gpui_kit::*`, whose `test` shadows `#[test]`.
    use std::fs;

    use super::SettingsStore;
    use crate::settings::{AppSettings, Appearance, InterfaceLanguage};

    #[test]
    fn a_first_start_has_the_defaults_and_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let (mut store, problem) = SettingsStore::load(path.clone());
        assert!(problem.is_none());
        assert_eq!(store.settings(), AppSettings::default());

        // Choosing what is already chosen is not a change.
        let default = AppSettings::default().appearance;
        assert!(
            store
                .update_unnotified(|settings| settings.appearance = default)
                .is_none()
        );
        assert!(!path.exists());
    }

    #[test]
    fn a_change_is_written_and_read_back() {
        let dir = tempfile::tempdir().unwrap();
        // The data directory may not exist yet.
        let path = dir.path().join("shellr").join("settings.json");
        let (mut store, _) = SettingsStore::load(path.clone());
        let written = store.update_unnotified(|settings| {
            settings.appearance = Appearance::Dark;
            settings.language = InterfaceLanguage::English;
        });
        assert!(matches!(written, Some(Ok(()))));

        let (reloaded, problem) = SettingsStore::load(path);
        assert!(problem.is_none());
        assert_eq!(reloaded.settings().appearance, Appearance::Dark);
        assert_eq!(reloaded.settings().language, InterfaceLanguage::English);
    }

    #[test]
    fn an_unreadable_file_falls_back_to_the_defaults_and_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        fs::write(&path, "{ not json").unwrap();
        let (store, problem) = SettingsStore::load(path.clone());
        assert_eq!(store.settings(), AppSettings::default());
        assert!(problem.unwrap().starts_with("无法读取设置"));
        // Left alone until the user changes something.
        assert_eq!(fs::read_to_string(&path).unwrap(), "{ not json");
    }

    #[test]
    fn a_failed_write_keeps_the_change_in_memory() {
        let dir = tempfile::tempdir().unwrap();
        // A directory where the file should be cannot be replaced by it.
        let path = dir.path().join("settings.json");
        fs::create_dir(&path).unwrap();
        fs::write(path.join("keep"), "").unwrap();
        let mut store = SettingsStore {
            settings: AppSettings::default(),
            path: Some(path),
        };
        let written = store.update_unnotified(|settings| settings.appearance = Appearance::Light);
        assert!(matches!(written, Some(Err(_))));
        assert_eq!(store.settings().appearance, Appearance::Light);
    }

    #[test]
    fn in_memory_settings_touch_no_file() {
        let mut store = SettingsStore::in_memory();
        let written = store.update_unnotified(|settings| settings.appearance = Appearance::Dark);
        assert!(matches!(written, Some(Ok(()))));
        assert_eq!(store.settings().appearance, Appearance::Dark);
    }
}
