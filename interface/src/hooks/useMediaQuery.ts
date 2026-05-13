import {useEffect, useState} from "react";

export function useMediaQuery(query: string): boolean {
	const getMatch = () => {
		if (typeof window === "undefined") return false;
		return window.matchMedia(query).matches;
	};

	const [matches, setMatches] = useState<boolean>(getMatch);

	useEffect(() => {
		if (typeof window === "undefined") return;
		const mql = window.matchMedia(query);
		const handler = (e: MediaQueryListEvent) => setMatches(e.matches);
		setMatches(mql.matches);
		mql.addEventListener("change", handler);
		return () => mql.removeEventListener("change", handler);
	}, [query]);

	return matches;
}

export function useIsMobile(): boolean {
	return useMediaQuery("(max-width: 767px)");
}
