import {
	createContext,
	useContext,
	useState,
	useEffect,
	useCallback,
	useRef,
	type ReactNode,
} from "react";
import { setServerUrl as setClientServerUrl } from "@/api/client";

export type ServerState = "checking" | "connected" | "disconnected";

const DEFAULT_SERVER_URL = "http://localhost:19898";
const STORAGE_KEY = "spacebot_server_url";
const HEALTH_CHECK_INTERVAL = 5000;

interface ServerContextValue {
	/** The full base URL of the spacebot server, e.g. "http://localhost:19898" */
	serverUrl: string;
	/** Current connection state */
	state: ServerState;
	/** Update the server URL and attempt to connect */
	setServerUrl: (url: string) => void;
	/** Whether the server has been successfully reached at least once this session */
	hasConnected: boolean;
	/** Whether the app has fully bootstrapped (health check passed AND initial data loaded) */
	hasBootstrapped: boolean;
	/** Signal that bootstrap queries have completed (called by LiveContextProvider) */
	onBootstrapped: () => void;
	/** Whether the app is running inside Tauri */
	isTauri: boolean;
	/** Whether a sidecar binary is bundled (Tauri only) */
	hasSidecar: boolean;
}

const ServerContext = createContext<ServerContextValue>({
	serverUrl: DEFAULT_SERVER_URL,
	state: "checking",
	setServerUrl: () => {},
	hasConnected: false,
	hasBootstrapped: false,
	onBootstrapped: () => {},
	isTauri: false,
	hasSidecar: false,
});

export function useServer() {
	return useContext(ServerContext);
}

/** Normalise a URL to its origin (scheme + host + port). Strips paths,
 *  query strings, and fragments so desktop connections use a clean base. */
function normalizeUrl(raw: string): string {
	let url = raw.trim();
	if (!url) return DEFAULT_SERVER_URL;
	if (!/^https?:\/\//i.test(url)) url = `http://${url}`;
	try {
		return new URL(url).origin;
	} catch {
		return DEFAULT_SERVER_URL;
	}
}

/** Check if we're inside a Tauri webview */
const IS_TAURI = !!(window as any).__TAURI_INTERNALS__;

async function checkHealth(baseUrl: string): Promise<boolean> {
	const controller = new AbortController();
	const timeout = setTimeout(() => controller.abort(), 3000);
	try {
		const response = await fetch(`${baseUrl}/api/health`, {
			signal: controller.signal,
		});
		return response.ok;
	} catch {
		return false;
	} finally {
		clearTimeout(timeout);
	}
}

/** Persist the server URL. Uses Tauri IPC when available, localStorage otherwise. */
async function persistUrl(url: string): Promise<void> {
	localStorage.setItem(STORAGE_KEY, url);
	if (IS_TAURI) {
		try {
			const { invoke } = await import("@tauri-apps/api/core");
			await invoke("set_server_url", { url });
		} catch {
			// Tauri command not available (e.g. dev mode without Tauri)
		}
	}
}

/** Load the persisted server URL. Tauri IPC is async so this is only
 *  used for the initial load; subsequent reads use React state. */
async function loadPersistedUrl(): Promise<string | null> {
	if (IS_TAURI) {
		try {
			const { invoke } = await import("@tauri-apps/api/core");
			const url = await invoke<string>("get_server_url");
			// The Tauri command returns the default sentinel when nothing
			// is stored. Treat it (and empty strings) as "not persisted"
			// so we fall through to localStorage.
			if (url && url !== DEFAULT_SERVER_URL) return url;
		} catch {
			// Fallback to localStorage
		}
	}
	return localStorage.getItem(STORAGE_KEY);
}

/**
 * Detect whether the SPA is served from the spacebot server itself
 * (same-origin) rather than from Tauri or a separate dev server.
 *
 * In same-origin mode (embedded via rust_embed, or behind the hosted
 * proxy with __SPACEBOT_BASE_PATH), API calls use relative paths and
 * no server URL picker is needed.
 */
function isSameOriginMode(): boolean {
	// Hosted deployment with a base path prefix
	if ((window as any).__SPACEBOT_BASE_PATH) return true;
	// Not Tauri, and served from a normal HTTP origin (not a dev server
	// with a Vite proxy). In production the spacebot binary embeds the
	// SPA at the same origin, so relative API paths just work.
	// In Vite dev mode, the proxy handles /api, so same-origin is also fine.
	if (!IS_TAURI) return true;
	return false;
}

export function ServerProvider({ children }: { children: ReactNode }) {
	const sameOrigin = isSameOriginMode();

	// Load saved URL from localStorage synchronously for first render;
	// an async effect will reconcile with Tauri's persisted value.
	const [serverUrl, setServerUrlRaw] = useState<string>(() => {
		if (sameOrigin) {
			// Same-origin or dev proxy mode: API is relative, no server
			// picker needed. Leave _serverUrl empty so getApiBase()
			// returns the relative BASE_PATH + "/api".
			return "";
		}
		// Tauri desktop mode: need an explicit server URL.
		const saved = localStorage.getItem(STORAGE_KEY);
		const initial = saved ? normalizeUrl(saved) : DEFAULT_SERVER_URL;
		// Sync to the API client module immediately so the very first
		// fetch calls use the correct URL before any effect runs.
		setClientServerUrl(initial);
		return initial;
	});

	const [state, setState] = useState<ServerState>(
		sameOrigin ? "connected" : "checking",
	);
	const [hasConnected, setHasConnected] = useState(sameOrigin);
	const [hasBootstrapped, setHasBootstrapped] = useState(sameOrigin);
	const onBootstrapped = useCallback(() => setHasBootstrapped(true), []);
	const intervalRef = useRef<ReturnType<typeof setInterval> | null>(null);

	// On mount, reconcile with Tauri-persisted URL (async)
	useEffect(() => {
		if (sameOrigin) return;
		loadPersistedUrl().then((saved) => {
			if (saved) {
				const url = normalizeUrl(saved);
				setServerUrlRaw(url);
				setClientServerUrl(url);
			}
		});
	}, [sameOrigin]);

	const runHealthCheck = useCallback(async () => {
		if (sameOrigin) {
			// Same-origin mode: API is relative; assume connected.
			setState("connected");
			setHasConnected(true);
			return;
		}
		const ok = await checkHealth(serverUrl);
		setState(ok ? "connected" : "disconnected");
		if (ok) setHasConnected(true);
	}, [serverUrl, sameOrigin]);

	const setServerUrl = useCallback((raw: string) => {
		const url = normalizeUrl(raw);
		setServerUrlRaw(url);
		persistUrl(url);
		setState("checking");
	}, []);

	// Keep the API client module's server URL in sync with context state
	useEffect(() => {
		setClientServerUrl(serverUrl);
	}, [serverUrl]);

	// Initial check + polling
	useEffect(() => {
		runHealthCheck();
		intervalRef.current = setInterval(runHealthCheck, HEALTH_CHECK_INTERVAL);
		return () => {
			if (intervalRef.current) clearInterval(intervalRef.current);
		};
	}, [runHealthCheck]);

	return (
		<ServerContext.Provider
			value={{
				serverUrl,
				state,
				setServerUrl,
				hasConnected,
				hasBootstrapped,
				onBootstrapped,
				isTauri: IS_TAURI,
				hasSidecar: IS_TAURI, // sidecar is always bundled in Tauri builds
			}}
		>
			{children}
		</ServerContext.Provider>
	);
}
