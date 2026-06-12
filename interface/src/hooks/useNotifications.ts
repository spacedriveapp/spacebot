import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import { api, type NotificationItem } from "@/api/client";

export const NOTIFICATIONS_QUERY_KEY = ["notifications"];

export function useNotifications(filter?: "unread" | "all") {
	const queryClient = useQueryClient();

	const { data, isLoading } = useQuery({
		queryKey: [...NOTIFICATIONS_QUERY_KEY, filter ?? "unread"],
		queryFn: () => api.listNotifications({ filter: filter ?? "unread" }),
		staleTime: 30_000,
	});

	const markRead = useMutation({
		mutationFn: (id: string) => api.markNotificationRead(id),
		onSuccess: () => queryClient.invalidateQueries({ queryKey: NOTIFICATIONS_QUERY_KEY }),
	});

	const dismiss = useMutation({
		mutationFn: (id: string) => api.dismissNotification(id),
		onSuccess: () => queryClient.invalidateQueries({ queryKey: NOTIFICATIONS_QUERY_KEY }),
	});

	const markAllRead = useMutation({
		mutationFn: () => api.markAllNotificationsRead(),
		onSuccess: () => queryClient.invalidateQueries({ queryKey: NOTIFICATIONS_QUERY_KEY }),
	});

	const dismissRead = useMutation({
		mutationFn: () => api.dismissReadNotifications(),
		onSuccess: () => queryClient.invalidateQueries({ queryKey: NOTIFICATIONS_QUERY_KEY }),
	});

	const notifications: NotificationItem[] = data?.notifications ?? [];

	return {
		notifications,
		isLoading,
		markRead: (id: string) => markRead.mutate(id),
		dismiss: (id: string) => dismiss.mutate(id),
		markAllRead: () => markAllRead.mutate(),
		dismissRead: () => dismissRead.mutate(),
	};
}
