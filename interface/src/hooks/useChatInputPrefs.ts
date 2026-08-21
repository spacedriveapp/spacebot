import {useCallback, useEffect, useState} from "react";

export interface ChatInputPrefs {
	enterToSubmit: boolean;
}

const STORAGE_KEY = "spacebot-chat-input-prefs";
const DEFAULTS: ChatInputPrefs = {enterToSubmit: true};

function readStored(): ChatInputPrefs {
	if (typeof window === "undefined") return DEFAULTS;
	try {
		const raw = localStorage.getItem(STORAGE_KEY);
		if (!raw) return DEFAULTS;
		const parsed = JSON.parse(raw);
		return {
			enterToSubmit:
				typeof parsed?.enterToSubmit === "boolean"
					? parsed.enterToSubmit
					: DEFAULTS.enterToSubmit,
		};
	} catch {
		return DEFAULTS;
	}
}

export function useChatInputPrefs() {
	const [prefs, setPrefs] = useState<ChatInputPrefs>(readStored);

	useEffect(() => {
		const onStorage = (e: StorageEvent) => {
			if (e.key === STORAGE_KEY) setPrefs(readStored());
		};
		window.addEventListener("storage", onStorage);
		return () => window.removeEventListener("storage", onStorage);
	}, []);

	const setEnterToSubmit = useCallback((value: boolean) => {
		setPrefs((prev) => {
			const next = {...prev, enterToSubmit: value};
			localStorage.setItem(STORAGE_KEY, JSON.stringify(next));
			return next;
		});
	}, []);

	return {prefs, setEnterToSubmit};
}
