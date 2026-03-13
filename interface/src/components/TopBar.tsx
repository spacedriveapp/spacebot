import { createContext, useContext, useRef, useCallback, useSyncExternalStore, type ReactNode, type MouseEvent } from "react";
import { Link } from "@tanstack/react-router";
import { BASE_PATH, IS_TAURI } from "@/api/client";
import { Menu01Icon } from "@hugeicons/core-free-icons";
import { HugeiconsIcon } from "@hugeicons/react";

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

export function useSetTopBar(node: ReactNode) {
	const store = useContext(TopBarContext);
	if (!store) throw new Error("useSetTopBar must be used within TopBarProvider");
	store.setContent(node);
}

interface TopBarProps {
	isMobile?: boolean;
	onOpenMobileNav?: () => void;
}

export function TopBar({ isMobile = false, onOpenMobileNav }: TopBarProps) {
	const store = useContext(TopBarContext);
	if (!store) throw new Error("TopBar must be used within TopBarProvider");

	const content = useSyncExternalStore(
		store.subscribe,
		store.getSnapshot,
		store.getSnapshot,
	);

	const handleMouseDown = useCallback((e: MouseEvent) => {
		if (!IS_TAURI) return;
		if (e.buttons !== 1) return;
		const target = e.target as HTMLElement;
		if (target.closest("a, button, input, select, textarea, [role=button]")) return;
		e.preventDefault();
		(window as any).__TAURI_INTERNALS__.invoke("plugin:window|start_dragging");
	}, []);

	return (
		<div
			className={`flex h-12 w-full shrink-0 border-b border-app-line bg-app-darkBox/50${IS_TAURI ? " select-none" : ""}`}
			onMouseDown={handleMouseDown}
		>
			{IS_TAURI ? (
				<div className="w-[72px] shrink-0" />
			) : isMobile ? (
				<div className="flex shrink-0 items-center border-r border-sidebar-line bg-sidebar px-2">
					<button
						type="button"
						onClick={onOpenMobileNav}
						className="flex h-8 w-8 items-center justify-center rounded-md text-sidebar-inkDull hover:bg-sidebar-selected/50 hover:text-sidebar-ink"
						aria-label="Open navigation"
					>
						<HugeiconsIcon icon={Menu01Icon} className="h-4 w-4" />
					</button>
				</div>
			) : (
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

			<div className="flex min-w-0 flex-1 items-center overflow-hidden">{content}</div>
		</div>
	);
}
