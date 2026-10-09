use std::{
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Mutex,
    },
    thread,
    time::{Duration, Instant},
};

use tauri::{AppHandle, Manager, Runtime};

use crate::{
    audio,
    cleanup::{self, CleanupContext},
    commands,
    model::{ModelManager, ModelStatus},
    paste,
    permissions::{self, MicrophoneStatus},
    pill::{self, PillState},
    settings::Settings,
    stt::{Stt, TranscribeOpts},
    tray,
};

/// Debug builds dump the last recording here (owner-only, see `write_wav`).
#[cfg(debug_assertions)]
const DEBUG_WAV_PATH: &str = "/tmp/yaptic-last.wav";
/// How often the pill's level meter updates.
const METER_INTERVAL: Duration = Duration::from_millis(50);
/// Recordings quieter than this are treated as silence and skipped, since
/// whisper hallucinates text ("Thank you.") on silent input.
const SILENCE_RMS: f32 = 0.003;
/// Shorter presses are accidental taps and are discarded.
const MIN_HOLD: Duration = Duration::from_millis(300);

/// Orchestrates record -> stt -> cleanup -> paste.
pub struct Pipeline {
    recorder: audio::Recorder,
    recording: AtomicBool,
    pressed_at: Mutex<Option<Instant>>,
    /// Id of the latest dictation, bumped on every press and cancel. The
    /// meter loop stops when it changes, and an older dictation still pastes
    /// but no longer updates the pill.
    job: AtomicU64,
}

impl Pipeline {
    pub fn new() -> Self {
        Self {
            recorder: audio::Recorder::new(),
            recording: AtomicBool::new(false),
            pressed_at: Mutex::new(None),
            job: AtomicU64::new(0),
        }
    }
}

pub fn on_press<R: Runtime>(app: &AppHandle<R>) {
    let pipeline = app.state::<Pipeline>();
    // Ignore key repeat while held.
    if pipeline.recording.swap(true, Ordering::SeqCst) {
        return;
    }
    let job = pipeline.job.fetch_add(1, Ordering::SeqCst) + 1;

    if let Some((message, open_settings)) = blocker(app) {
        pipeline.recording.store(false, Ordering::SeqCst);
        pill::set(app, PillState::error(message));
        if open_settings {
            tray::show_settings(app);
        }
        return;
    }

    *pipeline.pressed_at.lock().unwrap() = Some(Instant::now());
    pipeline.recorder.start();
    pill::set_level(app, 0.0);
    pill::set(app, PillState::Listening);

    let app = app.clone();
    thread::spawn(move || {
        let pipeline = app.state::<Pipeline>();
        while pipeline.recording.load(Ordering::SeqCst)
            && pipeline.job.load(Ordering::SeqCst) == job
        {
            pill::set_level(&app, pipeline.recorder.level());
            thread::sleep(METER_INTERVAL);
        }
    });
}

pub fn on_release<R: Runtime>(app: &AppHandle<R>) {
    let pipeline = app.state::<Pipeline>();
    if !pipeline.recording.swap(false, Ordering::SeqCst) {
        return;
    }
    let held = pipeline
        .pressed_at
        .lock()
        .unwrap()
        .take()
        .map(|t| t.elapsed())
        .unwrap_or_default();
    // Queue the stop now so it stays ordered before any later press.
    let pending = pipeline.recorder.stop();

    if held < MIN_HOLD {
        println!("[pipeline] tap ({} ms), ignored", held.as_millis());
        drop(pending);
        pill::hide(app);
        return;
    }

    let job = pipeline.job.load(Ordering::SeqCst);
    pill::set(app, PillState::Transcribing);
    let app = app.clone();
    thread::spawn(move || {
        if let Some(state) = process(&app, job, pending) {
            update(&app, job, state);
        }
    });
}

/// Stops a recording in progress without transcribing it (e.g. when the
/// shortcut is suspended mid-hold, so no release event will arrive).
pub fn cancel<R: Runtime>(app: &AppHandle<R>) {
    let pipeline = app.state::<Pipeline>();
    if pipeline.recording.swap(false, Ordering::SeqCst) {
        pipeline.pressed_at.lock().unwrap().take();
        pipeline.job.fetch_add(1, Ordering::SeqCst);
        // Dropping the pending recording discards it.
        drop(pipeline.recorder.stop());
        pill::hide(app);
    }
}

/// Why dictation can't start right now, and whether settings can fix it.
fn blocker<R: Runtime>(app: &AppHandle<R>) -> Option<(String, bool)> {
    match permissions::microphone_status() {
        MicrophoneStatus::Denied => {
            return Some(("Microphone access denied. Allow it in Settings.".into(), true))
        }
        MicrophoneStatus::Restricted => {
            return Some(("Microphone access is restricted on this Mac.".into(), false))
        }
        // Undetermined: starting the recording shows the system prompt.
        MicrophoneStatus::Undetermined | MicrophoneStatus::Granted => {}
    }
    match app.state::<ModelManager>().status() {
        ModelStatus::Ready => None,
        ModelStatus::Loading => Some(("Speech model is still loading…".into(), false)),
        ModelStatus::Verifying => Some(("Speech model is being verified…".into(), false)),
        ModelStatus::Downloading { downloaded, total } => Some((
            format!("Speech model downloading ({}%)", downloaded * 100 / total.max(1)),
            false,
        )),
        ModelStatus::Missing { .. } => {
            Some(("Speech model not downloaded. Get it in Settings.".into(), true))
        }
        ModelStatus::Error { message } => Some((format!("Speech model: {message}"), true)),
    }
}

/// Updates the pill only if `job` is still the latest dictation.
fn update<R: Runtime>(app: &AppHandle<R>, job: u64, state: PillState) {
    if app.state::<Pipeline>().job.load(Ordering::SeqCst) == job {
        pill::set(app, state);
    }
}

/// Runs one dictation; returns the final pill state to show, or `None` if
/// it was already shown (from the paste callback).
fn process<R: Runtime>(
    app: &AppHandle<R>,
    job: u64,
    pending: audio::PendingRecording,
) -> Option<PillState> {
    let released = Instant::now();
    let samples = match pending.wait() {
        Ok(samples) => samples,
        Err(e) => {
            eprintln!("[pipeline] recording failed: {e}");
            return Some(PillState::error(format!("Recording failed: {e}")));
        }
    };

    let secs = samples.len() as f32 / audio::TARGET_RATE as f32;
    let rms = audio::rms(&samples);
    println!("[pipeline] captured {secs:.2}s, rms {rms:.4}");
    #[cfg(debug_assertions)]
    match audio::write_wav(std::path::Path::new(DEBUG_WAV_PATH), &samples, audio::TARGET_RATE) {
        Ok(()) => println!("[pipeline] wrote {DEBUG_WAV_PATH}"),
        Err(e) => eprintln!("[pipeline] write wav: {e}"),
    }
    if rms < SILENCE_RMS {
        println!("[pipeline] silent, skipped");
        return Some(PillState::info("No speech detected"));
    }

    let Some(engine) = app.state::<Stt>().engine() else {
        return Some(PillState::error("Speech model isn't loaded"));
    };
    let settings = Settings::load_or_init(app);
    let opts = TranscribeOpts {
        language: &settings.language,
        dictionary: &settings.dictionary,
    };

    let stt_started = Instant::now();
    let text = match engine.transcribe(&samples, &opts) {
        Ok(text) => text,
        Err(e) => {
            eprintln!("[stt] {e}");
            return Some(PillState::error(format!("Transcription failed: {e}")));
        }
    };
    println!(
        "[stt] {} ms for {secs:.1}s of audio, {} chars",
        stt_started.elapsed().as_millis(),
        text.chars().count()
    );
    // Transcripts are private: only debug builds print them.
    #[cfg(debug_assertions)]
    println!("[stt] {text:?}");
    if text.is_empty() {
        return Some(PillState::info("No speech detected"));
    }

    // With cleanup off this returns the raw text without any network call.
    let cleaner = commands::active_cleaner(app, &settings);
    if matches!(cleaner, Ok(Some(_))) {
        update(app, job, PillState::Cleaning);
    }
    let ctx = CleanupContext {
        dictionary: &settings.dictionary,
        language: &settings.language,
    };
    let outcome = tauri::async_runtime::block_on(cleanup::clean_or_raw(cleaner, &text, &ctx));
    match (&outcome.error, outcome.cleaned) {
        (None, true) => println!("[cleanup] {} ms", outcome.latency_ms),
        (None, false) => println!("[cleanup] off"),
        (Some(e), _) => println!("[cleanup] fell back to raw after {} ms: {e}", outcome.latency_ms),
    }
    #[cfg(debug_assertions)]
    println!("[cleanup] {:?}", outcome.text);
    if outcome.text.is_empty() {
        return Some(PillState::info("No speech detected"));
    }

    // Shown once the text is pasted. A cleanup error (no key, timeout, API
    // error) still pastes the raw transcript, so it's a warning.
    let pasted_state = match &outcome.error {
        Some(e) => PillState::warning(format!("Cleanup failed ({e}). Pasted raw text.")),
        None => PillState::done("Pasted"),
    };

    let restore_after = Duration::from_millis(settings.paste_restore_delay_ms.clamp(50, 5000));
    let pasted = paste::paste(&outcome.text, restore_after, move || {
        println!(
            "[pipeline] pasted {} ms after release",
            released.elapsed().as_millis()
        );
        update(app, job, pasted_state);
    });
    match pasted {
        Ok(()) => None,
        Err(e) => {
            eprintln!("[paste] {e}");
            tray::show_settings(app);
            Some(PillState::error(
                "Copied to clipboard. Allow Accessibility in Settings to paste automatically.",
            ))
        }
    }
}
