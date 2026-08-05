export function formatUptime(seconds: number): string {
	const hours = Math.floor(seconds / 3600);
	const minutes = Math.floor((seconds % 3600) / 60);
	const secs = seconds % 60;
	if (hours > 0) return `${hours}h ${minutes}m`;
	if (minutes > 0) return `${minutes}m ${secs}s`;
	return `${secs}s`;
}

export function formatTimeAgo(dateStr: string): string {
	const seconds = Math.floor((Date.now() - new Date(dateStr).getTime()) / 1000);
	if (seconds < 60) return "just now";
	if (seconds < 3600) return `${Math.floor(seconds / 60)}m ago`;
	if (seconds < 86400) return `${Math.floor(seconds / 3600)}h ago`;
	return `${Math.floor(seconds / 86400)}d ago`;
}

export function formatTimestamp(ts: number): string {
	return new Date(ts).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
}

export function formatDuration(seconds: number): string {
	if (seconds < 60) return `${seconds}s`;
	if (seconds % 86400 === 0) return `${seconds / 86400}d`;
	if (seconds % 3600 === 0) return `${seconds / 3600}h`;
	if (seconds % 60 === 0) return `${seconds / 60}m`;
	return `${Math.floor(seconds / 60)}m ${seconds % 60}s`;
}

// Nullable server fields arrive as `null`, and an absent optional as
// `undefined`. Accept both rather than making every caller normalise.
export function formatCronSchedule(
	cronExpr: string | null | undefined,
	intervalSecs: number,
): string {
	if (cronExpr) return cronExpr;
	if (intervalSecs % 86400 === 0) return `every ${intervalSecs / 86400}d`;
	if (intervalSecs % 3600 === 0) return `every ${intervalSecs / 3600}h`;
	if (intervalSecs % 60 === 0) return `every ${intervalSecs / 60}m`;
	return `every ${intervalSecs}s`;
}

export function platformIcon(platform: string): string {
	switch (platform) {
		case "discord": return "Discord";
		case "slack": return "Slack";
		case "telegram": return "Telegram";
		case "twitch": return "Twitch";
		case "webhook": return "Webhook";
		case "cron": return "Cron";
		default: return platform;
	}
}

/**
 * Platform chips used to be tinted one hue per platform — Discord indigo, Slack
 * green, Telegram blue, Twitch purple, cron amber. The token palette has no
 * decorative hues (only `accent`, the `ink`/`app` scales and the four status
 * colours), and none of these platforms is a status, so tinting them would mean
 * claiming e.g. Slack is a success and Twitch is an error. Every chip therefore
 * uses the neutral surface; the chip already spells the platform out in its
 * label (see `platformIcon`), so only the tint is lost.
 */
export function platformColor(_platform: string): string {
	return "bg-app-box text-ink-dull";
}

// E.164 Phone Number Validation
// Validates international phone numbers in format: + followed by country code and 6-15 digits
export const E164_REGEX = /^\+[1-9]\d{5,14}$/;

export const E164_ERROR_TEXT = 
	"Phone number must be in E.164 format: + followed by 6-15 digits after '+', with the first digit 1-9 (e.g., +1234567890)";

export function isValidE164(phoneNumber: string): boolean {
	return E164_REGEX.test(phoneNumber.trim());
}

export function validateE164(phoneNumber: string): { valid: boolean; error?: string } {
	const trimmed = phoneNumber.trim();
	if (!trimmed) {
		return { valid: false, error: "Phone number is required" };
	}
	if (!E164_REGEX.test(trimmed)) {
		return { valid: false, error: E164_ERROR_TEXT };
	}
	return { valid: true };
}

/**
 * Validate Signal DM allowed-users entries.
 * Each entry must be E.164 phone or uuid:xxx.
 */
export function validateSignalDmAllowedUsers(
	raw: string
): { valid: true; entries: string[] } | { valid: false; error: string } {
	const entries = raw.split(',').map(s => s.trim()).filter(s => s.length > 0);
	const invalid: string[] = [];
	const valid: string[] = [];

	for (const entry of entries) {
		if (
			isValidE164(entry) ||
			(entry.startsWith('uuid:') && entry.length > 5)
		) {
			valid.push(entry);
		} else {
			invalid.push(entry);
		}
	}

	if (invalid.length > 0) {
		return {
			valid: false,
			error: `Invalid entries: ${invalid.join(', ')}. Must be E.164 phone numbers (+1234567890) or uuid:xxx`,
		};
	}

	return { valid: true, entries: valid };
}
