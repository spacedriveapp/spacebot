import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { ReactQueryDevtools } from "@tanstack/react-query-devtools";
import { RouterProvider } from "@tanstack/react-router";
import { ErrorBoundary } from "@/components/ErrorBoundary";
import { DesktopInstanceDialog, DesktopInstanceGate } from "@/components/DesktopInstanceDialog";
import { useDesktopInstance, DesktopInstanceProvider } from "@/hooks/useDesktopInstance";
import { LiveContextProvider } from "@/hooks/useLiveContext";
import { router } from "@/router";

const queryClient = new QueryClient({
	defaultOptions: {
		queries: {
			staleTime: 30_000,
			retry: 1,
			refetchOnWindowFocus: true,
		},
	},
});

function AppShell() {
	const { isDesktop, hasConfiguredInstance } = useDesktopInstance();

	if (isDesktop && !hasConfiguredInstance) {
		return <DesktopInstanceGate />;
	}

	return (
		<ErrorBoundary>
			<QueryClientProvider client={queryClient}>
				<LiveContextProvider>
					<RouterProvider router={router} />
				</LiveContextProvider>
				<DesktopInstanceDialog />
				{import.meta.env.DEV && (
				<ReactQueryDevtools initialIsOpen={false} buttonPosition="bottom-right" />
			)}
			</QueryClientProvider>
		</ErrorBoundary>
	);
}

export function App() {
	return (
		<DesktopInstanceProvider>
			<AppShell />
		</DesktopInstanceProvider>
	);
}
