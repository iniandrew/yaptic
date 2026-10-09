import { listen } from "@tauri-apps/api/event";

type PillState =
  | { state: "listening" | "transcribing" | "cleaning" }
  | { state: "done" | "info" | "warning" | "error"; message: string };

const LABELS = {
  listening: "Listening",
  transcribing: "Transcribing",
  cleaning: "Cleaning up",
} as const;

const ICONS: Partial<Record<PillState["state"], string>> = {
  done: "✓",
  info: "i",
  warning: "!",
  error: "✕",
};

const pill = document.getElementById("pill")!;
const icon = document.getElementById("icon")!;
const label = document.getElementById("label")!;
const bars = Array.from(document.querySelectorAll<HTMLElement>(".bar"));

/** Per-bar weight so the meter reads as a waveform, not a block. */
const SHAPE = [0.45, 0.75, 1, 0.85, 0.6, 0.9, 0.5];
/** Bar heights in px: a round dot at silence, full height when loud. */
const MIN_HEIGHT = 3;
const MAX_HEIGHT = 20;

let smoothed = 0;

function renderLevel(level: number) {
  // Speech RMS is roughly 0.01 to 0.2; sqrt lifts quiet input into view.
  const target = Math.min(1, Math.sqrt(level) * 2.2);
  // Fast attack, slower decay.
  smoothed = target > smoothed ? target : smoothed * 0.7 + target * 0.3;
  bars.forEach((bar, i) => {
    const jitter = 0.75 + Math.random() * 0.5;
    const v = Math.min(1, smoothed * SHAPE[i] * jitter);
    bar.style.height = `${MIN_HEIGHT + (MAX_HEIGHT - MIN_HEIGHT) * v}px`;
  });
}

listen<number>("pill-level", ({ payload }) => {
  if (pill.dataset.state !== "listening") return;
  if (payload === 0) smoothed = 0;
  renderLevel(payload);
});

listen<PillState>("pill-state", ({ payload }) => {
  pill.dataset.state = payload.state;
  icon.textContent = ICONS[payload.state] ?? "";
  label.textContent = "message" in payload ? payload.message : LABELS[payload.state];
  pill.setAttribute("aria-label", label.textContent);
  if (payload.state === "listening") {
    smoothed = 0;
    renderLevel(0);
  } else {
    // Let the CSS wave (or hidden bars) take over from the live meter.
    bars.forEach((bar) => bar.style.removeProperty("height"));
  }
});

renderLevel(0);
