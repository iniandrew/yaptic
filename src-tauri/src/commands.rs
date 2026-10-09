use std::time::Instant;

use serde::Serialize;
use tauri::{AppHandle, Manager, Runtime, State};

use crate::{
    audio,
    cleanup::{self, CleanupContext, HttpClient, Outcome, Provider},
    hotkey, login,
    model::{self, ModelManager, ModelStatus},
    permissions::{self, MicrophoneStatus},
    settings::{ApiKeyStore, CleanupSettings, GeneralSettings, ProviderSettings, Settings},
};

// ------------------------------------------------------------------ general

#[tauri::command]
pub fn get_general(app: AppHandle) -> GeneralSettings {
    Settings::load_or_init(&app).general()
}

/// Validates and saves the non-cleanup settings, applying side effects:
/// re-registers a changed shortcut and reloads the model if its path changed.
#[tauri::command]
pub fn save_general(app: AppHandle, general: GeneralSettings) -> Result<GeneralSettings, String> {
    let mut settings = Settings::load_or_init(&app);
    let old = settings.general();
    let new = general.validated()?;

    let hotkey_changed = new.hotkey != old.hotkey;
    if hotkey_changed {
        if let Err(e) = hotkey::register(&app, &new.hotkey) {
            let _ = hotkey::register(&app, &old.hotkey);
            return Err(e);
        }
    }
    let model_changed = new.model_path != old.model_path;
    settings.set_general(new);
    if let Err(e) = settings.save(&app) {
        if hotkey_changed {
            let _ = hotkey::register(&app, &old.hotkey);
        }
        return Err(e);
    }
    if model_changed {
        model::init(&app);
    }
    Ok(settings.general())
}

/// Unregisters the shortcut while the settings window captures a new one.
#[tauri::command]
pub fn suspend_hotkey(app: AppHandle) {
    hotkey::suspend(&app);
}

/// Re-registers the saved shortcut (after capture ends, saved or not).
#[tauri::command]
pub fn resume_hotkey(app: AppHandle) -> Result<(), String> {
    hotkey::register(&app, &Settings::load_or_init(&app).hotkey)
}

#[derive(Serialize)]
pub struct AppPaths {
    settings: Option<String>,
    default_model: Option<String>,
}

#[tauri::command]
pub fn app_paths(app: AppHandle) -> AppPaths {
    let display = |p: std::path::PathBuf| p.display().to_string();
    AppPaths {
        settings: Settings::path(&app).map(display),
        default_model: model::default_path(&app).map(display),
    }
}

#[tauri::command]
pub fn launch_at_login() -> bool {
    login::is_enabled()
}

#[tauri::command]
pub fn set_launch_at_login(enabled: bool) -> Result<bool, String> {
    login::set_enabled(enabled)?;
    Ok(login::is_enabled())
}

// -------------------------------------------------------------- permissions

#[derive(Serialize)]
pub struct PermissionStatus {
    microphone: MicrophoneStatus,
    accessibility: bool,
}

#[tauri::command]
pub fn permission_status() -> PermissionStatus {
    PermissionStatus {
        microphone: permissions::microphone_status(),
        accessibility: permissions::accessibility_trusted(),
    }
}

/// Asks for mic access if never asked, otherwise opens System Settings.
#[tauri::command]
pub fn fix_microphone() {
    match permissions::microphone_status() {
        MicrophoneStatus::Undetermined => audio::request_permission(),
        _ => permissions::open_microphone_settings(),
    }
}

#[tauri::command]
pub fn fix_accessibility() {
    permissions::open_accessibility_settings();
}

// -------------------------------------------------------------------- model

#[tauri::command]
pub fn model_status(models: State<'_, ModelManager>) -> ModelStatus {
    models.status()
}

/// Starts or resumes the model download; progress arrives as `model-status` events.
#[tauri::command]
pub fn download_model(app: AppHandle) -> Result<(), String> {
    model::start_download(&app)
}

#[tauri::command]
pub fn cancel_model_download(app: AppHandle) {
    model::cancel_download(&app);
}

// ------------------------------------------------------------------ cleanup

/// Cleanup settings plus which providers have a key. Keys themselves never
/// leave the Rust side.
#[derive(Serialize)]
pub struct CleanupView {
    settings: CleanupSettings,
    has_key: KeyStatus,
}

#[derive(Serialize)]
pub struct KeyStatus {
    anthropic: bool,
    openai_compatible: bool,
}

fn cleanup_view(settings: CleanupSettings, keys: &ApiKeyStore) -> CleanupView {
    CleanupView {
        settings,
        has_key: KeyStatus {
            anthropic: keys.get(Provider::Anthropic).is_some(),
            openai_compatible: keys.get(Provider::OpenaiCompatible).is_some(),
        },
    }
}

#[tauri::command]
pub fn get_cleanup(app: AppHandle, keys: State<'_, ApiKeyStore>) -> CleanupView {
    cleanup_view(Settings::load_or_init(&app).cleanup, &keys)
}

#[tauri::command]
pub fn save_cleanup(
    app: AppHandle,
    keys: State<'_, ApiKeyStore>,
    cleanup: CleanupSettings,
) -> Result<CleanupView, String> {
    let mut settings = Settings::load_or_init(&app);
    settings.cleanup = cleanup.validated()?;
    settings.save(&app)?;
    Ok(cleanup_view(settings.cleanup, &keys))
}

/// Saves a provider's API key in the Keychain (empty removes it).
#[tauri::command]
pub fn set_cleanup_key(
    app: AppHandle,
    keys: State<'_, ApiKeyStore>,
    provider: Provider,
    key: String,
) -> Result<CleanupView, String> {
    keys.set(provider, &key)?;
    Ok(cleanup_view(Settings::load_or_init(&app).cleanup, &keys))
}

#[derive(Serialize)]
pub struct EndpointInfo {
    endpoint: Option<String>,
    error: Option<String>,
    warning: Option<String>,
}

/// Shows where requests would go, and warns about plain http:// to a
/// remote host.
#[tauri::command]
pub fn inspect_base_url(provider: Provider, config: ProviderSettings) -> EndpointInfo {
    match cleanup::endpoint_for(provider, &config) {
        Ok(url) => EndpointInfo {
            warning: cleanup::insecure_http_warning(&url),
            endpoint: Some(url.to_string()),
            error: None,
        },
        Err(e) => EndpointInfo {
            endpoint: None,
            error: Some(e),
            warning: None,
        },
    }
}

#[derive(Serialize)]
pub struct ConnectionTest {
    latency_ms: u64,
    output: String,
}

/// Sends a tiny fixed transcript with the given (possibly unsaved) settings
/// and the saved key, returning the model's answer or the real error.
#[tauri::command]
pub async fn test_cleanup_connection(
    app: AppHandle,
    provider: Provider,
    config: ProviderSettings,
) -> Result<ConnectionTest, String> {
    let key = app.state::<ApiKeyStore>().get(provider);
    let client = app.state::<HttpClient>().0.clone();
    let cleaner = cleanup::build(provider, &config, key, &client)?;

    let ctx = CleanupContext { dictionary: &[], language: "auto" };
    let started = Instant::now();
    let output = cleaner
        .clean("um, so, testing one two three", &ctx)
        .await
        .map_err(|e| e.to_string())?;
    Ok(ConnectionTest {
        latency_ms: started.elapsed().as_millis() as u64,
        output,
    })
}

/// Runs typed text through the saved cleanup settings, with the same
/// fallback the pipeline uses.
#[tauri::command]
pub async fn try_cleanup(app: AppHandle, text: String) -> Result<Outcome, String> {
    let settings = Settings::load_or_init(&app);
    let cleaner = active_cleaner(&app, &settings);
    let ctx = CleanupContext {
        dictionary: &settings.dictionary,
        language: &settings.language,
    };
    Ok(cleanup::clean_or_raw(cleaner, &text, &ctx).await)
}

/// The cleaner for the saved settings: `Ok(None)` when cleanup is off,
/// `Err` when it is on but unusable (see [`cleanup::build`]).
pub fn active_cleaner<R: Runtime>(
    app: &AppHandle<R>,
    settings: &Settings,
) -> Result<Option<Box<dyn cleanup::Cleaner>>, String> {
    let Some((provider, cfg)) = settings.cleanup.active() else {
        return Ok(None);
    };
    let key = app.state::<ApiKeyStore>().get(provider);
    cleanup::build(provider, cfg, key, &app.state::<HttpClient>().0).map(Some)
}
