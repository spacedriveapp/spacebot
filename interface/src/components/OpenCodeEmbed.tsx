import {useState, useEffect, useRef} from "react";
import {useTheme} from "../hooks/useTheme";

/** RFC 4648 base64url encoding (no padding), matching OpenCode's directory encoding. */
export function base64UrlEncode(value: string): string {
	const bytes = new TextEncoder().encode(value);
	const binary = Array.from(bytes, (b) => String.fromCharCode(b)).join("");
	return btoa(binary).replace(/\+/g, "-").replace(/\//g, "_").replace(/=/g, "");
}

/**
 * Cache for the OpenCode embed assets. Once loaded, the JS module and CSS
 * text are reused across mounts so we don't re-fetch on every tab switch.
 */
let embedAssetsPromise: Promise<{
	mountOpenCode: (
		el: HTMLElement,
		config: {
			serverUrl: string;
			initialRoute?: string;
			colorScheme?: "light" | "dark" | "system";
		},
	) => {
		dispose: () => void;
		navigate: (route: string) => void;
		setColorScheme?: (scheme: "light" | "dark" | "system") => void;
	};
	cssText: string;
}> | null = null;

function loadScript(src: string): Promise<void> {
	return new Promise((resolve, reject) => {
		// Don't add the same script twice
		if (document.querySelector(`script[src="${src}"]`)) {
			resolve();
			return;
		}
		const script = document.createElement("script");
		script.type = "module";
		script.src = src;
		script.onload = () => resolve();
		script.onerror = () => reject(new Error(`Failed to load script: ${src}`));
		document.head.appendChild(script);
	});
}

function loadEmbedAssets() {
	if (embedAssetsPromise) return embedAssetsPromise;
	embedAssetsPromise = (async () => {
		// Load the manifest to find hashed asset filenames
		const manifestRes = await fetch("/opencode-embed/manifest.json");
		if (!manifestRes.ok)
			throw new Error("Failed to load opencode-embed manifest");
		const manifest: {js: string; css: string} = await manifestRes.json();

		// Guard against path traversal or unexpected values from the manifest
		const validAssetPath = (p: unknown): p is string =>
			typeof p === "string" && p.startsWith("assets/") && !p.includes("..");
		if (!validAssetPath(manifest.js) || !validAssetPath(manifest.css)) {
			throw new Error("Invalid asset paths in opencode-embed manifest");
		}

		// Load JS via <script> tag (required for /public files in Vite dev)
		// and CSS via fetch (to inject into Shadow DOM) in parallel
		const [, cssRes] = await Promise.all([
			loadScript(`/opencode-embed/${manifest.js}`),
			fetch(`/opencode-embed/${manifest.css}`),
		]);

		if (!cssRes.ok) throw new Error("Failed to load opencode-embed CSS");
		const cssText = await cssRes.text();

		// The embed entry attaches mountOpenCode to window.__opencode_embed__
		const embedApi = (window as any).__opencode_embed__;
		if (!embedApi?.mountOpenCode) {
			throw new Error("OpenCode embed module did not export mountOpenCode");
		}

		return {mountOpenCode: embedApi.mountOpenCode, cssText};
	})();
	// If loading fails, clear the cache so the next attempt retries
	embedAssetsPromise.catch(() => {
		embedAssetsPromise = null;
	});
	return embedAssetsPromise;
}

/**
 * Reference count for the shared portal CSS injected into document.head.
 * Multiple OpenCodeEmbed instances can coexist (e.g. orchestration view);
 * the portal CSS is only removed when the last instance unmounts.
 */
/**
 * SpaceUI theme tokens (CSS custom properties) that need to cross the Shadow
 * DOM boundary so the embedded OpenCode SPA can theme its prompt-input,
 * buttons, etc. against the active Spacebot theme. Sourced from
 * spaceui/packages/tokens/src/css/themes/<theme>.css — keep in sync when
 * spaceui adds/removes tokens.
 */
const FORWARDED_THEME_TOKENS = [
	"--color-accent", "--color-accent-faint", "--color-accent-deep",
	"--color-ink", "--color-ink-dull", "--color-ink-faint",
	"--color-sidebar", "--color-sidebar-box", "--color-sidebar-line",
	"--color-sidebar-ink", "--color-sidebar-ink-dull", "--color-sidebar-ink-faint",
	"--color-sidebar-divider", "--color-sidebar-button", "--color-sidebar-selected",
	"--color-sidebar-shade",
	"--color-app", "--color-app-box", "--color-app-dark-box", "--color-app-darker-box",
	"--color-app-light-box", "--color-app-overlay", "--color-app-input", "--color-app-focus",
	"--color-app-line", "--color-app-divider", "--color-app-button", "--color-app-hover",
	"--color-app-selected", "--color-app-selected-item", "--color-app-active",
	"--color-app-shade", "--color-app-frame", "--color-app-slider",
	"--color-app-explorer-scrollbar",
	"--color-menu", "--color-menu-line", "--color-menu-ink", "--color-menu-faint",
	"--color-menu-hover", "--color-menu-selected", "--color-menu-shade",
];

/**
 * Read the current values of FORWARDED_THEME_TOKENS from the document root
 * and inject them as `:host { --foo: bar; ... }` into the given style
 * element. This makes the tokens available inside the Shadow DOM so the
 * OpenCode embed's CSS can resolve var(--color-app-box) etc. against the
 * active Spacebot theme.
 */
function forwardThemeTokens(styleEl: HTMLStyleElement) {
	const styles = getComputedStyle(document.documentElement);
	const declarations = FORWARDED_THEME_TOKENS
		.map((name) => {
			const value = styles.getPropertyValue(name).trim();
			return value ? `${name}: ${value};` : "";
		})
		.filter(Boolean)
		.join("\n\t");
	styleEl.textContent = `:host {\n\t${declarations}\n}`;
}

let portalCssRefCount = 0;

/**
 * Map a Spacebot theme to OpenCode's color scheme. Vanilla is the only
 * dedicated light theme in the Spacebot palette today; everything else is
 * a dark variant. If a future theme adds light variants, extend this list.
 */
function colorSchemeForTheme(theme: string): "light" | "dark" {
	return theme === "vanilla" ? "light" : "dark";
}

export function OpenCodeEmbed({
	port,
	sessionId,
	directory: initialDirectory,
}: {
	port: number;
	sessionId: string;
	directory: string | null;
}) {
	const [state, setState] = useState<"loading" | "ready" | "error">("loading");
	const [errorMessage, setErrorMessage] = useState<string | null>(null);
	const hostRef = useRef<HTMLDivElement>(null);
	const handleRef = useRef<{
		dispose: () => void;
		navigate: (route: string) => void;
		setColorScheme?: (scheme: "light" | "dark" | "system") => void;
	} | null>(null);
	// Style element inside the shadow root that mirrors :root's SpaceUI
	// theme tokens. Refresh on every theme change via the effect below.
	const themeForwardRef = useRef<HTMLStyleElement | null>(null);
	const {theme} = useTheme();

	// Route through the Spacebot proxy so it works for hosted/Tailscale
	// users, not just local dev. The proxy handles forwarding to the
	// actual OpenCode instance. In local dev the Vite proxy forwards
	// /api/* to the Rust backend at 19898; in production it's same-origin.
	const serverUrl = `/api/opencode/${port}`;

	// Discover the event directory from the OpenCode server.
	// OpenCode tags SSE events with Instance.directory (the process CWD),
	// which may differ from the session's directory field. The SPA subscribes
	// to events by directory, so we must use the event-tagged directory in
	// the route or live updates won't work.
	const [eventDirectory, setEventDirectory] = useState<string | null>(null);

	// Reset when the worker changes (props point to a different OpenCode instance)
	useEffect(() => {
		setEventDirectory(null);
	}, [port, sessionId]);

	useEffect(() => {
		const controller = new AbortController();

		(async () => {
			try {
				// Strategy 1: Probe the SSE stream briefly. The first non-heartbeat
				// event carries the actual Instance.directory.
				// Use a separate controller for the probe timeout so aborting the
				// probe doesn't poison the fallback fetch or the unmount controller.
				const probeController = new AbortController();
				const onUnmount = () => probeController.abort();
				controller.signal.addEventListener("abort", onUnmount);

				const sseRes = await fetch(`${serverUrl}/global/event`, {
					headers: {Accept: "text/event-stream"},
					signal: probeController.signal,
				});
				if (!sseRes.ok || !sseRes.body) throw new Error("SSE probe failed");

				const reader = sseRes.body
					.pipeThrough(new TextDecoderStream())
					.getReader();
				const timeout = setTimeout(() => probeController.abort(), 8000);
				let buffer = "";

				while (!probeController.signal.aborted) {
					const {done, value} = await reader.read();
					if (done) break;
					buffer += value;

					// Parse SSE lines
					const lines = buffer.split("\n");
					buffer = lines.pop() ?? "";
					for (const line of lines) {
						if (!line.startsWith("data: ")) continue;
						try {
							const event = JSON.parse(line.slice(6));
							if (event.directory && event.directory !== "global") {
								setEventDirectory(event.directory);
								clearTimeout(timeout);
								reader.cancel();
								return;
							}
						} catch {
							/* not JSON, skip */
						}
					}
				}
				clearTimeout(timeout);
			} catch {
				// If SSE probe fails (aborted, timeout, etc.), fall back to
				// the session API directory — unless the component unmounted.
				if (!controller.signal.aborted) {
					try {
						const res = await fetch(`${serverUrl}/session/${sessionId}`, {
							signal: controller.signal,
						});
						if (res.ok) {
							const session = await res.json();
							if (session?.directory) setEventDirectory(session.directory);
						}
					} catch {
						/* ignore */
					}
				}
			}
		})();

		return () => controller.abort();
	}, [serverUrl, sessionId]);

	// Use the discovered event directory, fall back to the prop directory
	const resolvedDirectory = eventDirectory ?? initialDirectory;

	// Stable ref for the initial route so the mount effect can read it
	// without re-running when the directory is discovered later.
	const initialRouteRef = useRef("/");
	const currentRoute = resolvedDirectory
		? `/${base64UrlEncode(resolvedDirectory)}/session/${sessionId}`
		: "/";
	initialRouteRef.current = currentRoute;

	// Mount the OpenCode SPA into a Shadow DOM.
	// Only depends on serverUrl — route changes are handled by the navigate
	// effect below, not by remounting the entire SolidJS app.
	useEffect(() => {
		const host = hostRef.current;
		if (!host) return;

		let disposed = false;

		(async () => {
			try {
				setState("loading");

				// Pre-seed OpenCode layout preferences so it starts with a clean
				// chat-only view (sidebar, terminal, file tree, review all closed).
				const layoutKey = "opencode.global.dat:layout";
				if (!localStorage.getItem(layoutKey)) {
					localStorage.setItem(
						layoutKey,
						JSON.stringify({
							sidebar: {
								opened: false,
								width: 344,
								workspaces: {},
								workspacesDefault: false,
							},
							terminal: {height: 280, opened: false},
							review: {diffStyle: "split", panelOpened: false},
							fileTree: {opened: false, width: 344, tab: "changes"},
							session: {width: 600},
							mobileSidebar: {opened: false},
							sessionTabs: {},
							sessionView: {},
							handoff: {},
						}),
					);
				}

				// First check if the OpenCode server is reachable
				const healthRes = await fetch(`${serverUrl}/global/health`);
				if (!healthRes.ok) throw new Error("OpenCode server not reachable");

				// Load the embed assets (cached after first load)
				const {mountOpenCode, cssText} = await loadEmbedAssets();

				if (disposed) return;

				// Create Shadow DOM for CSS isolation
				const shadow = host.shadowRoot ?? host.attachShadow({mode: "open"});

				// Clear any previous content
				shadow.innerHTML = "";

				// Forward SpaceUI theme tokens into the shadow as :host CSS
				// custom properties so OpenCode's embedded styles + our overrides
				// can resolve var(--color-app-box) etc. against the active theme.
				const themeForward = document.createElement("style");
				themeForward.id = "spacebot-theme-forward";
				shadow.appendChild(themeForward);
				forwardThemeTokens(themeForward);
				themeForwardRef.current = themeForward;

				// Inject the OpenCode CSS into the shadow root
				const style = document.createElement("style");
				style.textContent = cssText;
				shadow.appendChild(style);

				// Hide the sidebar, mobile sidebar, and top bar in embedded
				// mode — we only want the session/chat view.
				const overrides = document.createElement("style");
				overrides.textContent = `
					[data-component="sidebar-nav-desktop"],
					[data-component="sidebar-nav-mobile"],
					header:has(#opencode-titlebar-center),
					[data-session-title] { display: none !important; }
					main { border: none !important; border-radius: 0 !important; }
					/* Give the prompt input a background matching SpaceUI's ChatComposer
					   (bg-app-box/70 + rounded). OpenCode doesn't style it by default,
					   so it renders transparent against the app background. */
					[data-component="prompt-input"] {
						background: color-mix(in srgb, var(--color-app-box) 70%, transparent) !important;
						border: 1px solid var(--color-app-line) !important;
						border-radius: 12px !important;
					}
				`;
				shadow.appendChild(overrides);

				// Create the mount point inside the shadow.
				// Apply the base styles that OpenCode normally gets from
				// <body class="text-12-regular antialiased overflow-hidden">
				// and ensure rem units resolve correctly by setting font-size
				// on the shadow host (rem in Shadow DOM still resolves against
				// document <html>, but this establishes the inherited font-size
				// for em/% units used by child elements).
				host.style.fontSize = "16px";
				const mountDiv = document.createElement("div");
				mountDiv.id = "opencode-root";
				mountDiv.style.cssText =
					"display:flex;flex-direction:column;height:100%;width:100%;overflow:hidden;font-size:13px;line-height:150%;-webkit-font-smoothing:antialiased;";
				shadow.appendChild(mountDiv);

				// Inject a copy of OpenCode's CSS into the document <head>
				// so that Kobalte portals (dropdowns, dialogs, toasts) that
				// escape the Shadow DOM into document.body still get styled.
				// We scope it to avoid polluting Spacebot's own styles by
				// wrapping in a layer. Ref-counted so multiple instances don't
				// clobber each other's portal CSS on unmount.
				portalCssRefCount++;
				let portalStyle = document.getElementById("opencode-portal-css");
				if (!portalStyle) {
					portalStyle = document.createElement("style");
					portalStyle.id = "opencode-portal-css";
					// Use a CSS layer so OpenCode's global resets (*, html, body)
					// don't override Spacebot's styles. Portal elements from
					// Kobalte will pick up the right vars because they inherit
					// from :root where the CSS custom properties are set.
					portalStyle.textContent = `@layer opencode-portals {\n${cssText}\n}`;
					document.head.appendChild(portalStyle);
				}

				// Mount the SolidJS app — read the route from the ref so we
				// get the latest value without adding it as a dependency.
				const handle = mountOpenCode(mountDiv, {
					serverUrl,
					initialRoute: initialRouteRef.current,
					colorScheme: colorSchemeForTheme(theme),
				});

				handleRef.current = handle;
				setState("ready");
			} catch (error) {
				if (!disposed) {
					setState("error");
					setErrorMessage(
						error instanceof Error ? error.message : "Unknown error",
					);
				}
			}
		})();

		return () => {
			disposed = true;
			// Dispose the SolidJS app on unmount
			if (handleRef.current) {
				handleRef.current.dispose();
				handleRef.current = null;
			}
			// Clean up shadow DOM content
			if (host.shadowRoot) {
				host.shadowRoot.innerHTML = "";
			}
			// Only remove portal CSS when the last instance unmounts
			portalCssRefCount--;
			if (portalCssRefCount <= 0) {
				portalCssRefCount = 0;
				const portalStyle = document.getElementById("opencode-portal-css");
				if (portalStyle) portalStyle.remove();
			}
		};
	}, [serverUrl]);

	// Re-forward SpaceUI theme tokens AND update OpenCode's color scheme
	// whenever the active Spacebot theme changes — both without remounting,
	// preserving session state.
	useEffect(() => {
		const styleEl = themeForwardRef.current;
		if (styleEl) {
			forwardThemeTokens(styleEl);
		}
		handleRef.current?.setColorScheme?.(colorSchemeForTheme(theme));
	}, [theme]);

	// Navigate the embedded app when the route changes (directory discovered
	// via SSE probe, or props changed). This avoids remounting the entire
	// SolidJS app just to change routes.
	useEffect(() => {
		if (handleRef.current && resolvedDirectory) {
			const route = `/${base64UrlEncode(resolvedDirectory)}/session/${sessionId}`;
			handleRef.current.navigate(route);
		}
	}, [resolvedDirectory, sessionId]);

	// Always render the host div so the ref is available for the mount
	// effect. Overlay loading/error states on top.
	return (
		<div className="relative flex-1 overflow-hidden">
			<div
				ref={hostRef}
				className="absolute inset-0"
				style={{contain: "strict"}}
			/>
			{state === "loading" && (
				<div className="absolute inset-0 flex items-center justify-center bg-app-dark-box/80">
					<div className="flex items-center gap-2 text-xs text-ink-faint">
						<span className="h-2 w-2 animate-pulse rounded-full bg-accent" />
						Loading OpenCode...
					</div>
				</div>
			)}
			{state === "error" && (
				<div className="absolute inset-0 flex flex-col items-center justify-center gap-2 bg-app-dark-box/80 text-ink-faint">
					<p className="text-xs">Failed to load OpenCode</p>
					<p className="text-tiny">
						{errorMessage ||
							"The server may have been stopped. Try the Transcript tab for available data."}
					</p>
				</div>
			)}
		</div>
	);
}
