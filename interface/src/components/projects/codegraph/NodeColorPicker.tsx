// Shared color picker popover. Used by CodeGraphSidebar (node + edge color
// overrides), GroupContainer, and FileNodeCard.
//
// Two tabs: Swatches (fixed palette grid) and Spectrum (continuous HSV with a
// brightness slider). Changes are previewed locally; nothing is committed
// until the user presses Done. Recent colors persist in localStorage across
// picker instances.

import { clsx } from "clsx";
import * as Popover from "@radix-ui/react-popover";
import { useMemo, useRef, useState } from "react";

// Kept exported so existing imports of COLOR_PRESETS continue to resolve.
export const COLOR_PRESETS = [
	"#ef4444", "#f97316", "#eab308", "#22c55e",
	"#06b6d4", "#3b82f6", "#8b5cf6", "#d946ef",
	"#ec4899", "#f43f5e", "#14b8a6", "#84cc16",
	"#64748b", "#e2e8f0", "#a78bfa", "#fb923c",
];

const SPECTRUM_HUES = [0, 30, 60, 120, 180, 210, 240, 270, 300];
const GRID_ROWS = 12;

const RECENT_KEY = "codegraph.recentColors";
const RECENT_MAX = 5;

// -- Color math --------------------------------------------------------------

function clamp01(n: number): number {
	return Math.max(0, Math.min(1, n));
}

function hexToRgb(hex: string): [number, number, number] {
	let m = hex.replace("#", "");
	if (m.length === 3) m = m.split("").map((c) => c + c).join("");
	const n = parseInt(m, 16);
	if (Number.isNaN(n)) return [0, 0, 0];
	return [(n >> 16) & 255, (n >> 8) & 255, n & 255];
}

function rgbToHex(r: number, g: number, b: number): string {
	const h = (v: number) => Math.round(v).toString(16).padStart(2, "0");
	return `#${h(r)}${h(g)}${h(b)}`;
}

function hsvToRgb(h: number, s: number, v: number): [number, number, number] {
	const c = v * s;
	const hh = (((h % 360) + 360) % 360) / 60;
	const x = c * (1 - Math.abs((hh % 2) - 1));
	let r1 = 0, g1 = 0, b1 = 0;
	if (hh < 1) [r1, g1, b1] = [c, x, 0];
	else if (hh < 2) [r1, g1, b1] = [x, c, 0];
	else if (hh < 3) [r1, g1, b1] = [0, c, x];
	else if (hh < 4) [r1, g1, b1] = [0, x, c];
	else if (hh < 5) [r1, g1, b1] = [x, 0, c];
	else [r1, g1, b1] = [c, 0, x];
	const m = v - c;
	return [(r1 + m) * 255, (g1 + m) * 255, (b1 + m) * 255];
}

function rgbToHsv(r: number, g: number, b: number): [number, number, number] {
	const R = r / 255, G = g / 255, B = b / 255;
	const max = Math.max(R, G, B);
	const min = Math.min(R, G, B);
	const d = max - min;
	let h = 0;
	if (d !== 0) {
		if (max === R) h = ((G - B) / d) % 6;
		else if (max === G) h = (B - R) / d + 2;
		else h = (R - G) / d + 4;
		h *= 60;
		if (h < 0) h += 360;
	}
	const s = max === 0 ? 0 : d / max;
	return [h, s, max];
}

function hslToRgb(h: number, s: number, l: number): [number, number, number] {
	const S = s / 100, L = l / 100;
	const c = (1 - Math.abs(2 * L - 1)) * S;
	const hh = (((h % 360) + 360) % 360) / 60;
	const x = c * (1 - Math.abs((hh % 2) - 1));
	let r1 = 0, g1 = 0, b1 = 0;
	if (hh < 1) [r1, g1, b1] = [c, x, 0];
	else if (hh < 2) [r1, g1, b1] = [x, c, 0];
	else if (hh < 3) [r1, g1, b1] = [0, c, x];
	else if (hh < 4) [r1, g1, b1] = [0, x, c];
	else if (hh < 5) [r1, g1, b1] = [x, 0, c];
	else [r1, g1, b1] = [c, 0, x];
	const m = L - c / 2;
	return [(r1 + m) * 255, (g1 + m) * 255, (b1 + m) * 255];
}

function hexToHsv(hex: string): [number, number, number] {
	const [r, g, b] = hexToRgb(hex);
	return rgbToHsv(r, g, b);
}

function hsvToHex(h: number, s: number, v: number): string {
	const [r, g, b] = hsvToRgb(h, s, v);
	return rgbToHex(r, g, b);
}

// -- Swatch grid -------------------------------------------------------------

const SWATCHES: string[] = (() => {
	const out: string[] = [];
	for (let r = 0; r < GRID_ROWS; r++) {
		// Lightness curve spans 95% (top) → 5% (bottom) so the top row reads as
		// near-white and the bottom as near-black, matching the reference UI.
		const l = 95 - (r * 90) / (GRID_ROWS - 1);
		const gray = Math.round((l / 100) * 255);
		out.push(rgbToHex(gray, gray, gray));
		for (const hue of SPECTRUM_HUES) {
			const [rr, gg, bb] = hslToRgb(hue, 100, l);
			out.push(rgbToHex(rr, gg, bb));
		}
	}
	return out;
})();

// -- Recent colors -----------------------------------------------------------

function loadRecent(): string[] {
	try {
		const raw = localStorage.getItem(RECENT_KEY);
		if (!raw) return [];
		const parsed = JSON.parse(raw);
		if (!Array.isArray(parsed)) return [];
		return parsed.filter((c: unknown): c is string => typeof c === "string").slice(0, RECENT_MAX);
	} catch {
		return [];
	}
}

function pushRecent(color: string): void {
	const prev = loadRecent();
	const next = [color, ...prev.filter((c) => c.toLowerCase() !== color.toLowerCase())].slice(0, RECENT_MAX);
	try {
		localStorage.setItem(RECENT_KEY, JSON.stringify(next));
	} catch {
		// Quota or privacy mode — recents are best-effort.
	}
}

// -- Component ---------------------------------------------------------------

interface Props {
	currentColor: string;
	defaultColor: string;
	onSelect: (color: string) => void;
	onReset: () => void;
}

export function NodeColorPicker({ currentColor, defaultColor, onSelect, onReset }: Props) {
	const [tab, setTab] = useState<"swatches" | "spectrum">("swatches");
	const [pending, setPending] = useState(currentColor);
	const recent = useMemo(loadRecent, []);

	const [hue, sat, val] = useMemo(() => hexToHsv(pending), [pending]);
	const [r, g, b] = useMemo(() => hexToRgb(pending), [pending]);
	const isCustom = pending.toLowerCase() !== defaultColor.toLowerCase();

	const commit = () => {
		onSelect(pending);
		pushRecent(pending);
	};

	const openEyeDropper = async () => {
		const ED = (window as unknown as { EyeDropper?: new () => { open: () => Promise<{ sRGBHex: string }> } }).EyeDropper;
		if (!ED) return;
		try {
			const result = await new ED().open();
			if (result?.sRGBHex) setPending(result.sRGBHex);
		} catch {
			// User cancelled.
		}
	};

	return (
		<div className="flex w-[300px] flex-col gap-3">
			{/* Tabs */}
			<div className="flex items-center justify-center gap-2">
				<TabButton active={tab === "swatches"} onClick={() => setTab("swatches")}>Swatches</TabButton>
				<TabButton active={tab === "spectrum"} onClick={() => setTab("spectrum")}>Spectrum</TabButton>
			</div>

			{tab === "swatches"
				? <SwatchesGrid selected={pending} onPick={setPending} />
				: <SpectrumPicker h={hue} s={sat} v={val} onChange={(h, s, v) => setPending(hsvToHex(h, s, v))} />}

			{/* Preview + readouts */}
			<div className="flex items-center gap-3">
				<div className="flex h-9 w-14 overflow-hidden rounded-md border border-app-line">
					<div className="h-full w-1/2" style={{ backgroundColor: pending }} title="New" />
					<div className="h-full w-1/2" style={{ backgroundColor: currentColor }} title="Current" />
				</div>
				<div className="grid flex-1 grid-cols-4 gap-1">
					<Readout label="Hex" value={pending.toUpperCase()} mono />
					<Readout label="R" value={String(Math.round(r))} />
					<Readout label="G" value={String(Math.round(g))} />
					<Readout label="B" value={String(Math.round(b))} />
				</div>
			</div>

			{/* Recent colors */}
			<div className="flex flex-col gap-1.5">
				<div className="flex items-center gap-2">
					<span className="text-[10px] text-ink-faint">Recently used colors</span>
					<div className="flex-1 border-t border-dotted border-app-line" />
				</div>
				<div className="flex items-center gap-2">
					{Array.from({ length: RECENT_MAX }).map((_, i) => {
						const c = recent[i];
						return c ? (
							<button
								key={i}
								onClick={() => setPending(c)}
								className="h-6 w-6 rounded-full border border-app-line transition-transform hover:scale-110"
								style={{ backgroundColor: c }}
								title={c}
							/>
						) : (
							<div key={i} className="h-6 w-6 rounded-full border border-dashed border-app-line/60" />
						);
					})}
					<button
						onClick={openEyeDropper}
						className="ml-auto rounded p-1 text-ink-faint transition-colors hover:bg-app-hover hover:text-ink"
						title="Pick color from screen"
					>
						<EyeDropperIcon />
					</button>
				</div>
			</div>

			{isCustom && (
				<Popover.Close asChild>
					<button
						onClick={onReset}
						className="self-center rounded px-2 py-0.5 text-[10px] text-ink-faint transition-colors hover:bg-app-hover hover:text-ink"
					>
						Reset to default
					</button>
				</Popover.Close>
			)}

			{/* Actions */}
			<div className="flex items-stretch gap-1 border-t border-app-line pt-2">
				<Popover.Close className="flex-1 rounded py-1 text-xs text-ink-faint transition-colors hover:bg-app-hover hover:text-ink">
					Cancel
				</Popover.Close>
				<div className="w-px bg-app-line" />
				<Popover.Close
					onClick={commit}
					className="flex-1 rounded py-1 text-xs font-semibold text-accent transition-colors hover:bg-app-hover"
				>
					Done
				</Popover.Close>
			</div>
		</div>
	);
}

// -- Sub-components ----------------------------------------------------------

function TabButton({ active, onClick, children }: { active: boolean; onClick: () => void; children: React.ReactNode }) {
	return (
		<button
			onClick={onClick}
			className={clsx(
				"rounded-full px-4 py-1 text-xs transition-colors",
				active
					? "bg-app-hover font-semibold text-accent"
					: "text-ink-faint hover:text-ink",
			)}
		>
			{children}
		</button>
	);
}

function SwatchesGrid({ selected, onPick }: { selected: string; onPick: (c: string) => void }) {
	const sel = selected.toLowerCase();
	return (
		<div className="overflow-hidden rounded-lg">
			<div className="grid grid-cols-10">
				{SWATCHES.map((c, i) => {
					const active = c.toLowerCase() === sel;
					return (
						<button
							key={i}
							onClick={() => onPick(c)}
							className={clsx(
								"aspect-square transition-transform",
								active && "z-10 scale-[1.35] rounded-sm outline outline-2 outline-white",
							)}
							style={{ backgroundColor: c }}
							title={c}
						/>
					);
				})}
			</div>
		</div>
	);
}

function SpectrumPicker({
	h, s, v, onChange,
}: {
	h: number;
	s: number;
	v: number;
	onChange: (h: number, s: number, v: number) => void;
}) {
	const areaRef = useRef<HTMLDivElement>(null);

	const updateArea = (clientX: number, clientY: number) => {
		const el = areaRef.current;
		if (!el) return;
		const rect = el.getBoundingClientRect();
		const x = clamp01((clientX - rect.left) / rect.width);
		const y = clamp01((clientY - rect.top) / rect.height);
		onChange(x * 360, y, v);
	};

	return (
		<div className="flex flex-col gap-2.5">
			<div
				ref={areaRef}
				onPointerDown={(e) => {
					e.currentTarget.setPointerCapture(e.pointerId);
					updateArea(e.clientX, e.clientY);
				}}
				onPointerMove={(e) => {
					if (e.buttons === 1) updateArea(e.clientX, e.clientY);
				}}
				className="relative h-44 w-full cursor-crosshair touch-none overflow-hidden rounded-lg"
				style={{
					background: `
						linear-gradient(to bottom, rgba(255,255,255,1) 0%, rgba(255,255,255,0) 100%),
						linear-gradient(to right,
							hsl(0, 100%, 50%),
							hsl(60, 100%, 50%),
							hsl(120, 100%, 50%),
							hsl(180, 100%, 50%),
							hsl(240, 100%, 50%),
							hsl(300, 100%, 50%),
							hsl(360, 100%, 50%))
					`,
				}}
			>
				<div
					className="pointer-events-none absolute inset-0"
					style={{ backgroundColor: "black", opacity: 1 - v }}
				/>
				<div
					className="pointer-events-none absolute h-4 w-4 -translate-x-1/2 -translate-y-1/2 rounded-full border-2 border-white shadow"
					style={{ left: `${(h / 360) * 100}%`, top: `${s * 100}%` }}
				/>
			</div>

			<SliderTrack
				value={v}
				onChange={(nv) => onChange(h, s, nv)}
				trackStyle={{
					background: `linear-gradient(to right, #000, ${hsvToHex(h || 0, s || 0, 1)})`,
				}}
				label={`${Math.round(v * 100)}%`}
			/>
		</div>
	);
}

function SliderTrack({
	value, onChange, trackStyle, label,
}: {
	value: number;
	onChange: (v: number) => void;
	trackStyle: React.CSSProperties;
	label?: string;
}) {
	const ref = useRef<HTMLDivElement>(null);
	const update = (clientX: number) => {
		const el = ref.current;
		if (!el) return;
		const rect = el.getBoundingClientRect();
		onChange(clamp01((clientX - rect.left) / rect.width));
	};
	return (
		<div className="flex items-center gap-2">
			<div
				ref={ref}
				onPointerDown={(e) => {
					e.currentTarget.setPointerCapture(e.pointerId);
					update(e.clientX);
				}}
				onPointerMove={(e) => {
					if (e.buttons === 1) update(e.clientX);
				}}
				className="relative h-2.5 flex-1 cursor-pointer touch-none rounded-full"
				style={trackStyle}
			>
				<div
					className="pointer-events-none absolute top-1/2 h-4 w-4 -translate-x-1/2 -translate-y-1/2 rounded-full border border-app-line bg-white shadow"
					style={{ left: `${value * 100}%` }}
				/>
			</div>
			{label && <span className="w-10 shrink-0 text-right text-[10px] text-ink-faint">{label}</span>}
		</div>
	);
}

function Readout({ label, value, mono = false }: { label: string; value: string; mono?: boolean }) {
	return (
		<div className="flex flex-col items-center gap-0.5 leading-none">
			<div className="text-[9px] uppercase tracking-wider text-ink-faint/70">{label}</div>
			<div className={clsx("text-[10px] text-ink", mono && "font-mono")}>{value}</div>
		</div>
	);
}

function EyeDropperIcon() {
	return (
		<svg
			viewBox="0 0 24 24"
			fill="none"
			stroke="currentColor"
			strokeWidth="2"
			strokeLinecap="round"
			strokeLinejoin="round"
			className="h-4 w-4"
		>
			<path d="m2 22 1-1h3l9-9" />
			<path d="M3 21v-3l9-9" />
			<path d="m15 6 3.4-3.4a2.1 2.1 0 1 1 3 3L18 9l.4.4a2.1 2.1 0 1 1-3 3l-3.8-3.8a2.1 2.1 0 1 1 3-3l.4.4Z" />
		</svg>
	);
}
