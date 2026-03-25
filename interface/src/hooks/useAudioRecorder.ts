// Stub implementations for voice overlay hooks
// TODO: Implement actual audio recording and TTS playback

export type AudioRecorderStatus = "idle" | "recording" | "processing";

export function useAudioRecorder() {
	return {
		state: "idle" as AudioRecorderStatus,
		startRecording: () => Promise.resolve(),
		stopRecording: () => Promise.resolve(new Blob()),
		audioLevel: 0,
		spectrumLevels: [] as number[],
	};
}

export interface TtsProfile {
	id: string;
	name: string;
}

export type TtsStatus = "idle" | "playing";

export function useTtsPlayback(_agentId?: string, _profileId?: string) {
	return {
		speak: (_text: string, _onDone?: () => void) => Promise.resolve(),
		stop: () => {},
		playbackLevel: 0,
		spectrumLevels: [] as number[],
		status: "idle" as TtsStatus,
	};
}

export async function fetchTtsProfiles(_agentId: string): Promise<TtsProfile[]> {
	return [];
}
