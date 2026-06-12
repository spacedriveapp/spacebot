import { useState, useEffect, useCallback } from "react";

export type ThemeId =
	| "default"
	| "vanilla"
	| "midnight"
	| "noir"
	| "slate"
	| "nord"
	| "mocha";

export interface ThemeOption {
	id: ThemeId;
	name: string;
	description: string;
	className: string;
}

export const THEMES: ThemeOption[] = [
	{
		id: "default",
		name: "Default",
		description: "Dark theme with blue accent",
		className: "",
	},
	{
		id: "vanilla",
		name: "Vanilla",
		description: "Light theme",
		className: "vanilla-theme",
	},
	{
		id: "midnight",
		name: "Midnight",
		description: "Deep blue dark theme",
		className: "midnight-theme",
	},
	{
		id: "noir",
		name: "Noir",
		description: "Pure black and white theme",
		className: "noir-theme",
	},
	{
		id: "slate",
		name: "Slate",
		description: "Cool gray dark theme",
		className: "slate-theme",
	},
	{
		id: "nord",
		name: "Nord",
		description: "Arctic blue-gray theme",
		className: "nord-theme",
	},
	{
		id: "mocha",
		name: "Mocha",
		description: "Warm brown dark theme",
		className: "mocha-theme",
	},
];

const STORAGE_KEY = "spacebot-theme";

function getInitialTheme(): ThemeId {
	if (typeof window === "undefined") return "default";
	const stored = localStorage.getItem(STORAGE_KEY);
	if (stored && THEMES.some((t) => t.id === stored)) {
		return stored as ThemeId;
	}
	return "default";
}

function applyThemeClass(themeId: ThemeId) {
	const theme = THEMES.find((t) => t.id === themeId);
	const root = document.documentElement;

	// Remove all theme classes
	THEMES.forEach((t) => {
		if (t.className) {
			root.classList.remove(t.className);
		}
	});

	// Add the selected theme class
	if (theme?.className) {
		root.classList.add(theme.className);
	}
}

export function useTheme() {
	const [theme, setThemeState] = useState<ThemeId>(getInitialTheme);

	// Apply theme on mount and when theme changes
	useEffect(() => {
		applyThemeClass(theme);
	}, [theme]);

	const setTheme = useCallback((newTheme: ThemeId) => {
		setThemeState(newTheme);
		localStorage.setItem(STORAGE_KEY, newTheme);
	}, []);

	return { theme, setTheme, themes: THEMES };
}

// Initialize theme on page load (before React hydrates)
if (typeof window !== "undefined") {
	const initialTheme = getInitialTheme();
	applyThemeClass(initialTheme);
}
