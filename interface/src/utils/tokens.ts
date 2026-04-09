/**
 * Token usage utilities
 */

/**
 * Format token count with k suffix above 1000
 */
export const formatTokens = (tokens: number): string => {
	if (tokens >= 1000) {
		return `${(tokens / 1000).toFixed(1)}k`;
	}
	return tokens.toString();
};

/**
 * Determine color based on usage ratio: neutral below 60%, yellow 60-80%, red above 80%
 */
export const getTokenUsageColor = (ratio: number): string => {
	if (ratio >= 0.8) return "text-red-400"; // Red at 80%+
	if (ratio >= 0.6) return "text-yellow-400"; // Yellow at 60-80%
	return "text-ink-faint"; // Neutral below 60%
};
