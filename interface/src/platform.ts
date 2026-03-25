/**
 * Platform abstraction layer.
 *
 * The interface/ code is platform-agnostic. All host-specific behaviour
 * (Tauri desktop, web browser, future Electron, etc.) goes through this
 * module. Nothing in interface/ should ever import `@tauri-apps/*` directly.
 *
 * In web/browser mode every operation is a no-op or uses standard Web APIs.
 */

export const IS_TAURI: boolean = !!(window as any).__TAURI_INTERNALS__;

// ── Window management ───────────────────────────────────────────────────

/**
 * Resize a named overlay window to the given logical dimensions.
 * The host is responsible for repositioning (e.g. keeping the window
 * bottom-pinned). In browser mode this is a no-op.
 */
export async function resizeWindow(label: string, width: number, height: number): Promise<void> {
	if (!IS_TAURI) return;
	try {
		const {invoke} = await import("@tauri-apps/api/core");
		await invoke("resize_overlay_window", {label, width, height});
	} catch (error) {
		console.warn(`platform: resize failed for "${label}":`, error);
	}
}

/**
 * Start dragging the current window (Tauri only, for custom titlebars).
 */
export function startDragging(): void {
	if (!IS_TAURI) return;
	(window as any).__TAURI_INTERNALS__.invoke("plugin:window|start_dragging");
}

// ── IPC ─────────────────────────────────────────────────────────────────

/**
 * Invoke a host command by name. Returns undefined in browser mode.
 * Use this for any Tauri IPC that doesn't have a dedicated helper above.
 */
export async function invoke<T = void>(command: string, args?: Record<string, unknown>): Promise<T | undefined> {
	if (!IS_TAURI) return undefined;
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
	if (!IS_TAURI) return () => {};
	try {
		const {listen: tauriListen} = await import("@tauri-apps/api/event");
		return await tauriListen(event, () => handler());
	} catch {
		return () => {};
	}
}
