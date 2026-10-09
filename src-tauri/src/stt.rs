use std::{
    os::raw::c_int,
    path::Path,
    sync::{Arc, Mutex, RwLock},
};

use whisper_rs::{
    FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters, WhisperState,
};

use crate::audio::TARGET_RATE;

/// Per-call options shared by every engine.
pub struct TranscribeOpts<'a> {
    /// "auto" or an ISO 639-1 code such as "id" or "en".
    pub language: &'a str,
    /// Terms whose spelling the engine should prefer.
    pub dictionary: &'a [String],
}

/// A speech-to-text engine. The pipeline only talks to this trait, so a
/// second engine (e.g. Apple's on-device recognizer) can be added later.
pub trait Transcriber: Send + Sync {
    /// Transcribes 16 kHz mono f32 audio.
    fn transcribe(&self, audio: &[f32], opts: &TranscribeOpts) -> Result<String, String>;
}

/// The active engine, set once a model has loaded.
#[derive(Default)]
pub struct Stt {
    engine: RwLock<Option<Arc<dyn Transcriber>>>,
}

impl Stt {
    pub fn engine(&self) -> Option<Arc<dyn Transcriber>> {
        self.engine.read().unwrap().clone()
    }

    pub fn set(&self, engine: Option<Arc<dyn Transcriber>>) {
        *self.engine.write().unwrap() = engine;
    }
}

/// Phrases whisper is known to invent on silence or noise (lowercase, no
/// trailing punctuation).
const HALLUCINATIONS: &[&str] = &[
    "thank you",
    "thank you very much",
    "thanks for watching",
    "thank you for watching",
    "please subscribe",
    "subtitles by the amara.org community",
    "you",
    "terima kasih",
    "terima kasih telah menonton",
    "terima kasih sudah menonton",
];
/// A known phrase is only dropped when whisper itself thinks the segment is
/// likely non-speech, so a genuinely dictated "terima kasih" survives.
const HALLUCINATION_NO_SPEECH: f32 = 0.2;

/// whisper's encoder always processes a 30 s window (1500 frames), so a 3 s
/// dictation costs as much as a 30 s one. Shrinking the window to the audio
/// plus a margin cut latency about 3x on an M1. Too small a window makes
/// short clips hallucinate, hence the floor (512 frames is about 10 s).
const ENCODER_FRAMES_PER_SECOND: f32 = 1500.0 / 30.0;
const AUDIO_CTX_MARGIN: c_int = 128;
const AUDIO_CTX_FLOOR: c_int = 512;
const AUDIO_CTX_MAX: c_int = 1500;
/// Metal does the heavy lifting; more CPU threads didn't help on an M1 and
/// 8 spilled onto efficiency cores.
const THREADS: c_int = 4;

/// whisper.cpp (Metal) via whisper-rs. The model is loaded once and its
/// state reused across dictations; the state keeps the context alive.
pub struct WhisperTranscriber {
    state: Mutex<WhisperState>,
}

impl WhisperTranscriber {
    pub fn load(path: &Path) -> Result<Self, String> {
        // Route whisper.cpp / ggml logging away from stderr.
        whisper_rs::install_logging_hooks();

        let mut params = WhisperContextParameters::default();
        params.flash_attn(true);
        let ctx = WhisperContext::new_with_params(path, params)
            .map_err(|e| format!("load model {}: {e}", path.display()))?;
        let mut state = ctx
            .create_state()
            .map_err(|e| format!("create whisper state: {e}"))?;

        // Warm up: the first run compiles Metal shaders, so do it now rather
        // than on the user's first dictation.
        let silence = vec![0.0; TARGET_RATE as usize];
        run(&mut state, &silence, &TranscribeOpts { language: "en", dictionary: &[] })?;

        Ok(Self {
            state: Mutex::new(state),
        })
    }
}

impl Transcriber for WhisperTranscriber {
    fn transcribe(&self, audio: &[f32], opts: &TranscribeOpts) -> Result<String, String> {
        run(&mut self.state.lock().unwrap(), audio, opts)
    }
}

fn run(state: &mut WhisperState, audio: &[f32], opts: &TranscribeOpts) -> Result<String, String> {
    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
    let language = if opts.language.is_empty() { "auto" } else { opts.language };
    params.set_language(Some(language));
    params.set_n_threads(THREADS);
    params.set_audio_ctx(audio_ctx(audio.len()));
    params.set_no_timestamps(true);
    params.set_suppress_nst(true);
    params.set_print_special(false);
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);

    let prompt = initial_prompt(opts.dictionary);
    if !prompt.is_empty() {
        params.set_initial_prompt(&prompt);
    }

    state
        .full(params, audio)
        .map_err(|e| format!("transcribe: {e}"))?;

    let mut text = String::new();
    for segment in state.as_iter() {
        let Ok(part) = segment.to_str_lossy() else {
            continue;
        };
        let no_speech = segment.no_speech_probability();
        if is_hallucination(&part, no_speech) {
            println!("[stt] dropped likely hallucination (no_speech {no_speech:.2})");
            continue;
        }
        text.push_str(&part);
    }
    Ok(clean_markers(text.trim()).to_string())
}

/// Biases whisper toward the dictionary's spellings.
fn initial_prompt(dictionary: &[String]) -> String {
    dictionary
        .iter()
        .map(|t| t.trim().replace('\0', ""))
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>()
        .join(", ")
}

fn is_hallucination(segment: &str, no_speech: f32) -> bool {
    let normalized = segment
        .trim()
        .trim_end_matches(|c: char| c.is_ascii_punctuation() || c.is_whitespace())
        .to_lowercase();
    no_speech >= HALLUCINATION_NO_SPEECH && HALLUCINATIONS.contains(&normalized.as_str())
}

/// whisper emits markers like "[BLANK_AUDIO]" or "(silence)" for non-speech.
fn clean_markers(text: &str) -> &str {
    let bracketed = (text.starts_with('[') && text.ends_with(']'))
        || (text.starts_with('(') && text.ends_with(')'));
    if bracketed {
        ""
    } else {
        text
    }
}

/// Encoder window for `samples` of 16 kHz audio.
fn audio_ctx(samples: usize) -> c_int {
    let secs = samples as f32 / TARGET_RATE as f32;
    let frames = (secs * ENCODER_FRAMES_PER_SECOND).ceil() as c_int;
    (frames + AUDIO_CTX_MARGIN).clamp(AUDIO_CTX_FLOOR, AUDIO_CTX_MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_joins_dictionary_terms() {
        let terms = ["Tauri".to_string(), " ".to_string(), "whisper\0.cpp".to_string()];
        assert_eq!(initial_prompt(&terms), "Tauri, whisper.cpp");
    }

    #[test]
    fn hallucinations_need_a_known_phrase_and_high_no_speech() {
        assert!(is_hallucination(" Thank you.", 0.5));
        assert!(is_hallucination("Terima kasih telah menonton!", 0.3));
        assert!(!is_hallucination(" Thank you.", 0.05));
        assert!(!is_hallucination("Thank you for the review.", 0.9));
    }

    #[test]
    fn audio_ctx_tracks_length_within_bounds() {
        let rate = TARGET_RATE as usize;
        assert_eq!(audio_ctx(3 * rate), 512); // floor
        assert_eq!(audio_ctx(11 * rate), 550 + 128);
        assert_eq!(audio_ctx(60 * rate), 1500); // full window
    }

    #[test]
    fn drops_non_speech_markers() {
        assert_eq!(clean_markers("[BLANK_AUDIO]"), "");
        assert_eq!(clean_markers("(silence)"), "");
        assert_eq!(clean_markers("jam 4 [ya]"), "jam 4 [ya]");
    }
}
