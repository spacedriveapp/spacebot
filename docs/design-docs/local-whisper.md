# Local Whisper: Voice Transcription That Works Out Of The Box

Today, voice notes only work if you've already configured a model. `RoutingConfig::voice` defaults to `String::new()` (`src/llm/routing.rs:56`), and an empty route short-circuits in `transcribe_audio_attachment`:

```
[Audio attachment received but no voice model is configured in routing.voice: voice-message.ogg]
```

That's the first thing a new instance says when someone sends it a voice note, and it's a bad first impression. Worse, the fix isn't obvious — the working models are a short allowlist (`KNOWN_VOICE_TRANSCRIPTION_MODELS` in `src/api/models.rs:83`, currently all Gemini), because the transcription path is an OpenAI-compatible `/v1/chat/completions` call carrying an `input_audio` part. Anthropic endpoints are rejected outright.

This doc adds a local Whisper engine and makes it the default. Cloud routes stay exactly as they are, and become the override rather than the requirement.

The scope is real: this is an ASR engine plus an audio decode pipeline inside the Rust binary, not a defaults tweak.

---

## Target behavior

- A fresh instance transcribes voice notes with no configuration and no API key.
- The model file downloads on first use, like Chrome does for the browser tool.
- `routing.voice` set to a cloud model keeps today's behavior verbatim.
- Steady-state memory returns to baseline when nobody has sent a voice note in a while.

---

## Phase 1 — Audio decode

New module `src/voice.rs` + `src/voice/`, following the existing `foo.rs` + `foo/` layout.

Whisper takes 16 kHz mono `f32` PCM. What actually arrives is nothing like that:

| Source | Container / codec |
|---|---|
| Telegram voice | ogg/opus (`src/messaging/telegram.rs:1347`) |
| Discord voice message | ogg/opus |
| Slack | m4a / mp4 (AAC) |
| Email, uploads | mp3, wav, flac |

`symphonia` 0.6.1 (features `aac`, `isomp4`, `mp3`, `ogg`, `wav`, `flac`, `alac`) covers everything except Opus — symphonia still ships no Opus decoder. Opus is filled separately: the `ogg` crate demuxes the pages, and an Opus decoder turns packets into 48 kHz PCM (see Decision 1).

`src/voice/audio.rs` exposes one entry point:

```rust
/// Decode arbitrary audio bytes to the 16 kHz mono f32 PCM whisper expects.
pub fn decode_to_pcm16k(bytes: &[u8], mime_type: &str, filename: &str) -> Result<Vec<f32>, AudioError>;
```

Internally: probe by MIME with a filename-extension fallback (adapters lie about MIME often enough that the hint matters), decode, downmix to mono by averaging channels, resample to 16 kHz with `rubato` 5.

Bounds are checked before any decoding work: 25 MB and 10 minutes. Anything larger returns an `AudioError` that becomes a text marker in the turn rather than a multi-minute CPU stall on a shared-cpu box.

Tests use short fixture clips — one per container — asserting sample rate, mono, and approximate duration.

## Phase 2 — Whisper engine

`src/voice/whisper.rs`, built on `whisper-rs` 0.16 (bindings to whisper.cpp). Metal is enabled on macOS only:

```toml
[target.'cfg(target_os = "macos")'.dependencies]
whisper-rs = { version = "0.16", features = ["metal"] }

[target.'cfg(not(target_os = "macos"))'.dependencies]
whisper-rs = "0.16"
```

**Model storage.** `{instance_dir}/whisper/ggml-{size}.bin`, fetched from Hugging Face on first use. This mirrors two patterns already in the codebase: the Chrome fetcher (`src/tools/browser.rs:2567`) and the fastembed model cache (`src/main.rs:1044`). Download to a temp file, `rename` into place atomically, and single-flight the whole thing behind a mutex — two voice notes landing in the same second must not both pull 150 MB.

**Lifecycle.** `WhisperEngine` holds:

```rust
pub struct WhisperEngine {
    context: Arc<Mutex<Option<WhisperContext>>>,
    last_used: Arc<Mutex<Instant>>,
    config: VoiceConfig,
}
```

Inference is CPU-bound and blocking, so `transcribe()` wraps it in `spawn_blocking`. The mutex around the context doubles as a work queue — serializing transcription is desirable, since two clips decoding at once on a 2-core box is slower than doing them in order. A background task drops the context after `unload_after_idle_secs` (default 600); the next voice note reloads it from the already-downloaded file, which is fast.

This idle unload isn't optional polish. `fly.toml` provisions `shared-cpu-2x` with **1 gb** of memory, and the `base` model is roughly 300 MB resident alongside LanceDB and fastembed.

**Silence handling.** Whisper hallucinates on silence and background noise — "Thank you.", "[BLANK_AUDIO]", subtitle-credit boilerplate. Without a filter these arrive in the channel as if a human said them, which is materially worse than an empty transcript. Two mitigations, both in `whisper.rs`:

- whisper.cpp's own `no_speech_thold` / `logprob_thold` segment gating
- a small phrase blocklist applied to the final transcript, dropping it to empty when it matches

Params otherwise: `n_threads = min(4, available_parallelism)`, no timestamps, language from config.

## Phase 3 — Wire into the attachment path

`transcribe_audio_attachment` (`src/agent/channel_attachments.rs:190`) gains a route decision at the top:

```rust
let voice_model = routing.voice.trim();
if voice_model.is_empty() || voice_model.starts_with("local/") {
    return transcribe_locally(deps, attachment, bytes).await;
}
// existing input_audio path, unchanged
```

The `<voice_transcript name= mime=>` wrapper is emitted identically by both paths, so nothing downstream in the prompt or the conversation history moves.

Failure handling: if the local engine fails — model download blocked, codec unsupported — and a cloud route is configured, fall through to it. Otherwise emit the existing failure marker. Local failure should never be a dead end when the user has a working cloud model set up.

## Phase 4 — Config and surfaces

**Routing default.** `RoutingConfig::for_model` sets `voice: "local/whisper-base".into()`. That touches `src/llm/routing.rs:56` plus the ~15 test constructors in the same file that spell the struct out literally.

**New `[voice]` section** for the local-only knobs, threaded through `src/config/toml_schema.rs`, `src/config/types.rs`, and `src/config/load.rs`:

```toml
[voice]
model = "base"                  # tiny, base, small, medium, large-v3
language = "auto"               # or "en", "es", ...
threads = 4
unload_after_idle_secs = 600
max_duration_secs = 600
```

The `SPACEBOT_VOICE_MODEL` env override (`src/config/load.rs:1025`) already exists and keeps working — it sets the route, not the local model size.

**Model list.** `src/api/models.rs` injects synthetic entries for `local/whisper-{tiny,base,small,medium,large-v3}` with `input_audio: true`, and `is_known_voice_transcription_model` accepts the `local/` prefix. This is what makes them selectable in the dashboard dropdown — `ConfigSectionEditor.tsx:216` filters the voice row on capability `voice_transcription` — and `spacebot model list --capability voice_transcription` picks them up with no CLI changes.

## Phase 5 — Build and packaging

whisper-rs needs cmake, a C++ compiler, and libclang for bindgen. The first two are already present everywhere; libclang is not.

- **`Dockerfile`** — the builder installs `cmake` and inherits g++ from `rust:bookworm`. Add `clang`/`libclang-dev`, or set `WHISPER_DONT_GENERATE_BINDINGS=1` to use the crate's pre-generated bindings and skip bindgen entirely.
- **`Dockerfile.cross-aarch64`** — use `WHISPER_DONT_GENERATE_BINDINGS=1` here regardless; cross-compiling bindgen is not worth the maintenance. Set `CXX_aarch64_unknown_linux_gnu=aarch64-linux-gnu-g++` (the toolchain is already installed) and force `GGML_NATIVE=OFF` so cmake doesn't emit `-march=native` for the host arch.
- **`flake.nix`** — both devShells carry cmake but no clang. Add `llvmPackages.libclang` and `LIBCLANG_PATH`.

Clean builds gain a couple of minutes for whisper.cpp. `whisper-rs-sys` rebuilds only when it changes, so incremental builds are unaffected.

## Phase 6 — Docs

Voice page under `docs/content`, a README line noting transcription works with no configuration, and a CHANGELOG entry.

---

## Open decisions

**1. Opus decoder.** `opus-decoder` 0.1.1 is pure Rust, no unsafe, no FFI, RFC 8251 conformant, ~72k recent downloads — but it's five months old and single-author. The alternative is libopus through `audiopus_sys`, which is C but has been decoding the world's voice traffic for a decade, and we're already accepting a C++ toolchain for whisper.cpp. Recommendation: **libopus**. Opus bugs surface as subtly garbled transcripts, which is a miserable class of bug to chase in production.

**2. Default model size.** `base` is the right quality floor for voice notes, at ~150 MB on disk and ~300 MB resident. Idle unload bounds steady-state memory on the 1 gb fly box, but peak still lands during transcription. If that's too tight, default `base` locally and pin `tiny` or a q5_1 quant in the fly config.

**3. Default language.** `auto` is the honest default, but Whisper's language detection is unreliable on short clips and misdetection reads as a garbled transcript rather than a wrong-language one. Forcing `"en"` measurably reduces garbage for an English-speaking instance. Recommendation: default `auto`, document the tradeoff, make it a one-line config change.

---

## Out of scope

The desktop voice overlay (`interface/src/routes/Overlay.tsx`, `interface/src/hooks/useAudioRecorder.ts`) is still a stub with no server-side transcribe endpoint. Once this lands, that endpoint is a thin wrapper over the same engine — worth doing, but as its own change.
