import {useState, useRef, useCallback} from "react";
import {api} from "@/api/client";

export type PlaybackState = "idle" | "loading" | "playing";

interface UseTtsPlaybackReturn {
	state: PlaybackState;
	/** Synthesize and play spoken text via Voicebox TTS. */
	speak: (text: string, agentId?: string, profileId?: string) => Promise<void>;
	/** Stop playback immediately. */
	stop: () => void;
	/** Current playback energy level (0-1) for visualization. */
	playbackLevel: number;
	/** Frequency-band levels (0-1) for spectral bar visualization. */
	spectrumLevels: number[];
}

const SPECTRUM_BAR_COUNT = 45;

function buildSpectrumLevels(
	frequencyData: Uint8Array,
	previousLevels: number[],
): number[] {
	return Array.from({length: SPECTRUM_BAR_COUNT}, (_, index) => {
		const start = Math.floor((index * frequencyData.length) / SPECTRUM_BAR_COUNT);
		const end = Math.floor(((index + 1) * frequencyData.length) / SPECTRUM_BAR_COUNT);
		let sum = 0;
		for (let frequencyIndex = start; frequencyIndex < end; frequencyIndex += 1) {
			sum += frequencyData[frequencyIndex] ?? 0;
		}
		const binCount = Math.max(1, end - start);
		const average = sum / binCount / 255;
		const boosted = Math.min(1, average * 2.4);
		const previous = previousLevels[index] ?? 0;
		return previous * 0.52 + boosted * 0.48;
	});
}

/**
 * Hook for TTS playback via the Voicebox proxy endpoint.
 * Fetches audio from /api/tts/generate and plays it via the Web Audio API.
 */
export function useTtsPlayback(): UseTtsPlaybackReturn {
	const [state, setState] = useState<PlaybackState>("idle");
	const [playbackLevel, setPlaybackLevel] = useState(0);
	const [spectrumLevels, setSpectrumLevels] = useState<number[]>(() =>
		Array.from({length: SPECTRUM_BAR_COUNT}, () => 0),
	);
	const audioContextRef = useRef<AudioContext | null>(null);
	const sourceRef = useRef<AudioBufferSourceNode | null>(null);
	const analyserRef = useRef<AnalyserNode | null>(null);
	const animationFrameRef = useRef<number>(0);
	const playbackDoneRef = useRef<(() => void) | null>(null);
	const requestIdRef = useRef(0);
	const smoothedLevelRef = useRef(0);
	const smoothedSpectrumRef = useRef<number[]>(
		Array.from({length: SPECTRUM_BAR_COUNT}, () => 0),
	);

	const stopMetering = useCallback(() => {
		cancelAnimationFrame(animationFrameRef.current);
		analyserRef.current = null;
		smoothedLevelRef.current = 0;
		setPlaybackLevel(0);
		smoothedSpectrumRef.current = Array.from({length: SPECTRUM_BAR_COUNT}, () => 0);
		setSpectrumLevels(smoothedSpectrumRef.current);
	}, []);

	const stop = useCallback(() => {
		requestIdRef.current += 1;
		if (sourceRef.current) {
			try {
				sourceRef.current.stop();
			} catch { /* already stopped */ }
			sourceRef.current = null;
		}
		if (playbackDoneRef.current) {
			playbackDoneRef.current();
			playbackDoneRef.current = null;
		}
		stopMetering();
		setState("idle");
	}, [stopMetering]);

	const speak = useCallback(async (text: string, agentId?: string, profileId?: string) => {
		// Stop any current playback
		stop();

		if (!text.trim()) return;

		const requestId = ++requestIdRef.current;
		const isCurrentRequest = () => requestIdRef.current === requestId;
		const abandonIfStale = (source?: AudioBufferSourceNode, analyser?: AnalyserNode) => {
			if (isCurrentRequest()) {
				return false;
			}
			if (source) {
				try {
					source.stop();
				} catch {
					// no-op: source was never started or already stopped
				}
			}
			if (sourceRef.current === source) {
				sourceRef.current = null;
			}
			if (analyserRef.current === analyser) {
				analyserRef.current = null;
			}
			playbackDoneRef.current = null;
			stopMetering();
			setState("idle");
			return true;
		};

		setState("loading");

		try {
			const audioBuffer = await api.ttsGenerate(text, {agentId, profileId});
			if (abandonIfStale()) return;

			// Decode the audio data
			if (!audioContextRef.current || audioContextRef.current.state === "closed") {
				audioContextRef.current = new AudioContext();
			}
			const context = audioContextRef.current;
			if (context.state === "suspended") {
				await context.resume();
				if (abandonIfStale()) return;
			}

			const decoded = await context.decodeAudioData(audioBuffer);
			if (abandonIfStale()) return;

			// Play the audio
			const source = context.createBufferSource();
			const analyser = context.createAnalyser();
			analyser.fftSize = 256;
			analyser.smoothingTimeConstant = 0.78;
			analyserRef.current = analyser;
			source.buffer = decoded;
			source.connect(analyser);
			analyser.connect(context.destination);
			sourceRef.current = source;

			const dataArray = new Float32Array(analyser.fftSize);
			const frequencyData = new Uint8Array(analyser.frequencyBinCount);
			const tick = () => {
				if (!analyserRef.current) return;
				analyser.getFloatTimeDomainData(dataArray);
				analyser.getByteFrequencyData(frequencyData);
				let sumSquares = 0;
				for (const sample of dataArray) {
					sumSquares += sample * sample;
				}
				const rms = Math.sqrt(sumSquares / dataArray.length);
				const normalized = Math.min(1, Math.max(0, rms - 0.01) * 7.5);
				const smoothed = smoothedLevelRef.current * 0.68 + normalized * 0.32;
				smoothedLevelRef.current = smoothed;
				setPlaybackLevel(smoothed);

				const nextSpectrumLevels = buildSpectrumLevels(
					frequencyData,
					smoothedSpectrumRef.current,
				);
				smoothedSpectrumRef.current = nextSpectrumLevels;
				setSpectrumLevels(nextSpectrumLevels);
				animationFrameRef.current = requestAnimationFrame(tick);
			};
			tick();
			if (abandonIfStale(source, analyser)) return;

			await new Promise<void>((resolve) => {
				if (abandonIfStale(source, analyser)) {
					resolve();
					return;
				}
				playbackDoneRef.current = resolve;
				source.onended = () => {
					if (!isCurrentRequest()) {
						resolve();
						return;
					}
					sourceRef.current = null;
					playbackDoneRef.current = null;
					stopMetering();
					setState("idle");
					resolve();
				};

				if (abandonIfStale(source, analyser)) {
					resolve();
					return;
				}
				source.start();
				setState("playing");
			});
		} catch (error) {
			if (!isCurrentRequest()) {
				return;
			}
			console.error("TTS playback failed:", error);
			playbackDoneRef.current = null;
			stopMetering();
			setState("idle");
		}
	}, [stop, stopMetering]);

	return {state, speak, stop, playbackLevel, spectrumLevels};
}
