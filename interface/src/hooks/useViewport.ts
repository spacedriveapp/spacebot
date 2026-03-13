import { useEffect, useState } from "react";

const MOBILE_MAX_WIDTH = 649;
const TABLET_MAX_WIDTH = 867;

function getViewportWidth(): number {
	if (typeof window === "undefined") return TABLET_MAX_WIDTH;
	return window.innerWidth;
}

export function useViewport() {
	const [width, setWidth] = useState<number>(getViewportWidth);

	useEffect(() => {
		const onResize = () => setWidth(getViewportWidth());
		window.addEventListener("resize", onResize);
		return () => window.removeEventListener("resize", onResize);
	}, []);

	const isMobile = width <= MOBILE_MAX_WIDTH;
	const isTablet = width > MOBILE_MAX_WIDTH && width <= TABLET_MAX_WIDTH;
	const isDesktop = width > TABLET_MAX_WIDTH;

	return { width, isMobile, isTablet, isDesktop };
}

export function useIsMobile() {
	return useViewport().isMobile;
}
