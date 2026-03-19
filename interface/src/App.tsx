import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { ReactQueryDevtools } from "@tanstack/react-query-devtools";
import { RouterProvider } from "@tanstack/react-router";
import { ErrorBoundary } from "@/components/ErrorBoundary";
import { ConnectionScreen } from "@/components/ConnectionScreen";
import { LiveContextProvider } from "@/hooks/useLiveContext";
import { ServerProvider, useServer } from "@/hooks/useServer";
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

/**
 * Inner shell: shows the connection screen until the server is
 * reachable and initial data has loaded, then renders the main app.
 */
function AppShell() {
	const { state, hasBootstrapped, onBootstrapped } = useServer();

	// Show connection screen until we've both connected AND loaded
	// initial data. This prevents flashing the main shell before
	// LiveContextProvider's bootstrap queries complete.
	if (state !== "connected" && !hasBootstrapped) {
		return <ConnectionScreen />;
	}

	return (
		<LiveContextProvider onBootstrapped={onBootstrapped}>
			<RouterProvider router={router} />
		</LiveContextProvider>
	);
}

export function App() {
	return (
		<ErrorBoundary>
			<QueryClientProvider client={queryClient}>
				<ServerProvider>
					<AppShell />
				</ServerProvider>
				{import.meta.env.DEV && !window.location.pathname.endsWith("/overlay") && (
					<ReactQueryDevtools initialIsOpen={false} buttonPosition="bottom-right" />
				)}
			</QueryClientProvider>
		</ErrorBoundary>
	);
}
