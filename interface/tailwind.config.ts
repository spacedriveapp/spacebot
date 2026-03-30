import defaultTheme from "tailwindcss/defaultTheme";
import type { Config } from "tailwindcss";

function alpha(variableName: string) {
	return `color-mix(in srgb, var(${variableName}) calc(<alpha-value> * 100%), transparent)`;
}

export default {
	content: ["./index.html", "./src/**/*.{ts,tsx}"],

	theme: {
		screens: {
			xs: "475px",
			sm: "650px",
			md: "868px",
			lg: "1024px",
			xl: "1280px",
		},
		fontFamily: {
			sans: [...defaultTheme.fontFamily.sans],
			mono: [...defaultTheme.fontFamily.mono],
			plex: ["IBM Plex Sans", ...defaultTheme.fontFamily.sans],
		},
		fontSize: {
			tiny: ".70rem",
			xs: ".75rem",
			sm: ".80rem",
			base: "1rem",
			lg: "1.125rem",
			xl: "1.25rem",
			"2xl": "1.5rem",
			"3xl": "1.875rem",
			"4xl": "2.25rem",
			"5xl": "3rem",
			"6xl": "4rem",
			"7xl": "5rem",
		},
		extend: {
			colors: {
				accent: {
					DEFAULT: alpha("--color-accent"),
					faint: alpha("--color-accent-faint"),
					deep: alpha("--color-accent-deep"),
				},
				ink: {
					DEFAULT: alpha("--color-ink"),
					dull: alpha("--color-ink-dull"),
					faint: alpha("--color-ink-faint"),
				},
				sidebar: {
					DEFAULT: alpha("--color-sidebar"),
					box: alpha("--color-sidebar-box"),
					line: alpha("--color-sidebar-line"),
					ink: alpha("--color-sidebar-ink"),
					inkFaint: alpha("--color-sidebar-ink-faint"),
					inkDull: alpha("--color-sidebar-ink-dull"),
					divider: alpha("--color-sidebar-divider"),
					button: alpha("--color-sidebar-button"),
					selected: alpha("--color-sidebar-selected"),
					shade: alpha("--color-sidebar-shade"),
				},
				app: {
					DEFAULT: alpha("--color-app"),
					box: alpha("--color-app-box"),
					darkBox: alpha("--color-app-dark-box"),
					darkerBox: alpha("--color-app-darker-box"),
					lightBox: alpha("--color-app-light-box"),
					overlay: alpha("--color-app-overlay"),
					input: alpha("--color-app-input"),
					focus: alpha("--color-app-focus"),
					line: alpha("--color-app-line"),
					divider: alpha("--color-app-divider"),
					button: alpha("--color-app-button"),
					selected: alpha("--color-app-selected"),
					selectedItem: alpha("--color-app-selected-item"),
					hover: alpha("--color-app-hover"),
					active: alpha("--color-app-active"),
					shade: alpha("--color-app-shade"),
					frame: alpha("--color-app-frame"),
					slider: alpha("--color-app-slider"),
					explorerScrollbar: alpha("--color-app-explorer-scrollbar"),
				},
				menu: {
					DEFAULT: alpha("--color-menu"),
					line: alpha("--color-menu-line"),
					hover: alpha("--color-menu-hover"),
					selected: alpha("--color-menu-selected"),
					shade: alpha("--color-menu-shade"),
					ink: alpha("--color-menu-ink"),
					faint: alpha("--color-menu-faint"),
				},
				status: {
					success: alpha("--color-status-success"),
					warning: alpha("--color-status-warning"),
					error: alpha("--color-status-error"),
					info: alpha("--color-status-info"),
				},
			},
			transitionTimingFunction: {
				"in-sine": "cubic-bezier(0.12, 0, 0.39, 0)",
				"out-sine": "cubic-bezier(0.61, 1, 0.88, 1)",
				"in-out-sine": "cubic-bezier(0.37, 0, 0.63, 1)",
				"in-cubic": "cubic-bezier(0.32, 0, 0.67, 0)",
				"out-cubic": "cubic-bezier(0.33, 1, 0.68, 1)",
				"in-out-cubic": "cubic-bezier(0.65, 0, 0.35, 1)",
				"in-expo": "cubic-bezier(0.7, 0, 0.84, 0)",
				"out-expo": "cubic-bezier(0.16, 1, 0.3, 1)",
				"in-out-expo": "cubic-bezier(0.87, 0, 0.13, 1)",
			},
		},
	},
	plugins: [
		require("tailwindcss-animate"),
	],
} satisfies Config;
