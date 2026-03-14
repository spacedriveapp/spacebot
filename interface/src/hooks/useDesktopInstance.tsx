import { createContext, useContext, useMemo, useState, type ReactNode } from "react";
import {
	getDesktopInstanceLabel,
	getDesktopInstanceUrl,
	hasSavedDesktopInstanceUrl,
	isTauriDesktop,
	saveDesktopInstanceUrl,
} from "@/lib/backend";

interface DesktopInstanceContextValue {
	isDesktop: boolean;
	currentUrl: string;
	currentLabel: string;
	hasConfiguredInstance: boolean;
	dialogOpen: boolean;
	openDialog: () => void;
	closeDialog: () => void;
	connectToInstance: (value: string) => string;
}

const DesktopInstanceContext = createContext<DesktopInstanceContextValue>({
	isDesktop: false,
	currentUrl: "",
	currentLabel: "",
	hasConfiguredInstance: false,
	dialogOpen: false,
	openDialog: () => {},
	closeDialog: () => {},
	connectToInstance: (value) => value,
});

export function DesktopInstanceProvider({ children }: { children: ReactNode }) {
	const isDesktop = isTauriDesktop();
	const [currentUrl, setCurrentUrl] = useState(() => getDesktopInstanceUrl());
	const [dialogOpen, setDialogOpen] = useState(false);

	const value = useMemo<DesktopInstanceContextValue>(() => ({
		isDesktop,
		currentUrl,
		currentLabel: getDesktopInstanceLabel(currentUrl),
		hasConfiguredInstance: hasSavedDesktopInstanceUrl(),
		dialogOpen,
		openDialog: () => setDialogOpen(true),
		closeDialog: () => setDialogOpen(false),
		connectToInstance: (value: string) => {
			const nextUrl = saveDesktopInstanceUrl(value);
			setCurrentUrl(nextUrl);
			window.location.reload();
			return nextUrl;
		},
	}), [currentUrl, dialogOpen, isDesktop]);

	return (
		<DesktopInstanceContext.Provider value={value}>
			{children}
		</DesktopInstanceContext.Provider>
	);
}

export function useDesktopInstance() {
	return useContext(DesktopInstanceContext);
}
