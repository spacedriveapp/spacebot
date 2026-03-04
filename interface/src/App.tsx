import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { ReactQueryDevtools } from "@tanstack/react-query-devtools";
import { RouterProvider } from "@tanstack/react-router";
import { ErrorBoundary } from "@/components/ErrorBoundary";
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

export function App() {
	return (
		<ErrorBoundary>
			<QueryClientProvider client={queryClient}>
				<LiveContextProvider>
					<RouterProvider router={router} />
				</LiveContextProvider>
				{import.meta.env.DEV && (
				<ReactQueryDevtools initialIsOpen={false} buttonPosition="bottom-right" />
			)}
			</QueryClientProvider>
		</ErrorBoundary>
	);
}
