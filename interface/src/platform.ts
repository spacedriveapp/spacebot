/**
 * Platform abstraction layer.
 *
 * The interface/ code is platform-agnostic. All host-specific behaviour
 * (Tauri desktop, web browser, future Electron, etc.) goes through this
 * module. Nothing in interface/ should ever import `@tauri-apps/*` directly.
 *
 * In web/browser mode every operation is a no-op or uses standard Web APIs.
 */

export type HostKind = "browser" | "desktop";
export type OperatingSystem = "macos" | "windows" | "linux" | "unknown";

export interface PlatformInfo {
	host: HostKind;
	os: OperatingSystem;
	hasBundledServer: boolean;
}

interface NavigatorWithUserAgentData extends Navigator {
	userAgentData?: {
		platform?: string;
	};
}

interface BundledProcessCloseEvent {
	code: number | null;
}

interface BundledProcessHandlers {
	onError?: (error: string) => void;
	onClose?: (event: BundledProcessCloseEvent) => void;
	onStdout?: (line: string) => void;
}

function getDesktopBridge(): {invoke: (command: string, args?: unknown) => unknown} | null {
	if (typeof window === "undefined") return null;
	return (window as any).__TAURI_INTERNALS__ ?? null;
}

function detectOperatingSystem(): OperatingSystem {
	if (typeof navigator === "undefined") return "unknown";

	const browserNavigator = navigator as NavigatorWithUserAgentData;
	const platformHints = [
		browserNavigator.userAgentData?.platform,
		navigator.platform,
		navigator.userAgent,
	]
		.filter((value): value is string => typeof value === "string" && value.length > 0)
		.join(" ")
		.toLowerCase();

	if (/(iphone|ipad|ipod)/.test(platformHints)) return "unknown";
	if (/(mac|darwin)/.test(platformHints)) return "macos";
	if (/(win|windows)/.test(platformHints)) return "windows";
	if (/(linux|x11)/.test(platformHints)) return "linux";
	return "unknown";
}

const host: HostKind = getDesktopBridge() ? "desktop" : "browser";

export const PLATFORM: PlatformInfo = Object.freeze({
	host,
	os: detectOperatingSystem(),
	hasBundledServer: host === "desktop",
});

export const IS_DESKTOP = PLATFORM.host === "desktop";
export const IS_MACOS = PLATFORM.os === "macos";
export const HAS_BUNDLED_SERVER = PLATFORM.hasBundledServer;

const DESKTOP_DRAG_REGION_ATTRIBUTES = Object.freeze({
	"data-tauri-drag-region": "true",
});
const NO_DRAG_REGION_ATTRIBUTES = Object.freeze({}) as Readonly<Record<string, string>>;

export function dragRegionAttributes(): Readonly<Record<string, string>> {
	return IS_DESKTOP ? DESKTOP_DRAG_REGION_ATTRIBUTES : NO_DRAG_REGION_ATTRIBUTES;
}

export async function spawnBundledProcess(
	binary: string,
	args: string[],
	handlers: BundledProcessHandlers = {},
): Promise<boolean> {
	if (!HAS_BUNDLED_SERVER) return false;

	const {Command} = await import("@tauri-apps/plugin-shell");
	const command = Command.sidecar(binary, args);

	if (handlers.onError) {
		command.on("error", (error: string) => {
			handlers.onError?.(error);
		});
	}

	if (handlers.onClose) {
		command.on("close", (event: {code: number | null}) => {
			handlers.onClose?.({code: event.code});
		});
	}

	if (handlers.onStdout) {
		command.stdout.on("data", (line: string) => {
			handlers.onStdout?.(line);
		});
	}

	await command.spawn();
	return true;
}

// ── Window management ───────────────────────────────────────────────────

/**
 * Resize a named overlay window to the given logical dimensions.
 * The host is responsible for repositioning (e.g. keeping the window
 * bottom-pinned). In browser mode this is a no-op.
 */
export async function resizeWindow(label: string, width: number, height: number): Promise<void> {
	if (!IS_DESKTOP) return;
	try {
		const {invoke} = await import("@tauri-apps/api/core");
		await invoke("resize_overlay_window", {label, width, height});
	} catch (error) {
		console.warn(`platform: resize failed for "${label}":`, error);
	}
}

/**
 * Start dragging the current desktop window for custom titlebars.
 */
export function startDragging(): void {
	const desktopBridge = getDesktopBridge();
	if (!desktopBridge) return;
	desktopBridge.invoke("plugin:window|start_dragging");
}

// ── IPC ─────────────────────────────────────────────────────────────────

/**
 * Invoke a host command by name. Returns undefined in browser mode.
 * Use this for any Tauri IPC that doesn't have a dedicated helper above.
 */
export async function invoke<T = void>(command: string, args?: Record<string, unknown>): Promise<T | undefined> {
	if (!IS_DESKTOP) return undefined;
	const {invoke: tauriInvoke} = await import("@tauri-apps/api/core");
	return tauriInvoke<T>(command, args);
}

// ── Events ──────────────────────────────────────────────────────────────

type UnlistenFn = () => void;

/**
 * Subscribe to a named event from the host shell.
 * Returns an unlisten function. In browser mode returns a no-op.
 */
export async function listen(event: string, handler: () => void): Promise<UnlistenFn> {
	if (!IS_DESKTOP) return () => {};
	try {
		const {listen: tauriListen} = await import("@tauri-apps/api/event");
		return await tauriListen(event, () => handler());
	} catch {
		return () => {};
	}
}
