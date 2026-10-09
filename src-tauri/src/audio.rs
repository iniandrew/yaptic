use std::{
    fs::{self, OpenOptions},
    io::{self, BufWriter, Write},
    os::unix::fs::OpenOptionsExt,
    path::Path,
    sync::{
        atomic::{AtomicU32, Ordering},
        mpsc, Arc, Mutex,
    },
    thread,
    time::Duration,
};

use cpal::{
    traits::{DeviceTrait, HostTrait, StreamTrait},
    FromSample, Sample, SampleFormat, SizedSample, Stream, StreamConfig,
};
use rubato::{audioadapter_buffers::direct::InterleavedSlice, Fft, FixedSync, Resampler};

/// Sample rate whisper.cpp expects.
pub const TARGET_RATE: u32 = 16_000;

type StopReply = mpsc::Sender<Result<Vec<f32>, String>>;

enum Command {
    Start,
    Stop(StopReply),
}

/// Microphone recorder. cpal streams are not `Send` on macOS, so the stream
/// lives on a dedicated thread and is driven through a command channel.
/// Commands are processed in order, so `start` never blocks the caller and a
/// failed start surfaces as an error from the following stop.
pub struct Recorder {
    tx: mpsc::Sender<Command>,
    /// RMS of the latest audio callback (f32 bits), for the pill's meter.
    level: Arc<AtomicU32>,
}

struct Active {
    stream: Stream,
    samples: Arc<Mutex<Vec<f32>>>,
    rate: u32,
}

impl Recorder {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel();
        let level = Arc::new(AtomicU32::new(0));
        let thread_level = level.clone();
        thread::Builder::new()
            .name("audio".into())
            .spawn(move || run(rx, thread_level))
            .expect("failed to spawn audio thread");
        Self { tx, level }
    }

    pub fn start(&self) {
        let _ = self.tx.send(Command::Start);
    }

    /// Queues a stop immediately (keeping it ordered before any later
    /// `start`) and returns a handle that yields the 16 kHz mono audio.
    pub fn stop(&self) -> PendingRecording {
        let (reply_tx, reply_rx) = mpsc::channel();
        let _ = self.tx.send(Command::Stop(reply_tx));
        PendingRecording(reply_rx)
    }

    /// Current input level (RMS, roughly 0.0 to 0.3 for speech).
    pub fn level(&self) -> f32 {
        f32::from_bits(self.level.load(Ordering::Relaxed))
    }
}

pub struct PendingRecording(mpsc::Receiver<Result<Vec<f32>, String>>);

impl PendingRecording {
    /// Waits for the stream to close and the audio to be resampled.
    pub fn wait(self) -> Result<Vec<f32>, String> {
        self.0
            .recv()
            .map_err(|_| "audio thread stopped".to_string())?
    }
}

/// Opens the microphone briefly so macOS shows its permission prompt.
pub fn request_permission() {
    thread::spawn(|| match open_stream(Arc::new(AtomicU32::new(0))) {
        Ok(active) => {
            thread::sleep(Duration::from_millis(500));
            drop(active);
        }
        Err(e) => eprintln!("[audio] permission probe: {e}"),
    });
}

fn run(rx: mpsc::Receiver<Command>, level: Arc<AtomicU32>) {
    let mut active: Option<Result<Active, String>> = None;

    for command in rx {
        match command {
            Command::Start => {
                if active.is_none() {
                    active = Some(open_stream(level.clone()));
                }
            }
            Command::Stop(reply) => {
                let result = match active.take() {
                    Some(Ok(Active {
                        stream,
                        samples,
                        rate,
                    })) => {
                        drop(stream);
                        let samples = std::mem::take(&mut *samples.lock().unwrap());
                        resample(samples, rate)
                    }
                    Some(Err(err)) => Err(err),
                    None => Err("not recording".to_string()),
                };
                level.store(0, Ordering::Relaxed);
                let _ = reply.send(result);
            }
        }
    }
}

fn open_stream(level: Arc<AtomicU32>) -> Result<Active, String> {
    let device = cpal::default_host()
        .default_input_device()
        .ok_or("no microphone found")?;
    let supported = device
        .default_input_config()
        .map_err(|e| format!("microphone config: {e}"))?;

    let rate = supported.sample_rate();
    let format = supported.sample_format();
    let config: StreamConfig = supported.into();
    let samples = Arc::new(Mutex::new(Vec::with_capacity(rate as usize * 30)));
    let sink = Sink {
        samples: samples.clone(),
        level,
    };

    let stream = match format {
        SampleFormat::F32 => build_stream::<f32>(&device, config, sink),
        SampleFormat::I16 => build_stream::<i16>(&device, config, sink),
        SampleFormat::I32 => build_stream::<i32>(&device, config, sink),
        SampleFormat::U16 => build_stream::<u16>(&device, config, sink),
        other => return Err(format!("unsupported sample format {other}")),
    }
    .map_err(|e| format!("open microphone: {e}"))?;

    stream
        .play()
        .map_err(|e| format!("start microphone: {e}"))?;

    Ok(Active {
        stream,
        samples,
        rate,
    })
}

/// Where the audio callback writes: the mono buffer and the live level.
struct Sink {
    samples: Arc<Mutex<Vec<f32>>>,
    level: Arc<AtomicU32>,
}

fn build_stream<T>(
    device: &cpal::Device,
    config: StreamConfig,
    sink: Sink,
) -> Result<Stream, cpal::Error>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    let channels = config.channels as usize;
    device.build_input_stream(
        config,
        move |data: &[T], _: &cpal::InputCallbackInfo| {
            let mut buf = sink.samples.lock().unwrap();
            let start = buf.len();
            // Downmix interleaved frames to mono.
            for frame in data.chunks_exact(channels) {
                let sum: f32 = frame.iter().map(|&s| f32::from_sample(s)).sum();
                buf.push(sum / channels as f32);
            }
            sink.level
                .store(rms(&buf[start..]).to_bits(), Ordering::Relaxed);
        },
        |err| eprintln!("[audio] stream error: {err}"),
        None,
    )
}

fn resample(samples: Vec<f32>, from: u32) -> Result<Vec<f32>, String> {
    if from == TARGET_RATE || samples.is_empty() {
        return Ok(samples);
    }
    let err = |e: &dyn std::fmt::Display| format!("resample: {e}");

    let mut resampler = Fft::<f32>::new(
        from as usize,
        TARGET_RATE as usize,
        1024,
        1,
        FixedSync::Input,
    )
    .map_err(|e| err(&e))?;
    let input = InterleavedSlice::new(&samples, 1, samples.len()).map_err(|e| err(&e))?;
    let output = resampler
        .process_all(&input, samples.len(), None)
        .map_err(|e| err(&e))?;
    Ok(output.take_data())
}

/// Root-mean-square level of a block of samples.
pub fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt()
}

/// Writes mono f32 samples as a 16-bit PCM WAV file (debugging aid).
///
/// The file is owner-only (0600) and created exclusively: any old file is
/// unlinked first, and if someone else's file or symlink sits at the path
/// (e.g. in a shared /tmp) creation fails instead of writing through it.
/// Only debug builds dump audio.
#[cfg_attr(not(debug_assertions), allow(dead_code))]
pub fn write_wav(path: &Path, samples: &[f32], rate: u32) -> io::Result<()> {
    let data_len = (samples.len() * 2) as u32;
    match fs::remove_file(path) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    let mut w = BufWriter::new(file);

    w.write_all(b"RIFF")?;
    w.write_all(&(36 + data_len).to_le_bytes())?;
    w.write_all(b"WAVEfmt ")?;
    w.write_all(&16u32.to_le_bytes())?; // fmt chunk size
    w.write_all(&1u16.to_le_bytes())?; // PCM
    w.write_all(&1u16.to_le_bytes())?; // mono
    w.write_all(&rate.to_le_bytes())?;
    w.write_all(&(rate * 2).to_le_bytes())?; // byte rate
    w.write_all(&2u16.to_le_bytes())?; // block align
    w.write_all(&16u16.to_le_bytes())?; // bits per sample
    w.write_all(b"data")?;
    w.write_all(&data_len.to_le_bytes())?;
    for &s in samples {
        let v = (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
        w.write_all(&v.to_le_bytes())?;
    }
    w.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resamples_48k_to_16k() {
        let tone: Vec<f32> = (0..48_000)
            .map(|i| (i as f32 * 440.0 * std::f32::consts::TAU / 48_000.0).sin() * 0.5)
            .collect();
        let out = resample(tone, 48_000).unwrap();
        assert!((out.len() as i64 - 16_000).abs() <= 1, "got {}", out.len());
        let peak = out.iter().fold(0f32, |m, s| m.max(s.abs()));
        assert!((0.45..0.55).contains(&peak), "peak {peak}");
    }

    #[test]
    fn rms_of_square_wave() {
        assert_eq!(rms(&[]), 0.0);
        assert!((rms(&[0.5, -0.5, 0.5, -0.5]) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn writes_owner_only_wav_and_replaces_previous() {
        use std::os::unix::fs::PermissionsExt;

        let path = std::env::temp_dir().join("yaptic-test.wav");
        write_wav(&path, &[0.5; 10], TARGET_RATE).unwrap();
        write_wav(&path, &[0.0; 160], TARGET_RATE).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(bytes.len(), 44 + 320);
        assert_eq!(&bytes[..4], b"RIFF");
        assert_eq!(&bytes[8..16], b"WAVEfmt ");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        let _ = std::fs::remove_file(path);
    }
}
