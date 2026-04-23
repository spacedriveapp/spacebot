// LocalStorage-backed per-node color overrides for the mermaid view.
// Keyed by file qualified_name. Layer/folder coloring is derived and
// intentionally not user-editable.

export type NodeColorOverrides = Record<string, string>;

const NODE_KEY = (projectId: string) => `spacebot.codegraph.mermaid.nodeColors.${projectId}`;

function readJson<T>(key: string, fallback: T): T {
	try {
		const raw = window.localStorage.getItem(key);
		if (!raw) return fallback;
		const parsed = JSON.parse(raw);
		if (parsed && typeof parsed === "object") return parsed as T;
		return fallback;
	} catch {
		return fallback;
	}
}

function writeJson(key: string, value: unknown) {
	try { window.localStorage.setItem(key, JSON.stringify(value)); } catch { /* ignore */ }
}

export function loadNodeColors(projectId: string): NodeColorOverrides {
	return readJson<NodeColorOverrides>(NODE_KEY(projectId), {});
}

export function saveNodeColor(projectId: string, fileQname: string, color: string | null) {
	const all = loadNodeColors(projectId);
	if (color === null) delete all[fileQname];
	else all[fileQname] = color;
	writeJson(NODE_KEY(projectId), all);
}
