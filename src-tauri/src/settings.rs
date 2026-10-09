use std::{
    collections::{HashMap, HashSet},
    fs,
    path::PathBuf,
    sync::Mutex,
};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, Runtime};

use crate::{
    cleanup::{anthropic, openai_compat, Provider},
    hotkey,
};

/// Keychain service for every secret this app stores.
const KEYCHAIN_SERVICE: &str = "io.github.iniandrew.yaptic";

pub const LANGUAGES: [&str; 3] = ["auto", "id", "en"];
pub const MIN_PASTE_DELAY_MS: u64 = 50;
pub const MAX_PASTE_DELAY_MS: u64 = 5000;

/// User settings, stored as JSON in the app config dir. Secrets never go
/// here; API keys live in the Keychain.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Push-to-talk shortcut, e.g. "Alt+Space".
    pub hotkey: String,
    /// Speech language: "auto", "id", or "en".
    pub language: String,
    /// Terms whose spelling transcription and cleanup should keep.
    pub dictionary: Vec<String>,
    /// Use a different whisper model file instead of the downloaded one.
    pub model_path: Option<PathBuf>,
    /// Optional LLM cleanup. API keys are in the Keychain, never here.
    pub cleanup: CleanupSettings,
    /// How long to wait after pasting before restoring the previous
    /// clipboard. Raise it if an app pastes the old clipboard instead.
    pub paste_restore_delay_ms: u64,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            hotkey: hotkey::DEFAULT_HOTKEY.to_string(),
            language: "auto".to_string(),
            dictionary: Vec::new(),
            model_path: None,
            cleanup: CleanupSettings::default(),
            paste_restore_delay_ms: 300,
        }
    }
}

/// The settings the General/Dictation parts of the UI edit (everything but
/// cleanup, which has its own commands).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneralSettings {
    pub hotkey: String,
    pub language: String,
    pub dictionary: Vec<String>,
    pub model_path: Option<PathBuf>,
    pub paste_restore_delay_ms: u64,
}

impl Settings {
    pub fn general(&self) -> GeneralSettings {
        GeneralSettings {
            hotkey: self.hotkey.clone(),
            language: self.language.clone(),
            dictionary: self.dictionary.clone(),
            model_path: self.model_path.clone(),
            paste_restore_delay_ms: self.paste_restore_delay_ms,
        }
    }

    pub fn set_general(&mut self, general: GeneralSettings) {
        self.hotkey = general.hotkey;
        self.language = general.language;
        self.dictionary = general.dictionary;
        self.model_path = general.model_path;
        self.paste_restore_delay_ms = general.paste_restore_delay_ms;
    }
}

impl GeneralSettings {
    /// Checks and normalizes general settings coming from the UI.
    pub fn validated(mut self) -> Result<Self, String> {
        if !LANGUAGES.contains(&self.language.as_str()) {
            return Err(format!("unsupported language \"{}\"", self.language));
        }
        hotkey::parse(&self.hotkey)?;

        let mut seen = HashSet::new();
        self.dictionary = self
            .dictionary
            .iter()
            .map(|term| term.trim().to_string())
            .filter(|term| !term.is_empty() && seen.insert(term.to_lowercase()))
            .collect();
        self.paste_restore_delay_ms = self
            .paste_restore_delay_ms
            .clamp(MIN_PASTE_DELAY_MS, MAX_PASTE_DELAY_MS);
        self.model_path = self.model_path.filter(|p| !p.as_os_str().is_empty());
        Ok(self)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(from = "CleanupSettingsFile")]
pub struct CleanupSettings {
    /// Off until the user turns it on; the raw transcript is pasted.
    pub enabled: bool,
    pub provider: Provider,
    pub anthropic: ProviderSettings,
    pub openai_compatible: ProviderSettings,
}

impl Default for CleanupSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            provider: Provider::Anthropic,
            anthropic: ProviderSettings {
                base_url: anthropic::DEFAULT_BASE_URL.to_string(),
                model: anthropic::DEFAULT_MODEL.to_string(),
                timeout_ms: DEFAULT_TIMEOUT_MS,
            },
            openai_compatible: ProviderSettings {
                base_url: openai_compat::DEFAULT_BASE_URL.to_string(),
                // No default: model names go stale. The UI shows a placeholder.
                model: String::new(),
                timeout_ms: DEFAULT_TIMEOUT_MS,
            },
        }
    }
}

/// On-disk shape, accepting files written before the enable toggle existed
/// (`"provider": "none"` meant off).
#[derive(Deserialize, Default)]
#[serde(default)]
struct CleanupSettingsFile {
    enabled: Option<bool>,
    provider: Option<String>,
    anthropic: Option<ProviderSettings>,
    openai_compatible: Option<ProviderSettings>,
}

impl From<CleanupSettingsFile> for CleanupSettings {
    fn from(file: CleanupSettingsFile) -> Self {
        let defaults = Self::default();
        let provider = match file.provider.as_deref() {
            Some("openai_compatible") => Some(Provider::OpenaiCompatible),
            Some("anthropic") => Some(Provider::Anthropic),
            _ => None,
        };
        Self {
            enabled: file.enabled.unwrap_or(provider.is_some()),
            provider: provider.unwrap_or(defaults.provider),
            anthropic: file.anthropic.unwrap_or(defaults.anthropic),
            openai_compatible: file.openai_compatible.unwrap_or(defaults.openai_compatible),
        }
    }
}

impl CleanupSettings {
    pub fn for_provider(&self, provider: Provider) -> &ProviderSettings {
        match provider {
            Provider::Anthropic => &self.anthropic,
            Provider::OpenaiCompatible => &self.openai_compatible,
        }
    }

    /// The active provider and its settings, if cleanup is on.
    pub fn active(&self) -> Option<(Provider, &ProviderSettings)> {
        self.enabled
            .then(|| (self.provider, self.for_provider(self.provider)))
    }

    /// Checks and normalizes cleanup settings coming from the UI.
    pub fn validated(mut self) -> Result<Self, String> {
        for (provider, cfg) in [
            (Provider::Anthropic, &mut self.anthropic),
            (Provider::OpenaiCompatible, &mut self.openai_compatible),
        ] {
            cfg.base_url = cfg.base_url.trim().to_string();
            cfg.model = cfg.model.trim().to_string();
            cfg.timeout_ms = cfg.timeout_ms.clamp(MIN_TIMEOUT_MS, MAX_TIMEOUT_MS);
            crate::cleanup::endpoint_for(provider, cfg)?;
        }
        Ok(self)
    }
}

pub const DEFAULT_TIMEOUT_MS: u64 = 4000;
pub const MIN_TIMEOUT_MS: u64 = 1000;
/// Local models can be slow; allow up to a minute.
pub const MAX_TIMEOUT_MS: u64 = 60_000;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ProviderSettings {
    pub base_url: String,
    pub model: String,
    pub timeout_ms: u64,
}

impl Settings {
    pub fn path<R: Runtime>(app: &AppHandle<R>) -> Option<PathBuf> {
        app.path()
            .app_config_dir()
            .ok()
            .map(|dir| dir.join("settings.json"))
    }

    /// Reads settings, writing the file back if it is missing or lacks newer
    /// fields so every option is visible. Falls back to defaults on any
    /// error so dictation never breaks over a bad settings file.
    pub fn load_or_init<R: Runtime>(app: &AppHandle<R>) -> Self {
        let Some(path) = Self::path(app) else {
            return Self::default();
        };
        let existing = fs::read_to_string(&path).ok();
        let settings = match &existing {
            Some(json) => match serde_json::from_str::<Self>(json) {
                Ok(settings) => settings,
                Err(e) => {
                    eprintln!("[settings] invalid {}: {e}", path.display());
                    return Self::default();
                }
            },
            None => Self::default(),
        };
        if existing.as_deref() != Some(settings.to_json().as_str()) {
            if let Err(e) = settings.write(&path) {
                eprintln!("[settings] write {}: {e}", path.display());
            }
        }
        settings
    }

    pub fn save<R: Runtime>(&self, app: &AppHandle<R>) -> Result<(), String> {
        let path = Self::path(app).ok_or("Could not resolve the app config directory")?;
        self.write(&path)
            .map_err(|e| format!("write {}: {e}", path.display()))
    }

    fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("settings serialize") + "\n"
    }

    fn write(&self, path: &PathBuf) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        fs::write(path, self.to_json())
    }
}

/// API keys in the macOS Keychain, one entry per provider. Values are cached
/// after the first read: unsigned dev builds get a Keychain prompt on every
/// read after a rebuild. Keys are never logged or written to settings.
#[derive(Default)]
pub struct ApiKeyStore {
    /// Missing entry = Keychain not read yet for that provider.
    cache: Mutex<HashMap<Provider, Option<String>>>,
}

impl ApiKeyStore {
    pub fn get(&self, provider: Provider) -> Option<String> {
        self.cache
            .lock()
            .unwrap()
            .entry(provider)
            .or_insert_with(|| read_keychain(provider))
            .clone()
    }

    /// Saves the key to the Keychain; an empty key removes it.
    pub fn set(&self, provider: Provider, key: &str) -> Result<(), String> {
        let key = key.trim();
        let entry = keychain_entry(provider)?;
        if key.is_empty() {
            match entry.delete_credential() {
                Ok(()) | Err(keyring::Error::NoEntry) => {}
                Err(e) => return Err(format!("remove key from Keychain: {e}")),
            }
            self.cache.lock().unwrap().insert(provider, None);
        } else {
            entry
                .set_password(key)
                .map_err(|e| format!("save key to Keychain: {e}"))?;
            self.cache
                .lock()
                .unwrap()
                .insert(provider, Some(key.to_string()));
        }
        Ok(())
    }
}

fn keychain_entry(provider: Provider) -> Result<keyring::Entry, String> {
    keyring::Entry::new(KEYCHAIN_SERVICE, provider.keychain_account())
        .map_err(|e| format!("Keychain: {e}"))
}

fn read_keychain(provider: Provider) -> Option<String> {
    match keychain_entry(provider).ok()?.get_password() {
        Ok(key) if !key.trim().is_empty() => Some(key),
        Ok(_) | Err(keyring::Error::NoEntry) => None,
        Err(e) => {
            eprintln!("[settings] read {} key from Keychain: {e}", provider.keychain_account());
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrates_files_from_before_the_enable_toggle() {
        let off: Settings = serde_json::from_str(r#"{"cleanup":{"provider":"none"}}"#).unwrap();
        assert!(!off.cleanup.enabled);
        assert_eq!(off.cleanup.provider, Provider::Anthropic);

        let on: Settings = serde_json::from_str(
            r#"{"cleanup":{"provider":"openai_compatible",
                "openai_compatible":{"base_url":"http://localhost:11434/v1","model":"llama3.2","timeout_ms":8000}}}"#,
        )
        .unwrap();
        assert!(on.cleanup.enabled);
        assert_eq!(on.cleanup.provider, Provider::OpenaiCompatible);
        assert_eq!(on.cleanup.openai_compatible.model, "llama3.2");

        let explicit: Settings =
            serde_json::from_str(r#"{"cleanup":{"enabled":false,"provider":"anthropic"}}"#).unwrap();
        assert!(!explicit.cleanup.enabled);
    }

    #[test]
    fn round_trips_through_json() {
        let mut settings = Settings::default();
        settings.cleanup.enabled = true;
        settings.cleanup.provider = Provider::OpenaiCompatible;
        let back: Settings = serde_json::from_str(&settings.to_json()).unwrap();
        assert!(back.cleanup.enabled);
        assert_eq!(back.cleanup.provider, Provider::OpenaiCompatible);
        assert_eq!(back.hotkey, hotkey::DEFAULT_HOTKEY);
    }

    #[test]
    fn general_validation_normalizes_ui_input() {
        let general = GeneralSettings {
            hotkey: "Alt+Space".into(),
            language: "id".into(),
            dictionary: vec![" Tauri ".into(), "".into(), "tauri".into(), "Haiku".into()],
            model_path: Some(PathBuf::new()),
            paste_restore_delay_ms: 10,
        }
        .validated()
        .unwrap();
        assert_eq!(general.dictionary, ["Tauri", "Haiku"]);
        assert_eq!(general.paste_restore_delay_ms, MIN_PASTE_DELAY_MS);
        assert_eq!(general.model_path, None);
    }

    #[test]
    fn general_validation_rejects_bad_language_and_hotkey() {
        let base = Settings::default().general();
        let bad_language = GeneralSettings { language: "fr".into(), ..base.clone() };
        assert!(bad_language.validated().is_err());
        let bad_hotkey = GeneralSettings { hotkey: "Space".into(), ..base };
        assert!(bad_hotkey.validated().is_err());
    }
}
