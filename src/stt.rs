//! Local Whisper speech-to-text via whisper-rs.
//!
//! Only compiled when the `stt-whisper` feature is enabled.
//! Exposed as a single async `transcribe` function that lazily loads and caches
//! the model context for the lifetime of the process.

#[cfg(feature = "stt-whisper")]
pub use local::transcribe;

#[cfg(feature = "stt-whisper")]
mod local {
    use std::sync::OnceLock;

    use hf_hub::api::sync::Api;
    use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

    /// Known model size names and their GGML filenames on `ggerganov/whisper.cpp`.
    const KNOWN_SIZES: &[(&str, &str)] = &[
        ("tiny", "ggml-tiny.bin"),
        ("tiny.en", "ggml-tiny.en.bin"),
        ("base", "ggml-base.bin"),
        ("base.en", "ggml-base.en.bin"),
        ("small", "ggml-small.bin"),
        ("small.en", "ggml-small.en.bin"),
        ("medium", "ggml-medium.bin"),
        ("medium.en", "ggml-medium.en.bin"),
        ("large", "ggml-large-v3.bin"),
        ("large-v1", "ggml-large-v1.bin"),
        ("large-v2", "ggml-large-v2.bin"),
        ("large-v3", "ggml-large-v3.bin"),
    ];

    /// Cached (model_spec, WhisperContext) — one per process.
    ///
    /// If the user changes `routing.voice` at runtime we just keep using the
    /// already-loaded model; a restart is required to switch models.
    static CONTEXT: OnceLock<(String, WhisperContext)> = OnceLock::new();

    #[derive(Debug, thiserror::Error)]
    pub enum WhisperError {
        #[error("model not found and could not be downloaded: {0}")]
        ModelNotFound(String),
        #[error("hf-hub error: {0}")]
        HfHub(String),
        #[error("failed to load whisper model: {0}")]
        Load(String),
        #[error("failed to create whisper state: {0}")]
        State(String),
        #[error("transcription failed: {0}")]
        Transcription(String),
        #[error("audio decode error: {0}")]
        Decode(String),
    }

    /// Transcribe raw audio bytes using the local Whisper model.
    ///
    /// `model_spec` is the part after `whisper-local://`:
    /// - A known size name (`small`, `medium`, `large`, …) — downloaded from HF
    ///   into the HF cache on first use.
    /// - An absolute path (`/path/to/ggml-small.bin`) — loaded directly.
    pub async fn transcribe(model_spec: &str, audio: &[u8]) -> Result<String, WhisperError> {
        let model_spec = model_spec.to_owned();
        let audio = audio.to_vec();

        // Whisper inference is CPU-bound and blocking — run on a thread pool.
        tokio::task::spawn_blocking(move || transcribe_blocking(&model_spec, &audio))
            .await
            .map_err(|e| WhisperError::Transcription(e.to_string()))?
    }

    fn transcribe_blocking(model_spec: &str, audio: &[u8]) -> Result<String, WhisperError> {
        let ctx = get_or_load_context(model_spec)?;

        let mut state = ctx
            .create_state()
            .map_err(|e| WhisperError::State(e.to_string()))?;

        let samples = decode_to_f32(audio)?;

        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        params.set_language(Some("auto"));
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);

        state
            .full(params, &samples)
            .map_err(|e| WhisperError::Transcription(e.to_string()))?;

        let n = state.full_n_segments();
        let mut parts = Vec::with_capacity(n as usize);
        for i in 0..n {
            if let Some(segment) = state.get_segment(i) {
                if let Ok(text) = segment.to_str() {
                    let trimmed = text.trim();
                    if !trimmed.is_empty() {
                        parts.push(trimmed.to_owned());
                    }
                }
            }
        }

        Ok(parts.join(" "))
    }

    /// Return the cached context, loading it first if necessary.
    fn get_or_load_context(model_spec: &str) -> Result<&'static WhisperContext, WhisperError> {
        if let Some((_, ctx)) = CONTEXT.get() {
            return Ok(ctx);
        }

        let model_path = resolve_model_path(model_spec)?;

        tracing::info!(model_path = %model_path, "loading local Whisper model");

        let params = WhisperContextParameters::default();
        let ctx = WhisperContext::new_with_params(&model_path, params)
            .map_err(|e| WhisperError::Load(e.to_string()))?;

        let _ = CONTEXT.set((model_spec.to_owned(), ctx));

        tracing::info!(model_path = %model_path, "Whisper model loaded and cached");

        Ok(&CONTEXT.get().unwrap().1)
    }

    /// Resolve a model spec to an absolute path on disk, downloading via hf-hub if needed.
    fn resolve_model_path(spec: &str) -> Result<String, WhisperError> {
        // Absolute path — use directly.
        if spec.starts_with('/') {
            if std::path::Path::new(spec).exists() {
                return Ok(spec.to_owned());
            }
            return Err(WhisperError::ModelNotFound(format!(
                "model file not found: {spec}"
            )));
        }

        // Known size name — fetch via hf-hub (uses HF_HOME cache, downloads if missing).
        let filename = KNOWN_SIZES
            .iter()
            .find(|(name, _)| *name == spec)
            .map(|(_, file)| *file)
            .ok_or_else(|| {
                WhisperError::ModelNotFound(format!(
                    "unknown model size '{spec}'; use one of: {}",
                    KNOWN_SIZES
                        .iter()
                        .map(|(n, _)| *n)
                        .collect::<Vec<_>>()
                        .join(", ")
                ))
            })?;

        tracing::info!(model = %spec, filename = %filename, "fetching Whisper model via hf-hub");

        let api = Api::new().map_err(|e| WhisperError::HfHub(e.to_string()))?;
        let repo = api.model("ggerganov/whisper.cpp".to_owned());
        let path = repo
            .get(filename)
            .map_err(|e| WhisperError::HfHub(e.to_string()))?;

        Ok(path.to_string_lossy().to_string())
    }

    /// Decode arbitrary audio bytes to 16 kHz mono f32 samples for Whisper.
    ///
    /// Ogg/Opus (Telegram voice messages) is handled directly via the `ogg` +
    /// `opus` crates. Everything else falls through to symphonia.
    fn decode_to_f32(audio: &[u8]) -> Result<Vec<f32>, WhisperError> {
        if is_ogg_opus(audio) {
            return decode_ogg_opus(audio);
        }

        use symphonia::core::codecs::DecoderOptions;
        use symphonia::core::formats::FormatOptions;
        use symphonia::core::io::MediaSourceStream;
        use symphonia::core::meta::MetadataOptions;
        use symphonia::core::probe::Hint;

        let cursor = std::io::Cursor::new(audio.to_vec());
        let mss = MediaSourceStream::new(Box::new(cursor), Default::default());

        let probed = symphonia::default::get_probe()
            .format(
                &Hint::new(),
                mss,
                &FormatOptions::default(),
                &MetadataOptions::default(),
            )
            .map_err(|e| WhisperError::Decode(e.to_string()))?;

        let mut format = probed.format;
        let track = format
            .tracks()
            .iter()
            .find(|t| {
                t.codec_params.codec != symphonia::core::codecs::CODEC_TYPE_NULL
            })
            .ok_or_else(|| WhisperError::Decode("no audio track found".into()))?
            .clone();

        let mut decoder = symphonia::default::get_codecs()
            .make(&track.codec_params, &DecoderOptions::default())
            .map_err(|e| WhisperError::Decode(e.to_string()))?;

        let track_id = track.id;
        let sample_rate = track.codec_params.sample_rate.unwrap_or(16000);
        let channels = track
            .codec_params
            .channels
            .map(|c| c.count())
            .unwrap_or(1);

        let mut raw_samples: Vec<f32> = Vec::new();

        loop {
            let packet = match format.next_packet() {
                Ok(p) => p,
                Err(symphonia::core::errors::Error::IoError(_)) => break,
                Err(symphonia::core::errors::Error::ResetRequired) => break,
                Err(e) => return Err(WhisperError::Decode(e.to_string())),
            };

            if packet.track_id() != track_id {
                continue;
            }

            let decoded = decoder
                .decode(&packet)
                .map_err(|e| WhisperError::Decode(e.to_string()))?;

            // Convert to f32 mono using a sample-converting audio buffer.
            use symphonia::core::audio::{AudioBuffer, Signal as _};

            let mut f32_buf: AudioBuffer<f32> = AudioBuffer::new(
                decoded.capacity() as u64,
                decoded.spec().clone(),
            );
            decoded.convert(&mut f32_buf);

            // Mix down to mono.
            let frames = f32_buf.frames();
            for frame in 0..frames {
                let mut sum = 0f32;
                for ch in 0..channels {
                    sum += f32_buf.chan(ch)[frame];
                }
                raw_samples.push(sum / channels as f32);
            }
        }

        // Resample to 16 kHz if needed.
        if sample_rate != 16000 {
            raw_samples = resample(raw_samples, sample_rate, 16000);
        }

        Ok(raw_samples)
    }

    /// Check if the audio is an Ogg container with an Opus stream.
    fn is_ogg_opus(audio: &[u8]) -> bool {
        // OggS capture pattern at offset 0, and OpusHead magic at offset 28
        // (first packet of the first logical stream).
        audio.starts_with(b"OggS") && audio.len() > 36 && &audio[28..36] == b"OpusHead"
    }

    /// Decode Ogg/Opus audio to 16 kHz mono f32 samples.
    fn decode_ogg_opus(audio: &[u8]) -> Result<Vec<f32>, WhisperError> {
        use ogg::reading::PacketReader;

        let cursor = std::io::Cursor::new(audio);
        let mut reader = PacketReader::new(cursor);

        // Skip the OpusHead and OpusTags header packets.
        let mut header_packets = 0;
        let mut decoder: Option<opus::Decoder> = None;
        let mut sample_rate = 48000u32;
        let mut channels = 1usize;
        let mut samples: Vec<f32> = Vec::new();

        while let Ok(Some(packet)) = reader.read_packet() {
            if header_packets < 2 {
                if header_packets == 0 {
                    // Parse OpusHead to get channel count and pre-skip.
                    if packet.data.len() >= 11 && &packet.data[0..8] == b"OpusHead" {
                        channels = packet.data[9] as usize;
                        // Output sample rate is always 48000 for libopus.
                        sample_rate = 48000;
                    }
                    decoder = Some(
                        opus::Decoder::new(sample_rate, if channels == 2 {
                            opus::Channels::Stereo
                        } else {
                            opus::Channels::Mono
                        })
                        .map_err(|e| WhisperError::Decode(e.to_string()))?,
                    );
                }
                header_packets += 1;
                continue;
            }

            let dec = decoder.as_mut().unwrap();
            // Max Opus frame: 120ms at 48kHz = 5760 samples per channel.
            let max_samples = 5760 * channels;
            let mut pcm = vec![0f32; max_samples];
            let n = dec
                .decode_float(&packet.data, &mut pcm, false)
                .map_err(|e| WhisperError::Decode(e.to_string()))?;

            // Mix down to mono.
            if channels == 1 {
                samples.extend_from_slice(&pcm[..n]);
            } else {
                for frame in 0..n {
                    let mut sum = 0f32;
                    for ch in 0..channels {
                        sum += pcm[frame * channels + ch];
                    }
                    samples.push(sum / channels as f32);
                }
            }
        }

        // Resample from 48 kHz to 16 kHz.
        Ok(resample(samples, sample_rate, 16000))
    }

    /// Simple linear resampler (good enough for speech; not for music).
    fn resample(samples: Vec<f32>, from_hz: u32, to_hz: u32) -> Vec<f32> {
        if from_hz == to_hz {
            return samples;
        }
        let ratio = from_hz as f64 / to_hz as f64;
        let out_len = (samples.len() as f64 / ratio) as usize;
        let mut out = Vec::with_capacity(out_len);
        for i in 0..out_len {
            let pos = i as f64 * ratio;
            let idx = pos as usize;
            let frac = (pos - idx as f64) as f32;
            let a = samples.get(idx).copied().unwrap_or(0.0);
            let b = samples.get(idx + 1).copied().unwrap_or(0.0);
            out.push(a + frac * (b - a));
        }
        out
    }
}
