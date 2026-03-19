import {useState, useEffect, useCallback, useRef, lazy, Suspense} from "react";
import {useQuery} from "@tanstack/react-query";
import {api} from "@/api/client";
import {useAudioRecorder} from "@/hooks/useAudioRecorder";
import {useTtsPlayback} from "@/hooks/useTtsPlayback";
import {getPortalChatSessionId} from "@/hooks/useWebChat";
import {useEventSource} from "@/hooks/useEventSource";
import {cx} from "@/ui/utils";

const Orb = lazy(() => import("@/components/Orb"));

// ── Types ────────────────────────────────────────────────────────────────

type VoiceState = "idle" | "recording" | "processing" | "speaking";

interface SpokenResponseEvent {
	agent_id: string;
	channel_id: string;
	spoken_text: string;
	full_text: string;
}

// ── Component ────────────────────────────────────────────────────────────

export function Overlay() {
	const [expanded, setExpanded] = useState(false);
	const [voiceState, setVoiceState] = useState<VoiceState>("idle");
	const [agentId] = useState("main"); // default agent
	const [profileId, setProfileId] = useState<string>(() => localStorage.getItem("spacebot.voice.profileId") ?? "");
	const [statusText, setStatusText] = useState("Press Option+Space to talk");
	const [, setSpokenText] = useState<string | null>(null);
	const [transcript, setTranscript] = useState<Array<{role: string; text: string}>>([]);

	const sessionId = getPortalChatSessionId(agentId);
	const {state: recorderState, startRecording, stopRecording, audioLevel} = useAudioRecorder();
	const {speak, stop: stopTts} = useTtsPlayback();
	const pendingSpokenRef = useRef<string | null>(null);
	const spokenReceivedRef = useRef(false);
	const ttsStartedRef = useRef(false);

	const profilesQuery = useQuery({
		queryKey: ["tts-profiles", agentId],
		queryFn: () => api.ttsProfiles(agentId),
		staleTime: 60_000,
	});

	useEffect(() => {
		if (!profilesQuery.data?.length) return;
		if (profileId && profilesQuery.data.some((profile) => profile.id === profileId)) return;
		const firstProfileId = profilesQuery.data[0]?.id ?? "";
		if (firstProfileId) {
			setProfileId(firstProfileId);
			localStorage.setItem("spacebot.voice.profileId", firstProfileId);
		}
	}, [profilesQuery.data, profileId]);

	// -- SSE: listen for spoken_response + outbound_message events --
	const handleSpokenResponse = useCallback((data: unknown) => {
		const event = data as SpokenResponseEvent;
		if (event.channel_id !== sessionId) return;

		spokenReceivedRef.current = true;
		if (ttsStartedRef.current) {
			// We've already started playback for this turn via fallback.
			// Keep the transcript update, but don't trigger overlapping audio.
			setTranscript((prev) => [...prev, {role: "assistant", text: event.full_text}]);
			return;
		}

		setSpokenText(event.spoken_text);
		setVoiceState("speaking");
		setStatusText(event.spoken_text);

		// Add full text to transcript
		setTranscript((prev) => [...prev, {role: "assistant", text: event.full_text}]);

		// Play TTS
		ttsStartedRef.current = true;
		speak(event.spoken_text, agentId, profileId).then(() => {
			ttsStartedRef.current = false;
			setVoiceState("idle");
			setStatusText("Press Option+Space to talk");
		});
	}, [sessionId, agentId, speak, profileId]);

	const handleOutboundMessage = useCallback((data: unknown) => {
		const event = data as {agent_id: string; channel_id: string; text: string};
		if (event.channel_id !== sessionId) return;

		// Show the full reply in the transcript immediately, but only speak when
		// a dedicated spoken_response event arrives from the backend.
		if (voiceState === "processing") {
			setStatusText(event.text.slice(0, 120) + (event.text.length > 120 ? "..." : ""));
			setTranscript((prev) => [...prev, {role: "assistant", text: event.text}]);
		}
	}, [sessionId, voiceState]);

	const handleTypingState = useCallback((data: unknown) => {
		const event = data as {channel_id: string; is_typing: boolean};
		if (event.channel_id !== sessionId) return;

		if (event.is_typing && voiceState === "processing") {
			setStatusText("Thinking...");
		}
	}, [sessionId, voiceState]);

	useEventSource(api.getEventsUrl(), {
		handlers: {
			spoken_response: handleSpokenResponse,
			outbound_message: handleOutboundMessage,
			typing_state: handleTypingState,
		},
	});

	// -- Recording flow --
	const handleStartRecording = useCallback(async () => {
		if (voiceState !== "idle") return;
		stopTts();
		setVoiceState("recording");
		setStatusText("Listening...");
		setSpokenText(null);
		spokenReceivedRef.current = false;
		ttsStartedRef.current = false;
		pendingSpokenRef.current = null;
		await startRecording();
	}, [voiceState, startRecording, stopTts]);

	const handleStopRecording = useCallback(async () => {
		if (recorderState !== "recording") return;
		setVoiceState("processing");
		setStatusText("Processing...");

		const blob = await stopRecording();
		if (!blob || blob.size === 0) {
			setVoiceState("idle");
			setStatusText("Press Option+Space to talk");
			return;
		}

		// Add placeholder to transcript
		setTranscript((prev) => [...prev, {role: "user", text: "[voice message]"}]);

		try {
			const response = await api.webChatSendAudio(agentId, sessionId, blob);
			if (!response.ok) throw new Error(`HTTP ${response.status}`);
			// Now waiting for SSE events (typing_state, outbound_message, spoken_response)
		} catch (error) {
			console.error("Failed to send audio:", error);
			setVoiceState("idle");
			setStatusText("Failed to send. Try again.");
			setTimeout(() => setStatusText("Press Option+Space to talk"), 3000);
		}
	}, [recorderState, stopRecording, agentId, sessionId]);

	// -- Keyboard shortcut (for web mode, Tauri uses global shortcut) --
	useEffect(() => {
		const handleKeyDown = (event: KeyboardEvent) => {
			if (event.code === "Space" && event.altKey && voiceState === "idle") {
				event.preventDefault();
				handleStartRecording();
			}
		};
		const handleKeyUp = (event: KeyboardEvent) => {
			if (event.code === "Space" && voiceState === "recording") {
				event.preventDefault();
				handleStopRecording();
			}
		};

		window.addEventListener("keydown", handleKeyDown);
		window.addEventListener("keyup", handleKeyUp);
		return () => {
			window.removeEventListener("keydown", handleKeyDown);
			window.removeEventListener("keyup", handleKeyUp);
		};
	}, [voiceState, handleStartRecording, handleStopRecording]);

	// -- Audio level visualization dots --
	const levelDots = Array.from({length: 12}, (_, index) => {
		const threshold = index / 12;
		const active = voiceState === "recording" && audioLevel > threshold;
		return (
			<span
				key={index}
				className={cx(
					"h-1.5 w-1.5 rounded-full transition-all duration-75",
					active ? "bg-accent scale-110" : "bg-ink-faint/30",
				)}
			/>
		);
	});

	return (
		<div
			className="flex h-screen w-screen flex-col items-center justify-end select-none"
			style={{background: "transparent"}}
			data-tauri-drag-region
		>
			{/* Expanded transcript area */}
			{expanded && (
				<div className="mb-2 w-full max-w-[500px] overflow-hidden rounded-2xl border border-white/10 bg-app-darkBox/95 shadow-2xl backdrop-blur-xl">
					{/* Header */}
					<div className="flex items-start justify-between gap-3 border-b border-white/5 px-4 py-2.5">
						<div className="flex items-center gap-2">
							<div className="h-3 w-3 rounded-full bg-accent" />
							<span className="text-xs font-medium text-ink">Spacebot</span>
						</div>
						<div className="flex min-w-0 items-center gap-2">
							<select
								value={profileId}
								onChange={(event) => {
									setProfileId(event.target.value);
									localStorage.setItem("spacebot.voice.profileId", event.target.value);
								}}
								disabled={profilesQuery.isLoading || !profilesQuery.data?.length}
								className="min-w-[180px] max-w-[220px] rounded-md border border-white/10 bg-white/5 px-2 py-1 text-tiny text-ink outline-none disabled:opacity-60"
							>
								{profilesQuery.isLoading && <option>Loading voices...</option>}
								{!profilesQuery.isLoading && !profilesQuery.data?.length && (
									<option>No voices found</option>
								)}
								{profilesQuery.data?.map((profile) => (
									<option key={profile.id} value={profile.id}>
										{profile.name ?? profile.id.slice(0, 8)}
									</option>
								))}
							</select>
							<button
								onClick={() => setExpanded(false)}
								className="text-tiny text-ink-faint hover:text-ink"
							>
								Collapse
							</button>
						</div>
					</div>
					{profilesQuery.isError && (
						<div className="border-b border-white/5 px-4 py-2 text-tiny text-amber-300/80">
							Couldn&apos;t load Voicebox profiles. Restart Spacebot after backend changes, or set `SPACEBOT_VOICEBOX_PROFILE_ID`.
						</div>
					)}

					{/* Transcript */}
					<div className="max-h-[300px] overflow-y-auto p-4">
						{transcript.length === 0 ? (
							<p className="text-center text-xs text-ink-faint">
								Your conversation will appear here.
							</p>
						) : (
							<div className="flex flex-col gap-3">
								{transcript.map((entry, index) => (
									<div key={index} className="flex flex-col gap-0.5">
										<span className="text-tiny font-medium text-ink-faint">
											{entry.role === "user" ? "You" : "Spacebot"}
										</span>
										<p className="whitespace-pre-wrap text-xs leading-relaxed text-ink">
											{entry.text}
										</p>
									</div>
								))}
							</div>
						)}
					</div>
				</div>
			)}

			{/* Pill */}
			<div
				className={cx(
					"mb-2 flex w-full max-w-[500px] cursor-pointer items-center gap-3 rounded-2xl border px-4 py-3 shadow-2xl backdrop-blur-xl transition-all",
					voiceState === "recording"
						? "border-accent/40 bg-accent/10"
						: voiceState === "speaking"
							? "border-accent/30 bg-accent/5"
							: "border-white/10 bg-app-darkBox/95",
				)}
				onClick={() => {
					if (voiceState === "idle") setExpanded(!expanded);
				}}
			>
				{/* Status text */}
				<p className={cx(
					"min-w-0 flex-1 truncate text-sm",
					voiceState === "idle" ? "text-ink-faint" : "text-ink",
				)}>
					{statusText}
				</p>

				{/* Audio level dots (recording) or mic icon */}
				{voiceState === "recording" ? (
					<div className="flex items-center gap-0.5">
						{levelDots}
					</div>
				) : (
					<button
						onClick={(event) => {
							event.stopPropagation();
							if (voiceState === "idle") handleStartRecording();
							else if (voiceState === "speaking") stopTts();
						}}
						className={cx(
							"flex h-8 w-8 flex-shrink-0 items-center justify-center rounded-full transition-colors",
							voiceState === "idle" && "bg-white/5 text-ink-faint hover:bg-white/10 hover:text-ink",
							voiceState === "processing" && "animate-pulse bg-accent/20 text-accent",
							voiceState === "speaking" && "bg-accent/20 text-accent",
						)}
					>
						{voiceState === "speaking" ? (
							<SpeakerIcon />
						) : voiceState === "processing" ? (
							<Suspense fallback={<MicIcon />}>
								<div className="h-6 w-6">
									<Orb hue={270} forceHoverState />
								</div>
							</Suspense>
						) : (
							<MicIcon />
						)}
					</button>
				)}

				{/* Stop button (while recording) */}
				{voiceState === "recording" && (
					<button
						onClick={(event) => {
							event.stopPropagation();
							handleStopRecording();
						}}
						className="flex h-6 w-6 flex-shrink-0 items-center justify-center rounded bg-accent/80 transition-colors hover:bg-accent"
					>
						<span className="h-2.5 w-2.5 rounded-sm bg-white" />
					</button>
				)}
			</div>
		</div>
	);
}

// ── Icons ────────────────────────────────────────────────────────────────

function MicIcon() {
	return (
		<svg width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round">
			<rect x="5" y="1" width="6" height="9" rx="3" />
			<path d="M3 7a5 5 0 0 0 10 0" />
			<line x1="8" y1="12" x2="8" y2="15" />
			<line x1="5.5" y1="15" x2="10.5" y2="15" />
		</svg>
	);
}

function SpeakerIcon() {
	return (
		<svg width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round">
			<polygon points="2,5 6,5 10,1.5 10,14.5 6,11 2,11" fill="currentColor" stroke="none" />
			<path d="M12.5 5.5a3.5 3.5 0 0 1 0 5" />
		</svg>
	);
}
