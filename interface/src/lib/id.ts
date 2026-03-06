/**
 * Generate a unique ID, preferring crypto.randomUUID() when available.
 *
 * crypto.randomUUID() requires a secure context (HTTPS or localhost).
 * Over plain HTTP (e.g. Tailscale) it's unavailable, so we fall back
 * to Date.now + Math.random which is sufficient for ephemeral client-side IDs.
 */
export function generateId(): string {
	try {
		return crypto.randomUUID();
	} catch {
		return `${Date.now()}-${Math.random().toString(36).slice(2)}`;
	}
}
