import {useState, useEffect, useCallback, useRef, lazy, Suspense, useMemo} from "react";
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
	const [statusText, setStatusText] = useState("Press Option+Shift+Space to talk");
	const [, setSpokenText] = useState<string | null>(null);
	const [transcript, setTranscript] = useState<Array<{role: string; text: string}>>([]);

	const sessionId = getPortalChatSessionId(agentId);
	const {
		state: recorderState,
		startRecording,
		stopRecording,
		audioLevel,
		spectrumLevels: recorderSpectrumLevels,
	} = useAudioRecorder();
	const {
		speak,
		stop: stopTts,
		playbackLevel,
		spectrumLevels: playbackSpectrumLevels,
	} = useTtsPlayback();
	const pendingSpokenRef = useRef<string | null>(null);
	const spokenReceivedRef = useRef(false);
	const ttsStartedRef = useRef(false);

	useEffect(() => {
		document.documentElement.classList.add("overlay-window");
		document.body.classList.add("overlay-window");
		document.getElementById("root")?.classList.add("overlay-window");

		return () => {
			document.documentElement.classList.remove("overlay-window");
			document.body.classList.remove("overlay-window");
			document.getElementById("root")?.classList.remove("overlay-window");
		};
	}, []);

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
				setStatusText("Press Option+Shift+Space to talk");
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
			setStatusText("Press Option+Shift+Space to talk");
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
			setTimeout(() => setStatusText("Press Option+Shift+Space to talk"), 3000);
		}
	}, [recorderState, stopRecording, agentId, sessionId]);

	useEffect(() => {
		if (!(window as any).__TAURI_INTERNALS__) return;

		let disposed = false;
		let unlistenStart: null | (() => void) = null;
		let unlistenStop: null | (() => void) = null;

		void (async () => {
			const {listen} = await import("@tauri-apps/api/event");
			if (disposed) return;

			unlistenStart = await listen("voice-overlay:start-recording", () => {
				void handleStartRecording();
			});
			unlistenStop = await listen("voice-overlay:stop-recording", () => {
				void handleStopRecording();
			});
		})();

		return () => {
			disposed = true;
			unlistenStart?.();
			unlistenStop?.();
		};
	}, [handleStartRecording, handleStopRecording]);

	// -- Keyboard shortcut (web mode mirrors the dedicated voice hotkey) --
	useEffect(() => {
		const handleKeyDown = (event: KeyboardEvent) => {
			if (event.code === "Space" && event.altKey && event.shiftKey && voiceState === "idle") {
				event.preventDefault();
				handleStartRecording();
			}
		};
		const handleKeyUp = (event: KeyboardEvent) => {
			if (event.code === "Space" && event.altKey && event.shiftKey && voiceState === "recording") {
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

	const activeEnergy = voiceState === "recording"
		? audioLevel
		: voiceState === "speaking"
			? playbackLevel
			: 0;

	const activeSpectrumLevels = voiceState === "recording"
		? recorderSpectrumLevels
		: voiceState === "speaking"
			? playbackSpectrumLevels
			: Array.from({length: 45}, () => 0);

	const waveColor = voiceState === "recording"
		? "#70b8ff"
		: voiceState === "speaking"
			? "#ba5cf6"
			: "#6b7280";

	const haloStyle = (() => {
		if (voiceState === "recording") {
			return {
				background: `radial-gradient(circle, rgba(88, 166, 255, ${0.18 + audioLevel * 0.28}) 0%, rgba(88, 166, 255, ${0.08 + audioLevel * 0.12}) 34%, transparent 72%)`,
				transform: `scale(${1 + audioLevel * 0.16})`,
			};
		}

		if (voiceState === "speaking") {
			return {
				background: `radial-gradient(circle, rgba(186, 92, 246, ${0.16 + playbackLevel * 0.3}) 0%, rgba(186, 92, 246, ${0.06 + playbackLevel * 0.14}) 34%, transparent 72%)`,
				transform: `scale(${1 + playbackLevel * 0.18})`,
			};
		}

		return {
			background: "radial-gradient(circle, rgba(255,255,255,0.08) 0%, rgba(255,255,255,0.03) 34%, transparent 72%)",
			transform: "scale(1)",
		};
	})();

	return (
		<div
			className="flex h-screen w-screen flex-col items-center justify-end select-none"
			style={{background: "transparent"}}
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
					"voice-overlay-pill mb-2 flex w-full max-w-[460px] cursor-pointer items-center gap-2.5 overflow-hidden rounded-[20px] border px-3 py-2 shadow-2xl backdrop-blur-xl transition-all",
					voiceState === "recording"
						? "border-sky-300/35 bg-sky-400/10"
						: voiceState === "speaking"
							? "border-violet-300/35 bg-violet-400/10"
							: "border-white/10 bg-app-darkBox/95",
				)}
				data-tauri-drag-region
				onClick={() => {
					if (voiceState === "idle") setExpanded(!expanded);
				}}
			>
			<div
				className={cx(
					"pointer-events-none absolute inset-x-5 -bottom-5 -top-5 rounded-full blur-2xl transition-all duration-200",
					voiceState === "recording" && "voice-overlay-halo-recording",
					voiceState === "speaking" && "voice-overlay-halo-speaking",
				)}
				style={haloStyle}
			/>

				<div className="relative flex h-9 w-9 flex-shrink-0 items-center justify-center">
					<div
						className={cx(
							"absolute inset-0 transition-all duration-150",
							voiceState === "recording"
								? "bg-sky-400/12"
								: voiceState === "speaking"
									? "bg-violet-400/12"
									: "bg-transparent",
						)}
						style={{transform: `scale(${1 + activeEnergy * 0.22})`}}
					/>
					<div className="relative z-10 flex h-7 w-7 items-center justify-center text-ink">
						<BullLogoOrb showOrb />
					</div>
				</div>

				{/* Status text */}
				<div className="relative z-10 flex min-w-0 flex-1 flex-col gap-1">
					<div className="flex items-center">
						<span
							className={cx(
								"rounded-full py-0.5 text-[8px] font-semibold uppercase tracking-[0.14em]",
								voiceState === "recording"
									? "bg-sky-400/14 text-sky-200"
									: voiceState === "speaking"
										? "bg-violet-400/14 text-violet-200"
										: voiceState === "processing"
											? "bg-violet-400/14 text-violet-200"
											: "bg-white/6 text-ink-faint",
							)}
						>
							{voiceState === "recording"
								? "Input live"
								: voiceState === "speaking"
									? "Reply live"
									: voiceState === "processing"
										? "Thinking"
										: "Voice ready"}
						</span>
					</div>
					<p className={cx(
						"min-w-0 truncate text-[12px] leading-tight",
						voiceState === "idle" ? "text-ink-faint" : "text-ink",
					)}>
						{statusText}
					</p>
					<div className="relative z-10 h-9 overflow-hidden px-1">
						<SiriWaveform
							levels={activeSpectrumLevels}
							energy={activeEnergy}
							color={waveColor}
							active={voiceState === "recording" || voiceState === "speaking"}
						/>
					</div>
				</div>

				<button
					onClick={(event) => {
						event.stopPropagation();
						if (voiceState === "idle") handleStartRecording();
						else if (voiceState === "speaking") stopTts();
					}}
					className={cx(
						"relative z-10 flex h-8 w-8 flex-shrink-0 items-center justify-center rounded-full border transition-colors",
						voiceState === "idle" && "border-white/10 bg-white/5 text-ink-faint hover:bg-white/10 hover:text-ink",
						voiceState === "processing" && "animate-pulse border-violet-300/30 bg-violet-400/15 text-violet-100",
						voiceState === "speaking" && "border-violet-300/30 bg-violet-400/15 text-violet-100",
						voiceState === "recording" && "border-sky-300/30 bg-sky-400/15 text-sky-100",
					)}
				>
					{voiceState === "speaking" ? (
						<SpeakerIcon />
					) : (
						<MicIcon />
					)}
				</button>

				{/* Stop button (while recording) */}
				{voiceState === "recording" && (
					<button
						onClick={(event) => {
							event.stopPropagation();
							handleStopRecording();
						}}
						className="relative z-10 flex h-8 w-8 flex-shrink-0 items-center justify-center rounded-full border border-sky-300/30 bg-sky-400/15 text-sky-100 transition-colors hover:bg-sky-400/20"
					>
						<StopIcon />
					</button>
				)}
			</div>
		</div>
	);
}

function SiriWaveform({
	levels,
	energy,
	color,
	active,
}: {
	levels: number[];
	energy: number;
	color: string;
	active: boolean;
}) {
	const width = 280;
	const height = 36;
	const centerY = height / 2;

	const smoothedLevels = useMemo(() => {
		if (levels.length === 0) {
			return Array.from({length: 24}, () => 0);
		}

		const bucketCount = 24;
		return Array.from({length: bucketCount}, (_, bucketIndex) => {
			const start = Math.floor((bucketIndex / bucketCount) * levels.length);
			const end = Math.max(start + 1, Math.floor(((bucketIndex + 1) / bucketCount) * levels.length));
			const slice = levels.slice(start, end);
			const average = slice.reduce((sum, value) => sum + value, 0) / slice.length;
			return Math.min(1, average);
		});
	}, [levels]);

	const wavePaths = useMemo(() => {
		const makePath = (phase: number, amplitudeBoost: number, frequency: number, drift: number) => {
			const sampleCount = 88;
			const points = Array.from({length: sampleCount + 1}, (_, index) => {
				const progress = index / sampleCount;
				const x = progress * width;
				const levelIndex = Math.min(
					smoothedLevels.length - 1,
					Math.floor(progress * smoothedLevels.length),
				);
				const fft = smoothedLevels[levelIndex] ?? 0;
				const envelope = Math.pow(Math.sin(progress * Math.PI), 1.35);
				const neighboringLevel = smoothedLevels[Math.min(smoothedLevels.length - 1, levelIndex + 1)] ?? fft;
				const blendedFft = fft * 0.65 + neighboringLevel * 0.35;
				const baseAmplitude = active
					? 3.2 + energy * 9.5 + blendedFft * 12.5 * amplitudeBoost
					: 1.35;
				const primary = Math.sin(progress * Math.PI * frequency + phase);
				const secondary = Math.sin(progress * Math.PI * (frequency * 0.52) - drift) * 0.4;
				const tertiary = Math.cos(progress * Math.PI * (frequency * 0.24) + phase * 0.35) * 0.18;
				const y = centerY - (primary + secondary + tertiary) * baseAmplitude * envelope;
				return {x, y};
			});

			return points.reduce((path, point, index, array) => {
				if (index === 0) {
					return `M ${point.x.toFixed(2)} ${point.y.toFixed(2)}`;
				}
				const previous = array[index - 1];
				const controlX = ((previous.x + point.x) / 2).toFixed(2);
				const controlY = ((previous.y + point.y) / 2).toFixed(2);
				return `${path} Q ${previous.x.toFixed(2)} ${previous.y.toFixed(2)}, ${controlX} ${controlY}`;
			}, "");
		};

		const layerColors = buildWaveLayerColors(color);

		return [
			{path: makePath(0.1, 1.28, 3.15, 0.55), opacity: active ? 0.96 : 0.28, strokeWidth: 2.9, color: layerColors[0]},
			{path: makePath(0.9, 1.08, 2.7, 1.1), opacity: active ? 0.82 : 0.22, strokeWidth: 2.55, color: layerColors[1]},
			{path: makePath(1.7, 0.9, 2.2, 1.7), opacity: active ? 0.64 : 0.18, strokeWidth: 2.15, color: layerColors[2]},
			{path: makePath(2.45, 0.74, 1.75, 2.15), opacity: active ? 0.46 : 0.14, strokeWidth: 1.85, color: layerColors[3]},
			{path: makePath(3.2, 0.58, 1.3, 2.7), opacity: active ? 0.3 : 0.1, strokeWidth: 1.5, color: layerColors[4]},
		];
	}, [active, centerY, color, energy, smoothedLevels, width]);

	return (
		<svg viewBox={`0 0 ${width} ${height}`} className="h-full w-full" preserveAspectRatio="none" aria-hidden="true">
			<path d={`M 0 ${centerY} L ${width} ${centerY}`} stroke={color} strokeOpacity={active ? 0.14 : 0.08} strokeWidth="1" />
			{wavePaths.map((wave, index) => (
				<path
					key={index}
					d={wave.path}
					fill="none"
					stroke={wave.color}
					strokeWidth={wave.strokeWidth}
					strokeLinecap="round"
					strokeLinejoin="round"
					opacity={wave.opacity}
				/>
			))}
		</svg>
	);
}

function BullLogoOrb({showOrb}: {showOrb: boolean}) {
	return (
		<>
			<div className="absolute inset-[calc(5%-2px)] z-0" aria-hidden="true">
				<img
					src="/ball.png"
					alt=""
					className="h-full w-full object-contain"
					draggable={false}
				/>
			</div>
			{showOrb && (
				<Suspense fallback={null}>
					<div className="absolute inset-0 z-10" aria-hidden="true">
						<Orb hue={-30} hoverIntensity={0} rotateOnHover />
					</div>
				</Suspense>
			)}
		</>
	);
}

function buildWaveLayerColors(baseColor: string) {
	return [
		mixHex(baseColor, "#ffffff", 0.28),
		mixHex(baseColor, "#ffffff", 0.14),
		baseColor,
		mixHex(baseColor, "#0b1020", 0.16),
		mixHex(baseColor, "#0b1020", 0.28),
	];
}

function mixHex(colorA: string, colorB: string, amount: number) {
	const from = hexToRgb(colorA);
	const to = hexToRgb(colorB);
	const mix = (fromValue: number, toValue: number) => Math.round(fromValue + (toValue - fromValue) * amount);
	return `rgb(${mix(from.red, to.red)}, ${mix(from.green, to.green)}, ${mix(from.blue, to.blue)})`;
}

function hexToRgb(hex: string) {
	const normalized = hex.replace("#", "");
	const value = normalized.length === 3
		? normalized.split("").map((char) => char + char).join("")
		: normalized;
	const parsed = Number.parseInt(value, 16);
	return {
		red: (parsed >> 16) & 255,
		green: (parsed >> 8) & 255,
		blue: parsed & 255,
	};
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

function StopIcon() {
	return (
		<svg width="14" height="14" viewBox="0 0 14 14" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round">
			<rect x="3.5" y="3.5" width="7" height="7" rx="1.5" fill="currentColor" stroke="none" />
		</svg>
	);
}
