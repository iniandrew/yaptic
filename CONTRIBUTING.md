# Contributing to Yaptic

Thanks for helping. Bug reports, fixes, and small focused improvements are
all welcome. For anything large, open an issue first so we can agree on the
approach before you spend time on it.

## Setup

You need a Mac with Apple Silicon and:

```bash
xcode-select --install
brew install rust node cmake
git clone https://github.com/iniandrew/yaptic.git
cd yaptic
npm install
```

## Run in development

```bash
npm run tauri dev
```

The first build compiles whisper.cpp and takes a few minutes. The first launch
after a build also spends a few seconds compiling Metal shaders.

**Permissions in development.** macOS grants Microphone and Accessibility to
the app that launched the process, which is your terminal (Terminal, iTerm,
Warp), not Yaptic. Grant them there. If the shortcut or paste silently does
nothing, check this first.

The speech model downloads to
`~/Library/Application Support/io.github.iniandrew.yaptic/models/`. To skip
the in-app download, fetch it yourself:

```bash
mkdir -p "$HOME/Library/Application Support/io.github.iniandrew.yaptic/models"
cd "$HOME/Library/Application Support/io.github.iniandrew.yaptic/models"
curl -L -C - -o ggml-large-v3-turbo-q5_0.bin \
  https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo-q5_0.bin
```

Debug builds print transcripts to the terminal and save the last recording
to `/tmp/yaptic-last.wav` (readable only by you). Release builds do neither.

## Checks before a pull request

```bash
cd src-tauri
cargo test --lib
cargo clippy --all-targets
cd ..
npx tsc --noEmit
```

Tests run offline. The cleanup providers are tested against a local mock
HTTP server, and the clipboard test uses a private pasteboard, so neither
touches the network or your clipboard.

If you add, remove, or update a dependency, regenerate the license list and
commit the result:

```bash
scripts/third-party-licenses.sh
```

## Project layout

```
src-tauri/src/
  lib.rs          app setup, commands registration
  tray.rs         menu bar icon and menu
  hotkey.rs       push-to-talk shortcut
  audio.rs        microphone capture, resampling to 16 kHz
  stt.rs          Transcriber trait + WhisperTranscriber
  model.rs        model download, resume, SHA-256 check
  cleanup/        Cleaner trait, Anthropic and OpenAI-compatible providers
  paste.rs        clipboard swap, Cmd+V, clipboard restore
  pipeline.rs     record -> transcribe -> clean up -> paste
  pill.rs         the floating status pill
  settings.rs     settings file and Keychain keys
  permissions.rs  Microphone and Accessibility status
  login.rs        launch at login
src/              settings window and pill (TypeScript, no framework)
```

The pipeline only talks to the `Transcriber` and `Cleaner` traits. Adding a
speech engine or a cleanup provider means implementing one of them; the
pipeline code stays as it is.

## Guidelines

- Keep changes focused; one concern per pull request.
- Ask in an issue before adding a new dependency.
- Never log transcripts, request bodies, or API keys in release builds.
- Never commit model files, `.env` files, or keys.
- Write commit messages that say what changed and why.

By contributing, you agree that your contributions are licensed under the
MIT License.
