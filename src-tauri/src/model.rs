use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

use reqwest::{header::RANGE, StatusCode};
use serde::Serialize;
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Emitter, Manager, Runtime};

use crate::{
    settings::Settings,
    stt::{Stt, WhisperTranscriber},
};

pub const MODEL_FILE: &str = "ggml-large-v3-turbo-q5_0.bin";
const MODEL_URL: &str =
    "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo-q5_0.bin";
/// Exact size and SHA-256 of the published file, used to verify the
/// download (the hash is Hugging Face's LFS object id for this file).
const MODEL_SIZE: u64 = 574_041_195;
const MODEL_SHA256: &str = "394221709cd5ad1f40c46e6031ca61bce88931e6e088c188294c6d5a55ffa7e2";
const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);

pub const STATUS_EVENT: &str = "model-status";

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ModelStatus {
    /// Not on disk. `downloaded` > 0 means a partial download can resume.
    Missing { downloaded: u64, total: u64 },
    Downloading { downloaded: u64, total: u64 },
    /// Checking the finished download's SHA-256.
    Verifying,
    Loading,
    Ready,
    Error { message: String },
}

pub struct ModelManager {
    status: Mutex<ModelStatus>,
    downloading: AtomicBool,
    cancel: AtomicBool,
}

impl ModelManager {
    pub fn new() -> Self {
        Self {
            status: Mutex::new(ModelStatus::Loading),
            downloading: AtomicBool::new(false),
            cancel: AtomicBool::new(false),
        }
    }

    pub fn status(&self) -> ModelStatus {
        self.status.lock().unwrap().clone()
    }

    fn set_status<R: Runtime>(&self, app: &AppHandle<R>, status: ModelStatus) {
        *self.status.lock().unwrap() = status.clone();
        let _ = app.emit(STATUS_EVENT, status);
    }
}

enum DownloadError {
    Cancelled,
    Failed(String),
}

impl From<std::io::Error> for DownloadError {
    fn from(e: std::io::Error) -> Self {
        Self::Failed(format!("write model file: {e}"))
    }
}

/// Where the default model downloads to.
pub fn default_path<R: Runtime>(app: &AppHandle<R>) -> Option<PathBuf> {
    app.path()
        .app_data_dir()
        .ok()
        .map(|dir| dir.join("models").join(MODEL_FILE))
}

fn part_path(dest: &Path) -> PathBuf {
    dest.with_extension("bin.part")
}

fn partial_len(dest: &Path) -> u64 {
    fs::metadata(part_path(dest)).map(|m| m.len()).unwrap_or(0)
}

/// Checks the model on disk and loads it, or reports it missing.
/// Returns false if the user needs to act (download or fix the path).
pub fn init<R: Runtime>(app: &AppHandle<R>) -> bool {
    let manager = app.state::<ModelManager>();
    // Drop any loaded engine so a bad path can't silently keep the old one.
    app.state::<Stt>().set(None);

    if let Some(custom) = Settings::load_or_init(app).model_path {
        if custom.is_file() {
            load_in_background(app, custom);
            return true;
        }
        let message = format!("Model not found at {} (model_path in settings)", custom.display());
        manager.set_status(app, ModelStatus::Error { message });
        return false;
    }

    let Some(dest) = default_path(app) else {
        let message = "Could not resolve the app data directory".to_string();
        manager.set_status(app, ModelStatus::Error { message });
        return false;
    };

    match fs::metadata(&dest) {
        Ok(meta) if meta.len() == MODEL_SIZE => {
            load_in_background(app, dest);
            true
        }
        _ => {
            let status = ModelStatus::Missing {
                downloaded: partial_len(&dest),
                total: MODEL_SIZE,
            };
            manager.set_status(app, status);
            false
        }
    }
}

fn load_in_background<R: Runtime>(app: &AppHandle<R>, path: PathBuf) {
    app.state::<ModelManager>()
        .set_status(app, ModelStatus::Loading);
    let app = app.clone();
    thread::spawn(move || {
        let started = Instant::now();
        let status = match WhisperTranscriber::load(&path) {
            Ok(engine) => {
                app.state::<Stt>().set(Some(Arc::new(engine)));
                println!("[model] loaded {} in {:?}", path.display(), started.elapsed());
                ModelStatus::Ready
            }
            Err(message) => {
                eprintln!("[model] {message}");
                ModelStatus::Error { message }
            }
        };
        app.state::<ModelManager>().set_status(&app, status);
    });
}

/// Starts (or resumes) the download in the background. No-op if one is running.
pub fn start_download<R: Runtime>(app: &AppHandle<R>) -> Result<(), String> {
    let manager = app.state::<ModelManager>();
    if Settings::load_or_init(app).model_path.is_some() {
        return Err("A custom model_path is set; remove it to download the default model".into());
    }
    let dest = default_path(app).ok_or("Could not resolve the app data directory")?;
    if manager.downloading.swap(true, Ordering::SeqCst) {
        return Ok(());
    }
    manager.cancel.store(false, Ordering::SeqCst);

    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let result = download(&app, &dest).await;
        let manager = app.state::<ModelManager>();
        manager.downloading.store(false, Ordering::SeqCst);
        match result {
            Ok(()) => load_in_background(&app, dest),
            Err(DownloadError::Cancelled) => {
                let status = ModelStatus::Missing {
                    downloaded: partial_len(&dest),
                    total: MODEL_SIZE,
                };
                manager.set_status(&app, status);
            }
            Err(DownloadError::Failed(message)) => {
                eprintln!("[model] download: {message}");
                manager.set_status(&app, ModelStatus::Error { message });
            }
        }
    });
    Ok(())
}

pub fn cancel_download<R: Runtime>(app: &AppHandle<R>) {
    app.state::<ModelManager>()
        .cancel
        .store(true, Ordering::SeqCst);
}

/// Streams the model to `<dest>.part`, resuming with an HTTP Range request
/// if a partial file exists, then verifies the size and renames it into place.
async fn download<R: Runtime>(app: &AppHandle<R>, dest: &Path) -> Result<(), DownloadError> {
    let manager = app.state::<ModelManager>();
    let part = part_path(dest);
    if let Some(dir) = dest.parent() {
        fs::create_dir_all(dir)?;
    }

    let mut offset = partial_len(dest);
    if offset > MODEL_SIZE {
        fs::remove_file(&part)?;
        offset = 0;
    }

    if offset < MODEL_SIZE {
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(15))
            .read_timeout(Duration::from_secs(30))
            .build()
            .map_err(|e| DownloadError::Failed(format!("HTTP client: {e}")))?;
        let mut request = client.get(MODEL_URL);
        if offset > 0 {
            request = request.header(RANGE, format!("bytes={offset}-"));
        }
        let mut response = request
            .send()
            .await
            .map_err(|e| DownloadError::Failed(format!("connect: {e}")))?;

        let mut file = match response.status() {
            StatusCode::PARTIAL_CONTENT => OpenOptions::new().append(true).open(&part)?,
            // Server ignored the Range header: start over.
            StatusCode::OK => {
                offset = 0;
                File::create(&part)?
            }
            status => {
                return Err(DownloadError::Failed(format!("download failed: HTTP {status}")))
            }
        };

        let mut downloaded = offset;
        let mut last_emit = Instant::now() - PROGRESS_INTERVAL;
        loop {
            if manager.cancel.load(Ordering::SeqCst) {
                return Err(DownloadError::Cancelled);
            }
            let chunk = response
                .chunk()
                .await
                .map_err(|e| DownloadError::Failed(format!("download interrupted: {e}")))?;
            let Some(chunk) = chunk else { break };
            file.write_all(&chunk)?;
            downloaded += chunk.len() as u64;

            if last_emit.elapsed() >= PROGRESS_INTERVAL {
                last_emit = Instant::now();
                let status = ModelStatus::Downloading {
                    downloaded,
                    total: MODEL_SIZE,
                };
                manager.set_status(app, status);
            }
        }
        file.flush()?;
    }

    let size = fs::metadata(&part)?.len();
    if size != MODEL_SIZE {
        if size > MODEL_SIZE {
            fs::remove_file(&part)?;
        }
        return Err(DownloadError::Failed(format!(
            "download incomplete ({size} of {MODEL_SIZE} bytes); retry to resume"
        )));
    }

    manager.set_status(app, ModelStatus::Verifying);
    let hash_path = part.clone();
    let verify_error = |e: &dyn std::fmt::Display| DownloadError::Failed(format!("verify download: {e}"));
    let digest = tauri::async_runtime::spawn_blocking(move || sha256_file(&hash_path))
        .await
        .map_err(|e| verify_error(&e))?
        .map_err(|e| verify_error(&e))?;
    if digest != MODEL_SHA256 {
        // Wrong bytes of the right length: corrupt or tampered. Start over.
        fs::remove_file(&part)?;
        return Err(DownloadError::Failed(
            "checksum mismatch: the downloaded model was not the expected file and was deleted; retry the download".into(),
        ));
    }
    fs::rename(&part, dest)?;
    Ok(())
}

/// Lowercase hex SHA-256 of a file, read in 1 MiB chunks.
fn sha256_file(path: &Path) -> io::Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0; 1 << 20];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_matches_known_vector() {
        let path = std::env::temp_dir().join("yaptic-sha-test.bin");
        fs::write(&path, b"abc").unwrap();
        assert_eq!(
            sha256_file(&path).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        let _ = fs::remove_file(path);
    }

    #[test]
    fn pinned_hash_is_well_formed() {
        assert_eq!(MODEL_SHA256.len(), 64);
        assert!(MODEL_SHA256.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    }
}
