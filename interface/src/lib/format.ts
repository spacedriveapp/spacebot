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

export function formatCronSchedule(cronExpr: string | null, intervalSecs: number): string {
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

export function platformColor(platform: string): string {
	switch (platform) {
		case "discord": return "bg-indigo-500/20 text-indigo-400";
		case "slack": return "bg-green-500/20 text-green-400";
		case "telegram": return "bg-blue-500/20 text-blue-400";
		case "twitch": return "bg-purple-500/20 text-purple-400";
		case "cron": return "bg-amber-500/20 text-amber-400";
		default: return "bg-gray-500/20 text-gray-400";
	}
}

// E.164 Phone Number Validation
// Validates international phone numbers in format: + followed by country code and 6-15 digits
export const E164_REGEX = /^\+[1-9]\d{5,14}$/;

export const E164_ERROR_TEXT = 
	"Phone number must be in E.164 format: + followed by country code and 6-15 digits (e.g., +1234567890)";

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
