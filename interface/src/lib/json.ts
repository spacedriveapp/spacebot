/**
 * Narrowing helpers for free-form JSON coming off the API.
 *
 * Several fields are `serde_json::Value` on the server, which the generated
 * schema types as `unknown` — correctly, since nothing guarantees they are
 * objects. Consumers narrow here rather than asserting a shape the server
 * never promised.
 */

export function isRecord(value: unknown): value is Record<string, unknown> {
	return typeof value === "object" && value !== null && !Array.isArray(value);
}

/** The value as an object, or an empty one when it is anything else. */
export function asRecord(value: unknown): Record<string, unknown> {
	return isRecord(value) ? value : {};
}

/** Whether a free-form JSON value carries anything worth rendering. */
export function hasContent(value: unknown): boolean {
	if (value === null || value === undefined) return false;
	if (isRecord(value)) return Object.keys(value).length > 0;
	if (Array.isArray(value)) return value.length > 0;
	return true;
}
