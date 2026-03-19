import {useState, useRef, useCallback} from "react";

export type RecordingState = "idle" | "recording" | "stopping";

interface UseAudioRecorderReturn {
	state: RecordingState;
	/** Start recording from the microphone. */
	startRecording: () => Promise<void>;
	/** Stop recording and return the audio blob. */
	stopRecording: () => Promise<Blob | null>;
	/** Current audio level (0-1) for visualization. Updated ~60fps while recording. */
	audioLevel: number;
}

/**
 * Hook for recording audio from the user's microphone via MediaRecorder.
 * Returns a WebM/Opus blob suitable for upload to /api/webchat/send-audio.
 */
export function useAudioRecorder(): UseAudioRecorderReturn {
	const [state, setState] = useState<RecordingState>("idle");
	const [audioLevel, setAudioLevel] = useState(0);

	const mediaRecorderRef = useRef<MediaRecorder | null>(null);
	const chunksRef = useRef<Blob[]>([]);
	const streamRef = useRef<MediaStream | null>(null);
	const analyserRef = useRef<AnalyserNode | null>(null);
	const animFrameRef = useRef<number>(0);
	const resolveStopRef = useRef<((blob: Blob | null) => void) | null>(null);

	const startRecording = useCallback(async () => {
		if (state !== "idle") return;

		try {
			const stream = await navigator.mediaDevices.getUserMedia({
				audio: {
					echoCancellation: true,
					noiseSuppression: true,
					autoGainControl: true,
				},
			});
			streamRef.current = stream;

			// Set up audio analyser for level visualization
			const audioContext = new AudioContext();
			const source = audioContext.createMediaStreamSource(stream);
			const analyser = audioContext.createAnalyser();
			analyser.fftSize = 256;
			source.connect(analyser);
			analyserRef.current = analyser;

			// Start level monitoring
			const dataArray = new Uint8Array(analyser.frequencyBinCount);
			const updateLevel = () => {
				analyser.getByteFrequencyData(dataArray);
				const average = dataArray.reduce((sum, value) => sum + value, 0) / dataArray.length;
				setAudioLevel(Math.min(average / 128, 1));
				animFrameRef.current = requestAnimationFrame(updateLevel);
			};
			updateLevel();

			// Configure MediaRecorder
			const mimeType = MediaRecorder.isTypeSupported("audio/webm;codecs=opus")
				? "audio/webm;codecs=opus"
				: "audio/webm";

			const recorder = new MediaRecorder(stream, {mimeType});
			chunksRef.current = [];

			recorder.ondataavailable = (event) => {
				if (event.data.size > 0) {
					chunksRef.current.push(event.data);
				}
			};

			recorder.onstop = () => {
				const blob = new Blob(chunksRef.current, {type: mimeType});
				chunksRef.current = [];

				// Clean up
				cancelAnimationFrame(animFrameRef.current);
				setAudioLevel(0);
				stream.getTracks().forEach((track) => track.stop());
				streamRef.current = null;
				analyserRef.current = null;
				audioContext.close();

				setState("idle");

				if (resolveStopRef.current) {
					resolveStopRef.current(blob);
					resolveStopRef.current = null;
				}
			};

			mediaRecorderRef.current = recorder;
			recorder.start(100); // Collect data every 100ms
			setState("recording");
		} catch (error) {
			console.error("Failed to start recording:", error);
			setState("idle");
		}
	}, [state]);

	const stopRecording = useCallback((): Promise<Blob | null> => {
		return new Promise((resolve) => {
			const recorder = mediaRecorderRef.current;
			if (!recorder || recorder.state !== "recording") {
				resolve(null);
				return;
			}

			setState("stopping");
			resolveStopRef.current = resolve;
			recorder.stop();
		});
	}, []);

	return {state, startRecording, stopRecording, audioLevel};
}
