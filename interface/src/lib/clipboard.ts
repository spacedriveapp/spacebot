/**
 * Copy text to the clipboard, everywhere the app actually runs.
 *
 * `navigator.clipboard` only exists in a secure context. On `localhost` it is
 * there; served over plain HTTP from a LAN or tailnet address it is `undefined`,
 * and `navigator.clipboard.writeText(...)` throws a TypeError rather than
 * failing gracefully. Half the call sites guarded with `?.` and silently did
 * nothing; the other half threw into an unhandled rejection. Neither told the
 * person clicking that their copy did not happen.
 *
 * The fallback is the old `execCommand("copy")` path, which is deprecated but
 * still works in every browser this app targets and needs no secure context.
 */
export async function copyText(text: string): Promise<boolean> {
	if (navigator.clipboard?.writeText) {
		try {
			await navigator.clipboard.writeText(text);
			return true;
		} catch {
			// Permission denied, or a context that advertises the API and then
			// refuses. Fall through rather than giving up.
		}
	}

	return legacyCopy(text);
}

function legacyCopy(text: string): boolean {
	try {
		const area = document.createElement("textarea");
		area.value = text;
		// Off-screen rather than hidden: a display:none element cannot be
		// selected, and the copy silently yields an empty string.
		area.style.position = "fixed";
		area.style.top = "-9999px";
		area.setAttribute("readonly", "");
		document.body.appendChild(area);
		area.select();
		const copied = document.execCommand("copy");
		document.body.removeChild(area);
		return copied;
	} catch {
		return false;
	}
}
