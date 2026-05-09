import { useState, useEffect, useCallback, useMemo } from "react";

export type ThemeId =
	| "default"
	| "vanilla"
	| "midnight"
	| "noir"
	| "slate"
	| "nord"
	| "mocha"
	| "solarized-light"
	| "solarized-dark"
	| "catppuccin-latte";

export type ThemeMode = "light" | "dark" | "system";

export interface ThemeOption {
	id: ThemeId;
	name: string;
	description: string;
	className: string;
	/** Whether this theme has a light surface (true) or dark surface (false).
	 * Used for grouping in the picker and for the system-mode selector to
	 * route the OS preference to the right per-mode theme slot. */
	isLight: boolean;
}

export const THEMES: ThemeOption[] = [
	{
		id: "default",
		name: "Default",
		description: "Dark theme with blue accent",
		className: "",
		isLight: false,
	},
	{
		id: "vanilla",
		name: "Vanilla",
		description: "Light theme",
		className: "vanilla-theme",
		isLight: true,
	},
	{
		id: "midnight",
		name: "Midnight",
		description: "Deep blue dark theme",
		className: "midnight-theme",
		isLight: false,
	},
	{
		id: "noir",
		name: "Noir",
		description: "Pure black and white theme",
		className: "noir-theme",
		isLight: false,
	},
	{
		id: "slate",
		name: "Slate",
		description: "Cool gray dark theme",
		className: "slate-theme",
		isLight: false,
	},
	{
		id: "nord",
		name: "Nord",
		description: "Arctic blue-gray theme",
		className: "nord-theme",
		isLight: false,
	},
	{
		id: "mocha",
		name: "Mocha (Catppuccin)",
		description: "Warm brown dark theme",
		className: "mocha-theme",
		isLight: false,
	},
	{
		id: "catppuccin-latte",
		name: "Latte (Catppuccin)",
		description: "Catppuccin's official light variant",
		className: "catppuccin-latte-theme",
		isLight: true,
	},
	{
		id: "solarized-light",
		name: "Solarized Light",
		description: "Ethan Schoonover's classic light palette",
		className: "solarized-light-theme",
		isLight: true,
	},
	{
		id: "solarized-dark",
		name: "Solarized Dark",
		description: "Ethan Schoonover's classic dark palette",
		className: "solarized-dark-theme",
		isLight: false,
	},
];

const MODE_KEY = "spacebot-theme-mode";
const LIGHT_KEY = "spacebot-theme-light";
const DARK_KEY = "spacebot-theme-dark";
const LEGACY_KEY = "spacebot-theme";

const DEFAULT_LIGHT: ThemeId = "vanilla";
const DEFAULT_DARK: ThemeId = "default";

function getThemeById(id: string): ThemeOption | undefined {
	return THEMES.find((t) => t.id === id);
}

/** Read mode + per-mode theme slots from localStorage with one-time migration
 * from the legacy single-key format ("spacebot-theme"). */
function readPersisted(): {
	mode: ThemeMode;
	lightTheme: ThemeId;
	darkTheme: ThemeId;
} {
	if (typeof window === "undefined") {
		return { mode: "system", lightTheme: DEFAULT_LIGHT, darkTheme: DEFAULT_DARK };
	}

	let mode = localStorage.getItem(MODE_KEY) as ThemeMode | null;
	let lightTheme = localStorage.getItem(LIGHT_KEY) as ThemeId | null;
	let darkTheme = localStorage.getItem(DARK_KEY) as ThemeId | null;

	// Migration: legacy `spacebot-theme` is a single ThemeId. Route it to the
	// matching slot based on isLight, mode = system. Cleared after migration.
	if (!mode && !lightTheme && !darkTheme) {
		const legacy = localStorage.getItem(LEGACY_KEY);
		const legacyTheme = legacy ? getThemeById(legacy) : undefined;
		if (legacyTheme) {
			if (legacyTheme.isLight) {
				lightTheme = legacyTheme.id;
			} else {
				darkTheme = legacyTheme.id;
			}
			mode = "system";
			localStorage.setItem(MODE_KEY, mode);
			if (lightTheme) localStorage.setItem(LIGHT_KEY, lightTheme);
			if (darkTheme) localStorage.setItem(DARK_KEY, darkTheme);
			localStorage.removeItem(LEGACY_KEY);
			console.info(
				"[useTheme] migrated legacy spacebot-theme=%s → mode=%s light=%s dark=%s",
				legacy,
				mode,
				lightTheme ?? DEFAULT_LIGHT,
				darkTheme ?? DEFAULT_DARK,
			);
		}
	}

	return {
		mode:
			mode === "light" || mode === "dark" || mode === "system" ? mode : "system",
		lightTheme:
			lightTheme && getThemeById(lightTheme)?.isLight
				? lightTheme
				: DEFAULT_LIGHT,
		darkTheme:
			darkTheme && getThemeById(darkTheme)?.isLight === false
				? darkTheme
				: DEFAULT_DARK,
	};
}

function getSystemPrefersDark(): boolean {
	if (typeof window === "undefined" || !window.matchMedia) return true;
	return window.matchMedia("(prefers-color-scheme: dark)").matches;
}

/** Compute the currently-active theme id from mode + per-mode choices +
 * system preference. */
function effectiveTheme(
	mode: ThemeMode,
	lightTheme: ThemeId,
	darkTheme: ThemeId,
	systemPrefersDark: boolean,
): ThemeId {
	if (mode === "light") return lightTheme;
	if (mode === "dark") return darkTheme;
	return systemPrefersDark ? darkTheme : lightTheme;
}

function applyThemeClass(themeId: ThemeId) {
	const theme = getThemeById(themeId);
	const root = document.documentElement;

	// Remove all theme classes
	for (const t of THEMES) {
		if (t.className) root.classList.remove(t.className);
	}

	// Add the selected theme class
	if (theme?.className) {
		root.classList.add(theme.className);
	}
}

export function useTheme() {
	const [{ mode, lightTheme, darkTheme }, setPersisted] = useState(readPersisted);
	const [systemPrefersDark, setSystemPrefersDark] = useState(getSystemPrefersDark);

	// Subscribe to OS-level color-scheme preference changes
	useEffect(() => {
		if (typeof window === "undefined" || !window.matchMedia) return;
		const mq = window.matchMedia("(prefers-color-scheme: dark)");
		const onChange = (e: MediaQueryListEvent) =>
			setSystemPrefersDark(e.matches);
		mq.addEventListener("change", onChange);
		return () => mq.removeEventListener("change", onChange);
	}, []);

	const theme = useMemo(
		() => effectiveTheme(mode, lightTheme, darkTheme, systemPrefersDark),
		[mode, lightTheme, darkTheme, systemPrefersDark],
	);

	// Apply theme class on every change
	useEffect(() => {
		applyThemeClass(theme);
	}, [theme]);

	const setMode = useCallback((newMode: ThemeMode) => {
		setPersisted((prev) => ({ ...prev, mode: newMode }));
		localStorage.setItem(MODE_KEY, newMode);
	}, []);

	const setLightTheme = useCallback((id: ThemeId) => {
		const opt = getThemeById(id);
		if (!opt?.isLight) return;
		setPersisted((prev) => ({ ...prev, lightTheme: id }));
		localStorage.setItem(LIGHT_KEY, id);
	}, []);

	const setDarkTheme = useCallback((id: ThemeId) => {
		const opt = getThemeById(id);
		if (!opt || opt.isLight) return;
		setPersisted((prev) => ({ ...prev, darkTheme: id }));
		localStorage.setItem(DARK_KEY, id);
	}, []);

	/** Backward-compatible setter — routes the chosen theme into the right
	 * slot based on isLight, and switches mode away from "system" so the
	 * choice sticks. Keeps existing call sites working unchanged. */
	const setTheme = useCallback(
		(id: ThemeId) => {
			const opt = getThemeById(id);
			if (!opt) return;
			if (opt.isLight) {
				setLightTheme(id);
				setMode("light");
			} else {
				setDarkTheme(id);
				setMode("dark");
			}
		},
		[setLightTheme, setDarkTheme, setMode],
	);

	const isLight = !!getThemeById(theme)?.isLight;

	return {
		mode,
		setMode,
		lightTheme,
		setLightTheme,
		darkTheme,
		setDarkTheme,
		theme,
		setTheme,
		themes: THEMES,
		isLight,
	};
}

// Initialize theme on page load (before React hydrates) to avoid FOUC.
if (typeof window !== "undefined") {
	const persisted = readPersisted();
	const initial = effectiveTheme(
		persisted.mode,
		persisted.lightTheme,
		persisted.darkTheme,
		getSystemPrefersDark(),
	);
	applyThemeClass(initial);
}
