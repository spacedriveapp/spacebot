import {useState, useRef, useCallback} from "react";
import {api} from "@/api/client";

export type PlaybackState = "idle" | "loading" | "playing";

interface UseTtsPlaybackReturn {
	state: PlaybackState;
	/** Synthesize and play spoken text via Voicebox TTS. */
	speak: (text: string, agentId?: string, profileId?: string) => Promise<void>;
	/** Stop playback immediately. */
	stop: () => void;
}

/**
 * Hook for TTS playback via the Voicebox proxy endpoint.
 * Fetches audio from /api/tts/generate and plays it via the Web Audio API.
 */
export function useTtsPlayback(): UseTtsPlaybackReturn {
	const [state, setState] = useState<PlaybackState>("idle");
	const audioContextRef = useRef<AudioContext | null>(null);
	const sourceRef = useRef<AudioBufferSourceNode | null>(null);

	const stop = useCallback(() => {
		if (sourceRef.current) {
			try {
				sourceRef.current.stop();
			} catch { /* already stopped */ }
			sourceRef.current = null;
		}
		setState("idle");
	}, []);

	const speak = useCallback(async (text: string, agentId?: string, profileId?: string) => {
		// Stop any current playback
		stop();

		if (!text.trim()) return;

		setState("loading");

		try {
			const audioBuffer = await api.ttsGenerate(text, {agentId, profileId});

			// Decode the audio data
			if (!audioContextRef.current || audioContextRef.current.state === "closed") {
				audioContextRef.current = new AudioContext();
			}
			const context = audioContextRef.current;

			const decoded = await context.decodeAudioData(audioBuffer);

			// Play the audio
			const source = context.createBufferSource();
			source.buffer = decoded;
			source.connect(context.destination);
			sourceRef.current = source;

			source.onended = () => {
				sourceRef.current = null;
				setState("idle");
			};

			source.start();
			setState("playing");
		} catch (error) {
			console.error("TTS playback failed:", error);
			setState("idle");
		}
	}, [stop]);

	return {state, speak, stop};
}
