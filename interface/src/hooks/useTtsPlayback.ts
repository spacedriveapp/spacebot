// Stub implementation for TTS playback hook
// TODO: Implement actual TTS playback

export interface TtsProfile {
	id: string;
	name: string;
}

export type TtsStatus = "idle" | "playing";

export function useTtsPlayback(_agentId?: string, _profileId?: string) {
	return {
		speak: (_text: string, _agentId?: string, _profileId?: string) => Promise.resolve(),
		stop: () => {},
		playbackLevel: 0,
		spectrumLevels: [] as number[],
		status: "idle" as TtsStatus,
	};
}

export async function fetchTtsProfiles(_agentId: string): Promise<TtsProfile[]> {
	return [];
}
