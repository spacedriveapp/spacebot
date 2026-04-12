export function basename(path: string): string {
	const trimmed = path.replace(/\/+$/, "");
	const idx = trimmed.lastIndexOf("/");
	return idx === -1 ? trimmed : trimmed.slice(idx + 1);
}

export function normalizePath(path: string): string {
	if (!path) return "";
	const isAbsolute = path.startsWith("/");
	const segments = path.split("/").filter(Boolean);
	const out: string[] = [];
	for (const segment of segments) {
		if (segment === ".") continue;
		if (segment === "..") {
			out.pop();
			continue;
		}
		out.push(segment);
	}
	return (isAbsolute ? "/" : "") + out.join("/");
}

export function resolvePath(base: string, relative: string): string {
	if (!relative) return normalizePath(base);
	if (relative.startsWith("/")) return normalizePath(relative);
	// Join base + relative and normalize (handles .. segments)
	const joined = base.replace(/\/+$/, "") + "/" + relative;
	return normalizePath(joined);
}
