import defaultTheme from "tailwindcss/defaultTheme";
import type { Config } from "tailwindcss";

function alpha(variableName: string) {
	return `hsla(var(${variableName}), <alpha-value>)`;
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
			sans: ["Inter Variable", "Inter", ...defaultTheme.fontFamily.sans],
			mono: ["IBM Plex Mono", ...defaultTheme.fontFamily.mono],
			plex: ["IBM Plex Sans", ...defaultTheme.fontFamily.sans],
		},
		fontSize: {
			tiny: [".70rem", { lineHeight: "1rem", letterSpacing: "0.02em" }],
			xs: [".75rem", { lineHeight: "1rem", letterSpacing: "0.01em" }],
			sm: [".8125rem", { lineHeight: "1.25rem" }],
			base: [".875rem", { lineHeight: "1.375rem" }],
			lg: ["1rem", { lineHeight: "1.5rem", letterSpacing: "-0.01em" }],
			xl: ["1.125rem", { lineHeight: "1.625rem", letterSpacing: "-0.015em" }],
			"2xl": ["1.375rem", { lineHeight: "1.75rem", letterSpacing: "-0.02em" }],
			"3xl": ["1.75rem", { lineHeight: "2.125rem", letterSpacing: "-0.025em" }],
			"4xl": ["2.25rem", { lineHeight: "2.625rem", letterSpacing: "-0.03em" }],
			"5xl": ["3rem", { lineHeight: "3.5rem", letterSpacing: "-0.03em" }],
			"6xl": ["4rem", { lineHeight: "4.5rem", letterSpacing: "-0.035em" }],
			"7xl": ["5rem", { lineHeight: "5.5rem", letterSpacing: "-0.04em" }],
		},
		extend: {
			colors: {
				accent: {
					DEFAULT: alpha("--color-accent"),
					faint: alpha("--color-accent-faint"),
					deep: alpha("--color-accent-deep"),
				},
				success: {
					DEFAULT: alpha("--color-success"),
				},
				warning: {
					DEFAULT: alpha("--color-warning"),
				},
				error: {
					DEFAULT: alpha("--color-error"),
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
			},
			keyframes: {
				shimmer: {
					"0%": { backgroundPosition: "200% 0" },
					"100%": { backgroundPosition: "-200% 0" },
				},
				"card-shine": {
					"0%, 100%": { backgroundPosition: "200% 0" },
					"50%": { backgroundPosition: "-200% 0" },
				},
			},
			boxShadow: {
				"elevation-1": "0 1px 2px rgba(0,0,0,0.25), 0 0 0 1px rgba(255,255,255,0.03)",
				"elevation-2": "0 4px 12px rgba(0,0,0,0.5), 0 1px 3px rgba(0,0,0,0.3), 0 0 0 1px rgba(255,255,255,0.06)",
				"elevation-3": "0 12px 32px rgba(0,0,0,0.6), 0 4px 8px rgba(0,0,0,0.3), 0 0 0 1px rgba(255,255,255,0.08)",
				"card": "0 1px 3px rgba(0,0,0,0.2), 0 0 0 1px rgba(255,255,255,0.04), inset 0 1px 0 rgba(255,255,255,0.03)",
				"card-hover": "0 4px 16px rgba(0,0,0,0.3), 0 1px 3px rgba(0,0,0,0.2), 0 0 0 1px rgba(255,255,255,0.06), inset 0 1px 0 rgba(255,255,255,0.05)",
				"glow-accent": "0 0 16px hsla(234,55%,62%,0.12), 0 0 32px hsla(234,55%,62%,0.06)",
				"glow-accent-strong": "0 0 20px hsla(234,55%,62%,0.2), 0 0 48px hsla(234,55%,62%,0.08)",
				"inner-highlight": "inset 0 1px 0 rgba(255,255,255,0.04)",
			},
			backgroundImage: {
				"gradient-accent": "linear-gradient(135deg, hsla(234,55%,62%,1), hsla(254,50%,58%,1))",
				"gradient-accent-hover": "linear-gradient(135deg, hsla(234,55%,68%,1), hsla(254,50%,64%,1))",
				"gradient-surface": "linear-gradient(180deg, hsla(var(--dark-hue),10%,14%,1), hsla(var(--dark-hue),10%,11%,1))",
				"gradient-surface-subtle": "linear-gradient(180deg, rgba(255,255,255,0.03), transparent)",
				"gradient-radial-accent": "radial-gradient(ellipse at top center, hsla(234,55%,62%,0.12), transparent 70%)",
				"gradient-card": "linear-gradient(180deg, rgba(255,255,255,0.025), transparent 60%)",
			},
			animation: {
				shimmer: "shimmer 1.5s ease-in-out infinite",
				"card-shine": "card-shine var(--shine-duration, 12s) ease-in-out var(--shine-delay, 0s) infinite",
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
