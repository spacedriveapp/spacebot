import {useEffect, useRef, type RefObject} from "react";

/** Scroll within this many pixels of the bottom counts as "user is at the
 * bottom" — small enough to feel pinned, large enough to forgive sub-pixel
 * scroll offsets and momentum overshoot. */
const NEAR_BOTTOM_PX = 64;

/** Keeps a scroll container pinned to the bottom of its content while the
 * user is already near the bottom; respects scroll-up intent so reading
 * history isn't yanked back to bottom by new messages or async layout shifts.
 *
 * Why a `ResizeObserver` instead of an effect with content deps: tool result
 * expansion, async markdown reflow (highlighter, fonts, images), and
 * `ThinkingIndicator` toggling all change height without changing the deps
 * a normal effect could watch. Observing the content directly catches them
 * all uniformly.
 *
 * Uses `behavior: "auto"`: smooth scroll animations race intervening layout
 * shifts and land short, which is the original bug. Auto repaints once,
 * then the next observed shift snaps us forward again. */
export function useStickToBottom(
	scrollRef: RefObject<HTMLElement | null>,
	contentRef: RefObject<HTMLElement | null>,
) {
	const isPinnedRef = useRef(true);

	useEffect(() => {
		const scroll = scrollRef.current;
		const content = contentRef.current;
		if (!scroll || !content) return;

		const isNearBottom = () =>
			scroll.scrollHeight - scroll.scrollTop - scroll.clientHeight <
			NEAR_BOTTOM_PX;

		/** Snap to the bottom now, then again on each of the next two
		 * animation frames. The follow-up snaps catch growth that happens
		 * AFTER the current ResizeObserver callback returns: scrollbar
		 * appearing and narrowing content (extra row of wrap), web-font
		 * swap, late Markdown layout (images, code blocks), or a sibling
		 * that mounts a frame later (e.g. a hover-action toolbar). Cheap to
		 * over-call — `scrollTop = scrollHeight` is a no-op once we're at
		 * the bottom. */
		const settleToBottom = () => {
			scroll.scrollTop = scroll.scrollHeight;
			requestAnimationFrame(() => {
				if (!isPinnedRef.current) return;
				scroll.scrollTop = scroll.scrollHeight;
				requestAnimationFrame(() => {
					if (!isPinnedRef.current) return;
					scroll.scrollTop = scroll.scrollHeight;
				});
			});
		};

		// Land at the bottom on first mount regardless of the initial
		// scrollTop value the browser remembered.
		settleToBottom();

		const onScroll = () => {
			isPinnedRef.current = isNearBottom();
		};
		scroll.addEventListener("scroll", onScroll, {passive: true});

		const ro = new ResizeObserver(() => {
			if (isPinnedRef.current) settleToBottom();
		});
		ro.observe(content);

		return () => {
			scroll.removeEventListener("scroll", onScroll);
			ro.disconnect();
		};
	}, [scrollRef, contentRef]);
}
