import { createContext, useContext, useRef, useCallback, useSyncExternalStore, type ReactNode, type MouseEvent } from "react";
import { Link } from "@tanstack/react-router";
import { BASE_PATH } from "@/api/client";
import { IS_TAURI, startDragging } from "@/platform";

// ── Context ──────────────────────────────────────────────────────────────

interface TopBarStore {
	content: ReactNode;
	subscribe: (cb: () => void) => () => void;
	setContent: (node: ReactNode) => void;
	getSnapshot: () => ReactNode;
}

function createTopBarStore(): TopBarStore {
	let content: ReactNode = null;
	const listeners = new Set<() => void>();

	return {
		get content() {
			return content;
		},
		subscribe(cb: () => void) {
			listeners.add(cb);
			return () => listeners.delete(cb);
		},
		setContent(node: ReactNode) {
			content = node;
			for (const cb of listeners) cb();
		},
		getSnapshot() {
			return content;
		},
	};
}

const TopBarContext = createContext<TopBarStore | null>(null);

export function TopBarProvider({ children }: { children: ReactNode }) {
	const storeRef = useRef<TopBarStore>(null);
	if (!storeRef.current) {
		storeRef.current = createTopBarStore();
	}
	return (
		<TopBarContext.Provider value={storeRef.current}>
			{children}
		</TopBarContext.Provider>
	);
}

/**
 * Hook for routes to set topbar content. Content is displayed in the TopBar
 * component rendered at the root layout level.
 *
 * The component that calls this hook "owns" the topbar content for its lifetime.
 * Uses a ref + effect to avoid re-render loops.
 */
export function useSetTopBar(node: ReactNode) {
	const store = useContext(TopBarContext);
	if (!store) throw new Error("useSetTopBar must be used within TopBarProvider");

	// Update content synchronously during render (like useSyncExternalStore pattern).
	// This avoids the useEffect loop entirely.
	store.setContent(node);
}

// ── Component ────────────────────────────────────────────────────────────

export function TopBar() {
	const store = useContext(TopBarContext);
	if (!store) throw new Error("TopBar must be used within TopBarProvider");

	const content = useSyncExternalStore(
		store.subscribe,
		store.getSnapshot,
		store.getSnapshot,
	);

	const handleMouseDown = useCallback((e: MouseEvent) => {
		if (!IS_TAURI) return;
		// Only drag on primary button, and not when clicking interactive elements
		if (e.buttons !== 1) return;
		const target = e.target as HTMLElement;
		if (target.closest("a, button, input, select, textarea, [role=button]")) return;
		e.preventDefault();
		startDragging();
	}, []);

	return (
		<div
			className={`flex h-12 w-full shrink-0 border-b border-app-line bg-app-darkBox/50${IS_TAURI ? " select-none" : ""}`}
			onMouseDown={handleMouseDown}
		>
			{/* Left corner block */}
			{IS_TAURI ? (
				/* Tauri: padding for macOS traffic lights */
				<div className="w-[72px] shrink-0" />
			) : (
				/* Web: ball icon block matching sidebar width + border */
				<Link
					to="/"
					className="flex w-14 shrink-0 items-center justify-center border-r border-sidebar-line bg-sidebar"
				>
					<img
						src={`${BASE_PATH}/ball.png`}
						alt=""
						className="h-6 w-6 transition-transform duration-150 ease-out hover:scale-110 active:scale-95"
						draggable={false}
					/>
				</Link>
			)}

			{/* Route-controlled content area */}
			<div className="flex min-w-0 flex-1 items-center">
				{content}
			</div>
		</div>
	);
}
