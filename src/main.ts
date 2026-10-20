import { getVersion } from "@tauri-apps/api/app";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

// -------------------------------------------------------------------- types

interface GeneralSettings {
  hotkey: string;
  language: "auto" | "id" | "en";
  dictionary: string[];
  model_path: string | null;
  paste_restore_delay_ms: number;
}

type Provider = "anthropic" | "openai_compatible";

interface ProviderSettings {
  base_url: string;
  model: string;
  timeout_ms: number;
}

interface CleanupSettings {
  enabled: boolean;
  provider: Provider;
  anthropic: ProviderSettings;
  openai_compatible: ProviderSettings;
}

interface CleanupView {
  settings: CleanupSettings;
  has_key: Record<Provider, boolean>;
}

interface EndpointInfo {
  endpoint: string | null;
  error: string | null;
  warning: string | null;
}

interface Outcome {
  text: string;
  cleaned: boolean;
  error: string | null;
  latency_ms: number;
}

interface UpdateInfo {
  current: string;
  latest: string;
  available: boolean;
}

interface PermissionStatus {
  microphone: "undetermined" | "restricted" | "denied" | "granted";
  accessibility: boolean;
}

type ModelStatus =
  | { state: "missing"; downloaded: number; total: number }
  | { state: "downloading"; downloaded: number; total: number }
  | { state: "verifying" }
  | { state: "loading" }
  | { state: "ready" }
  | { state: "error"; message: string };

// ------------------------------------------------------------------ helpers

const $ = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;

type Tone = "" | "error" | "ok" | "warning";

/** Sets a message; empty text hides the element. */
function show(el: HTMLElement, text: string, tone: Tone = "") {
  el.textContent = text;
  el.hidden = text === "";
  el.classList.toggle("error", tone === "error");
  el.classList.toggle("ok", tone === "ok");
  el.classList.toggle("warning", tone === "warning");
}

/** Restores an element's `data-default` text (or hides it). */
function reset(el: HTMLElement) {
  show(el, el.dataset.default ?? "");
}

// ------------------------------------------------------------------ general

let general: GeneralSettings | null = null;
let modelStatus: ModelStatus = { state: "loading" };

/** Saves a change to the general settings; shows errors in `errorEl`. */
async function saveGeneral(patch: Partial<GeneralSettings>, errorEl: HTMLElement) {
  if (!general) return false;
  try {
    general = await invoke<GeneralSettings>("save_general", {
      general: { ...general, ...patch },
    });
    if (errorEl.classList.contains("error")) reset(errorEl);
    return true;
  } catch (err) {
    show(errorEl, String(err), "error");
    return false;
  } finally {
    renderGeneral();
  }
}

// Shortcut ---------------------------------------------------------------

const MODIFIER_SYMBOLS: Record<string, string> = {
  control: "⌃",
  ctrl: "⌃",
  alt: "⌥",
  option: "⌥",
  shift: "⇧",
  super: "⌘",
  cmd: "⌘",
  command: "⌘",
};

const KEY_NAMES: Record<string, string> = {
  Space: "Space",
  Backquote: "`",
  Minus: "-",
  Equal: "=",
  BracketLeft: "[",
  BracketRight: "]",
  Backslash: "\\",
  Semicolon: ";",
  Quote: "'",
  Comma: ",",
  Period: ".",
  Slash: "/",
  ArrowUp: "↑",
  ArrowDown: "↓",
  ArrowLeft: "←",
  ArrowRight: "→",
  Enter: "Return",
};

function formatHotkey(hotkey: string): string {
  let mods = "";
  let key = "";
  for (const token of hotkey.split("+").filter(Boolean)) {
    const symbol = MODIFIER_SYMBOLS[token.toLowerCase()];
    if (symbol) mods += symbol;
    else key = KEY_NAMES[token] ?? token.replace(/^(Key|Digit)/, "");
  }
  return key ? `${mods} ${key}`.trim() : mods;
}

const MODIFIER_CODES = new Set([
  "AltLeft",
  "AltRight",
  "ShiftLeft",
  "ShiftRight",
  "ControlLeft",
  "ControlRight",
  "MetaLeft",
  "MetaRight",
  "CapsLock",
  "Fn",
]);

const hotkeyButton = $<HTMLButtonElement>("hotkey");
const hotkeyHint = $<HTMLParagraphElement>("hotkey-hint");
const hotkeyInline = $<HTMLElement>("hotkey-inline");
let capturing = false;

async function startCapture() {
  capturing = true;
  hotkeyButton.classList.add("capturing");
  hotkeyButton.textContent = "Press keys…";
  show(hotkeyHint, "Hold modifiers and press a key. Esc cancels.");
  // A registered global shortcut never reaches the page, so pause it.
  await invoke("suspend_hotkey");
}

async function endCapture() {
  capturing = false;
  hotkeyButton.classList.remove("capturing");
  try {
    await invoke("resume_hotkey");
  } catch (err) {
    show(hotkeyHint, String(err), "error");
  }
  renderGeneral();
}

hotkeyButton.addEventListener("click", () => {
  if (!capturing) startCapture();
});

window.addEventListener(
  "keydown",
  async (e) => {
    if (!capturing) return;
    e.preventDefault();
    e.stopPropagation();
    if (e.repeat) return;

    const mods = [
      e.ctrlKey && "Control",
      e.altKey && "Alt",
      e.shiftKey && "Shift",
      e.metaKey && "Super",
    ].filter((m): m is string => Boolean(m));

    if (e.code === "Escape" && mods.length === 0) {
      reset(hotkeyHint);
      await endCapture();
      return;
    }
    if (MODIFIER_CODES.has(e.code)) {
      hotkeyButton.textContent = `${formatHotkey(mods.join("+"))} …`;
      return;
    }

    capturing = false;
    if (await saveGeneral({ hotkey: [...mods, e.code].join("+") }, hotkeyHint)) {
      reset(hotkeyHint);
    }
    await endCapture();
  },
  true,
);

window.addEventListener("blur", () => {
  if (capturing) {
    reset(hotkeyHint);
    endCapture();
  }
});

// Language ---------------------------------------------------------------

const language = $<HTMLSelectElement>("language");
const languageNote = $<HTMLParagraphElement>("language-note");

language.addEventListener("change", () =>
  saveGeneral({ language: language.value as GeneralSettings["language"] }, languageNote),
);

// Dictionary -------------------------------------------------------------

const dictionaryList = $<HTMLUListElement>("dictionary");
const dictionaryForm = $<HTMLFormElement>("dictionary-form");
const dictionaryInput = $<HTMLInputElement>("dictionary-input");
const dictionaryError = $<HTMLParagraphElement>("dictionary-error");

function renderDictionary(terms: string[]) {
  dictionaryList.replaceChildren(
    ...terms.map((term) => {
      const item = document.createElement("li");
      const label = document.createElement("span");
      label.textContent = term;
      const remove = document.createElement("button");
      remove.type = "button";
      remove.className = "chip-remove";
      remove.textContent = "×";
      remove.title = `Remove ${term}`;
      remove.addEventListener("click", () => {
        if (!general) return;
        saveGeneral({ dictionary: general.dictionary.filter((t) => t !== term) }, dictionaryError);
      });
      item.append(label, remove);
      return item;
    }),
  );
  dictionaryList.hidden = terms.length === 0;
}

dictionaryForm.addEventListener("submit", async (e) => {
  e.preventDefault();
  const term = dictionaryInput.value.trim();
  if (!term || !general) return;
  if (await saveGeneral({ dictionary: [...general.dictionary, term] }, dictionaryError)) {
    dictionaryInput.value = "";
  }
});

// Model ------------------------------------------------------------------

const modelStatusEl = $<HTMLParagraphElement>("model-status");
const modelProgress = $<HTMLProgressElement>("model-progress");
const modelActions = $<HTMLDivElement>("model-actions");
const modelDownload = $<HTMLButtonElement>("model-download");
const modelCancel = $<HTMLButtonElement>("model-cancel");
const modelPath = $<HTMLInputElement>("model-path");
const modelPathApply = $<HTMLButtonElement>("model-path-apply");
const modelPathReset = $<HTMLButtonElement>("model-path-reset");
const modelPathHint = $<HTMLParagraphElement>("model-path-hint");

const mb = (bytes: number) => `${Math.round(bytes / 1_000_000)} MB`;

function renderModel() {
  const status = modelStatus;
  const custom = Boolean(general?.model_path);
  modelStatusEl.classList.toggle("error", status.state === "error");
  modelStatusEl.classList.toggle("ok", status.state === "ready");
  modelProgress.hidden = status.state !== "downloading";
  modelDownload.hidden = custom || !["missing", "error"].includes(status.state);
  modelCancel.hidden = status.state !== "downloading";
  modelActions.hidden = modelDownload.hidden && modelCancel.hidden;

  switch (status.state) {
    case "missing":
      modelStatusEl.textContent =
        status.downloaded > 0
          ? `Download paused at ${mb(status.downloaded)} of ${mb(status.total)}.`
          : `Whisper large-v3-turbo (${mb(status.total)}) is needed for transcription.`;
      modelDownload.textContent = status.downloaded > 0 ? "Resume download" : "Download";
      break;
    case "downloading":
      modelStatusEl.textContent = `Downloading… ${mb(status.downloaded)} of ${mb(status.total)}`;
      modelProgress.value = status.downloaded / status.total;
      break;
    case "verifying":
      modelStatusEl.textContent = "Verifying download…";
      break;
    case "loading":
      modelStatusEl.textContent = "Loading model…";
      break;
    case "ready":
      modelStatusEl.textContent = "Ready.";
      break;
    case "error":
      modelStatusEl.textContent = status.message;
      modelDownload.textContent = "Retry download";
      break;
  }
}

modelDownload.addEventListener("click", () => {
  invoke("download_model").catch((err) => show(modelStatusEl, String(err), "error"));
});
modelCancel.addEventListener("click", () => invoke("cancel_model_download"));
modelPath.addEventListener("input", () => {
  modelPathApply.disabled = modelPath.value.trim() === (general?.model_path ?? "");
});
modelPathApply.addEventListener("click", () =>
  saveGeneral({ model_path: modelPath.value.trim() || null }, modelPathHint),
);
modelPathReset.addEventListener("click", () => saveGeneral({ model_path: null }, modelPathHint));

listen<ModelStatus>("model-status", (e) => {
  modelStatus = e.payload;
  renderModel();
});

// General ----------------------------------------------------------------

const login = $<HTMLInputElement>("login");
const pasteDelay = $<HTMLInputElement>("paste-delay");
const generalError = $<HTMLParagraphElement>("general-error");

login.addEventListener("change", async () => {
  try {
    login.checked = await invoke<boolean>("set_launch_at_login", { enabled: login.checked });
    reset(generalError);
  } catch (err) {
    login.checked = !login.checked;
    show(generalError, String(err), "error");
  }
});

pasteDelay.addEventListener("change", () => {
  const ms = Number(pasteDelay.value);
  if (Number.isFinite(ms)) saveGeneral({ paste_restore_delay_ms: Math.round(ms) }, generalError);
});

// Updates -----------------------------------------------------------------

const appVersion = $<HTMLSpanElement>("app-version");
const updateCheck = $<HTMLButtonElement>("update-check");
const updateOpen = $<HTMLButtonElement>("update-open");
const updateNote = $<HTMLParagraphElement>("update-note");

updateCheck.addEventListener("click", async () => {
  updateCheck.disabled = true;
  updateOpen.hidden = true;
  show(updateNote, "Checking…");
  try {
    const info = await invoke<UpdateInfo>("check_for_update");
    if (info.available) {
      show(updateNote, `v${info.latest} is available.`, "ok");
      updateOpen.hidden = false;
    } else {
      show(updateNote, "You're on the latest version.", "ok");
    }
  } catch (err) {
    show(updateNote, String(err), "error");
  } finally {
    updateCheck.disabled = false;
  }
});

updateOpen.addEventListener("click", () => invoke("open_release_page"));

function renderGeneral() {
  if (!general) return;
  const label = formatHotkey(general.hotkey);
  if (!capturing) hotkeyButton.textContent = label;
  hotkeyInline.textContent = label;
  language.value = general.language;
  renderDictionary(general.dictionary);
  modelPath.value = general.model_path ?? "";
  modelPathApply.disabled = true;
  modelPathReset.disabled = !general.model_path;
  pasteDelay.value = String(general.paste_restore_delay_ms);
  renderModel();
}

// -------------------------------------------------------------- permissions

const micStatus = $<HTMLParagraphElement>("mic-status");
const micFix = $<HTMLButtonElement>("mic-fix");
const axStatus = $<HTMLParagraphElement>("ax-status");
const axFix = $<HTMLButtonElement>("ax-fix");

const MIC_TEXT: Record<PermissionStatus["microphone"], [string, Tone, string | null]> = {
  granted: ["Granted.", "ok", null],
  undetermined: ["Not requested yet.", "warning", "Allow"],
  denied: ["Denied. Recordings will be silent.", "error", "Open System Settings"],
  restricted: ["Restricted by a system policy.", "error", null],
};

async function refreshPermissions() {
  const status = await invoke<PermissionStatus>("permission_status");
  const [text, tone, action] = MIC_TEXT[status.microphone];
  show(micStatus, text, tone);
  micFix.hidden = action === null;
  micFix.textContent = action ?? "";

  show(
    axStatus,
    status.accessibility
      ? "Granted. Text is pasted at the cursor."
      : "Not granted. Text is copied to the clipboard instead.",
    status.accessibility ? "ok" : "error",
  );
  axFix.hidden = status.accessibility;
}

micFix.addEventListener("click", async () => {
  await invoke("fix_microphone");
  setTimeout(refreshPermissions, 1500);
});
axFix.addEventListener("click", () => invoke("fix_accessibility"));
// Permissions change in System Settings; re-check when we regain focus.
window.addEventListener("focus", refreshPermissions);

// ------------------------------------------------------------------ cleanup

const DEFAULTS: Record<Provider, { baseUrl: string; model: string }> = {
  anthropic: { baseUrl: "https://api.anthropic.com", model: "claude-haiku-5-5" },
  openai_compatible: {
    baseUrl: "https://api.openai.com/v1",
    model: "Model name from your provider",
  },
};

const PROVIDER_NAMES: Record<Provider, string> = {
  anthropic: "Anthropic",
  openai_compatible: "OpenAI-compatible",
};

const cleanupEnabled = $<HTMLInputElement>("cleanup-enabled");
const cleanupSummary = $<HTMLParagraphElement>("cleanup-summary");
const providerSelect = $<HTMLSelectElement>("provider");
const baseUrl = $<HTMLInputElement>("base-url");
const endpointNote = $<HTMLParagraphElement>("endpoint");
const urlWarning = $<HTMLParagraphElement>("url-warning");
const model = $<HTMLInputElement>("model");
const keyForm = $<HTMLFormElement>("key-form");
const apiKey = $<HTMLInputElement>("api-key");
const keyRemove = $<HTMLButtonElement>("key-remove");
const keyStatus = $<HTMLParagraphElement>("key-status");
const timeout = $<HTMLInputElement>("timeout");
const testButton = $<HTMLButtonElement>("test-connection");
const testResult = $<HTMLParagraphElement>("test-result");
const cleanupError = $<HTMLParagraphElement>("cleanup-error");

let cleanup: CleanupView | null = null;

/** Provider settings as currently typed (maybe unsaved). */
function formConfig(): ProviderSettings {
  return {
    base_url: baseUrl.value.trim(),
    model: model.value.trim(),
    timeout_ms: Math.round(Number(timeout.value) * 1000) || 4000,
  };
}

function selectedProvider(): Provider {
  return providerSelect.value as Provider;
}

async function refreshEndpoint() {
  const info = await invoke<EndpointInfo>("inspect_base_url", {
    provider: selectedProvider(),
    config: formConfig(),
  });
  if (info.error) show(endpointNote, info.error, "error");
  else show(endpointNote, `Requests go to ${info.endpoint}`);
  show(urlWarning, info.warning ?? "", "warning");
}

function renderCleanup() {
  if (!cleanup) return;
  const { settings, has_key } = cleanup;
  const provider = settings.provider;
  cleanupEnabled.checked = settings.enabled;
  providerSelect.value = provider;
  show(testResult, "");

  const cfg = settings[provider];
  baseUrl.value = cfg.base_url;
  baseUrl.placeholder = DEFAULTS[provider].baseUrl;
  model.value = cfg.model;
  model.placeholder = DEFAULTS[provider].model;
  timeout.value = String(cfg.timeout_ms / 1000);

  const local = provider === "openai_compatible";
  apiKey.value = "";
  apiKey.placeholder = has_key[provider]
    ? "•••••••• (saved)"
    : local
      ? "Optional for local servers"
      : "Paste your API key";
  keyRemove.disabled = !has_key[provider];
  show(
    keyStatus,
    has_key[provider]
      ? "Stored in the macOS Keychain."
      : local
        ? "No key. Fine for Ollama or LM Studio; cloud providers need one."
        : "No key yet. Cleanup is skipped until you add one.",
    has_key[provider] || local ? "" : "warning",
  );

  show(
    cleanupSummary,
    settings.enabled
      ? `On, using ${PROVIDER_NAMES[provider]}.`
      : "Off. The raw transcript is pasted and nothing is sent anywhere.",
    settings.enabled ? "ok" : "",
  );
  refreshEndpoint();
}

async function saveCleanup(patch: Partial<CleanupSettings>, keepForm = true) {
  if (!cleanup) return;
  const next: CleanupSettings = { ...cleanup.settings, ...patch };
  // Keep what's typed for the provider shown, unless switching providers.
  if (keepForm) next[cleanup.settings.provider] = formConfig();
  try {
    cleanup = await invoke<CleanupView>("save_cleanup", { cleanup: next });
    reset(cleanupError);
  } catch (err) {
    show(cleanupError, String(err), "error");
  }
  renderCleanup();
}

cleanupEnabled.addEventListener("change", () =>
  saveCleanup({ enabled: cleanupEnabled.checked }),
);
providerSelect.addEventListener("change", () =>
  // The form still shows the provider being left; save it with the switch.
  saveCleanup({
    provider: selectedProvider(),
    ...(cleanup ? { [cleanup.settings.provider]: formConfig() } : {}),
  }, false),
);
for (const input of [baseUrl, model, timeout]) {
  input.addEventListener("change", () => saveCleanup({}));
}
baseUrl.addEventListener("input", refreshEndpoint);

async function saveKey(key: string) {
  try {
    cleanup = await invoke<CleanupView>("set_cleanup_key", {
      provider: selectedProvider(),
      key,
    });
    renderCleanup();
  } catch (err) {
    show(keyStatus, String(err), "error");
  }
}

keyForm.addEventListener("submit", (e) => {
  e.preventDefault();
  if (apiKey.value.trim()) saveKey(apiKey.value);
});
keyRemove.addEventListener("click", () => saveKey(""));

testButton.addEventListener("click", async () => {
  testButton.disabled = true;
  show(testResult, "Testing…");
  try {
    const result = await invoke<{ latency_ms: number; output: string }>(
      "test_cleanup_connection",
      { provider: selectedProvider(), config: formConfig() },
    );
    show(testResult, `Connected in ${result.latency_ms} ms. Reply: "${result.output}"`, "ok");
  } catch (err) {
    show(testResult, String(err), "error");
  } finally {
    testButton.disabled = false;
  }
});

// Try cleanup --------------------------------------------------------------

const tryInput = $<HTMLTextAreaElement>("try-input");
const tryRun = $<HTMLButtonElement>("try-run");
const tryMeta = $<HTMLSpanElement>("try-meta");
const tryOutput = $<HTMLPreElement>("try-output");

tryRun.addEventListener("click", async () => {
  tryRun.disabled = true;
  show(tryMeta, "Cleaning…");
  tryOutput.hidden = true;
  try {
    const outcome = await invoke<Outcome>("try_cleanup", { text: tryInput.value });
    tryOutput.textContent = outcome.text;
    tryOutput.hidden = false;
    if (outcome.cleaned) show(tryMeta, `Cleaned in ${outcome.latency_ms} ms`, "ok");
    else if (outcome.error)
      show(tryMeta, `Raw text kept after ${outcome.latency_ms} ms: ${outcome.error}`, "error");
    else show(tryMeta, "Cleanup is off, so this is the raw text.");
  } catch (err) {
    show(tryMeta, String(err), "error");
  } finally {
    tryRun.disabled = false;
  }
});

// --------------------------------------------------------------------- init

async function init() {
  const [loadedGeneral, status, paths, loadedCleanup, loginEnabled, version] = await Promise.all([
    invoke<GeneralSettings>("get_general"),
    invoke<ModelStatus>("model_status"),
    invoke<{ settings: string | null; default_model: string | null }>("app_paths"),
    invoke<CleanupView>("get_cleanup"),
    invoke<boolean>("launch_at_login"),
    getVersion(),
  ]);
  appVersion.textContent = `v${version}`;
  general = loadedGeneral;
  modelStatus = status;
  cleanup = loadedCleanup;
  modelPath.placeholder = paths.default_model ?? "";
  if (paths.settings) show($("settings-path"), `Settings file: ${paths.settings}`);
  login.checked = loginEnabled;
  renderGeneral();
  renderCleanup();
  await refreshPermissions();
}

init().catch((err) => show(generalError, String(err), "error"));
