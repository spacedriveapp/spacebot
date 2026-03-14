const DEFAULT_DESKTOP_INSTANCE_URL = "http://127.0.0.1:19898";
const DESKTOP_INSTANCE_STORAGE_KEY = "spacebot-desktop-instance-url";

function basePath(): string {
	return (window as any).__SPACEBOT_BASE_PATH || "";
}

export function isTauriDesktop(): boolean {
	return typeof window !== "undefined" && !!(window as any).__TAURI_INTERNALS__;
}

export function normalizeDesktopInstanceUrl(value: string): string {
	const trimmed = value.trim();
	if (!trimmed) {
		throw new Error("Enter a Spacebot instance URL.");
	}

	const withProtocol = /^https?:\/\//i.test(trimmed) ? trimmed : `http://${trimmed}`;
	const url = new URL(withProtocol);
	url.pathname = "";
	url.search = "";
	url.hash = "";

	return url.origin.replace(/\/$/, "");
}

export function getDesktopInstanceUrl(): string {
	if (!isTauriDesktop()) {
		return window.location.origin;
	}

	try {
		const stored = localStorage.getItem(DESKTOP_INSTANCE_STORAGE_KEY);
		if (!stored) return DEFAULT_DESKTOP_INSTANCE_URL;
		return normalizeDesktopInstanceUrl(stored);
	} catch {
		return DEFAULT_DESKTOP_INSTANCE_URL;
	}
}

export function hasSavedDesktopInstanceUrl(): boolean {
	if (!isTauriDesktop() || typeof window === "undefined") return false;
	const stored = localStorage.getItem(DESKTOP_INSTANCE_STORAGE_KEY);
	if (!stored) return false;
	try {
		normalizeDesktopInstanceUrl(stored);
		return true;
	} catch {
		return false;
	}
}

export function saveDesktopInstanceUrl(value: string): string {
	const normalized = normalizeDesktopInstanceUrl(value);
	if (typeof window !== "undefined") {
		localStorage.setItem(DESKTOP_INSTANCE_STORAGE_KEY, normalized);
	}
	return normalized;
}

export function getDesktopInstanceLabel(value: string): string {
	try {
		const url = new URL(value);
		return url.host;
	} catch {
		return value;
	}
}

export function getApiBaseUrl(): string {
	return isTauriDesktop() ? `${getDesktopInstanceUrl()}/api` : `${basePath()}/api`;
}

export function assetUrl(path: string): string {
	return `${basePath()}${path}`;
}
