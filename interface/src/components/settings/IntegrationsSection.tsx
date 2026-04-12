import {useState} from "react";
import {useQuery, useMutation, useQueryClient} from "@tanstack/react-query";
import {api} from "@/api/client";
import type {IntegrationEntry} from "@/api/client";
import type {ComponentProps} from "react";
import {Badge, Button, Input, Switch} from "@spacedrive/primitives";
import {OpenCodeConfigForm} from "./OpenCodeConfigForm";

type BadgeVariant = ComponentProps<typeof Badge>["variant"];

const STATUS_VARIANT: Record<string, BadgeVariant> = {
	connected: "success",
	disconnected: "warning",
	unconfigured: "warning",
	disabled: "secondary",
};

function SpacedriveConfigForm({
	integration,
	onSaved,
}: {
	integration: IntegrationEntry;
	onSaved: () => void;
}) {
	const queryClient = useQueryClient();
	const [apiUrl, setApiUrl] = useState(
		(integration.config.api_url as string) ?? "",
	);
	const [webUrl, setWebUrl] = useState(
		(integration.config.web_url as string) ?? "",
	);
	const [libraryId, setLibraryId] = useState(
		(integration.config.library_id as string) ?? "",
	);
	const [message, setMessage] = useState<{
		text: string;
		type: "success" | "error";
	} | null>(null);

	const mutation = useMutation({
		mutationFn: (config: Record<string, unknown>) =>
			api.updateIntegration("spacedrive", config),
		onSuccess: (result) => {
			if (result.success) {
				setMessage({text: result.message, type: "success"});
				queryClient.invalidateQueries({queryKey: ["integrations"]});
				queryClient.invalidateQueries({queryKey: ["global-settings"]});
				onSaved();
			} else {
				setMessage({text: result.message, type: "error"});
			}
		},
		onError: (error) =>
			setMessage({text: `Failed: ${error.message}`, type: "error"}),
	});

	return (
		<div className="flex flex-col gap-3 pt-3">
			<label className="block">
				<span className="text-tiny font-medium text-ink-dull">API URL</span>
				<Input
					type="text"
					value={apiUrl}
					onChange={(e) => setApiUrl(e.target.value)}
					placeholder="http://localhost:8080"
					className="mt-1"
				/>
			</label>
			<label className="block">
				<span className="text-tiny font-medium text-ink-dull">Web URL</span>
				<Input
					type="text"
					value={webUrl}
					onChange={(e) => setWebUrl(e.target.value)}
					placeholder="http://localhost:8080"
					className="mt-1"
				/>
			</label>
			<label className="block">
				<span className="text-tiny font-medium text-ink-dull">
					Library ID
				</span>
				<Input
					type="text"
					value={libraryId}
					onChange={(e) => setLibraryId(e.target.value)}
					placeholder="(optional)"
					className="mt-1"
				/>
			</label>
			<Button
				onClick={() =>
					mutation.mutate({
						enabled: integration.enabled,
						api_url: apiUrl,
						web_url: webUrl,
						library_id: libraryId,
					})
				}
				loading={mutation.isPending}
				size="sm"
			>
				Save
			</Button>
			{message && (
				<div
					className={`rounded-md border px-3 py-2 text-sm ${
						message.type === "success"
							? "border-green-500/20 bg-green-500/10 text-green-400"
							: "border-red-500/20 bg-red-500/10 text-red-400"
					}`}
				>
					{message.text}
				</div>
			)}
		</div>
	);
}

function VoiceboxConfigForm({
	integration,
	onSaved,
}: {
	integration: IntegrationEntry;
	onSaved: () => void;
}) {
	const queryClient = useQueryClient();
	const [url, setUrl] = useState(
		(integration.config.url as string) ?? "",
	);
	const [profileId, setProfileId] = useState(
		(integration.config.profile_id as string) ?? "",
	);
	const [message, setMessage] = useState<{
		text: string;
		type: "success" | "error";
	} | null>(null);

	const mutation = useMutation({
		mutationFn: (config: Record<string, unknown>) =>
			api.updateIntegration("voicebox", config),
		onSuccess: (result) => {
			if (result.success) {
				setMessage({text: result.message, type: "success"});
				queryClient.invalidateQueries({queryKey: ["integrations"]});
				onSaved();
			} else {
				setMessage({text: result.message, type: "error"});
			}
		},
		onError: (error) =>
			setMessage({text: `Failed: ${error.message}`, type: "error"}),
	});

	return (
		<div className="flex flex-col gap-3 pt-3">
			<label className="block">
				<span className="text-tiny font-medium text-ink-dull">URL</span>
				<Input
					type="text"
					value={url}
					onChange={(e) => setUrl(e.target.value)}
					placeholder="http://localhost:17493"
					className="mt-1"
				/>
			</label>
			<label className="block">
				<span className="text-tiny font-medium text-ink-dull">
					Profile ID
				</span>
				<Input
					type="text"
					value={profileId}
					onChange={(e) => setProfileId(e.target.value)}
					placeholder="(optional)"
					className="mt-1"
				/>
			</label>
			<Button
				onClick={() =>
					mutation.mutate({
						enabled: integration.enabled,
						url,
						profile_id: profileId,
					})
				}
				loading={mutation.isPending}
				size="sm"
			>
				Save
			</Button>
			{message && (
				<div
					className={`rounded-md border px-3 py-2 text-sm ${
						message.type === "success"
							? "border-green-500/20 bg-green-500/10 text-green-400"
							: "border-red-500/20 bg-red-500/10 text-red-400"
					}`}
				>
					{message.text}
				</div>
			)}
		</div>
	);
}

function IntegrationCard({integration}: {integration: IntegrationEntry}) {
	const queryClient = useQueryClient();
	const [expanded, setExpanded] = useState(false);

	const toggleMutation = useMutation({
		mutationFn: () =>
			api.updateIntegration(integration.id, {
				enabled: !integration.enabled,
			}),
		onSuccess: () => {
			queryClient.invalidateQueries({queryKey: ["integrations"]});
			queryClient.invalidateQueries({queryKey: ["global-settings"]});
		},
	});

	return (
		<div className="rounded-lg border border-app-line bg-app-box">
			<div className="flex items-center justify-between p-4">
				<div className="flex items-center gap-3">
					<div
						className={`h-2.5 w-2.5 rounded-full ${integration.enabled ? "bg-green-500" : "bg-ink-faint/40"}`}
					/>
					<div>
						<div className="flex items-center gap-2">
							<span className="text-sm font-medium text-ink">
								{integration.name}
							</span>
							<Badge variant={STATUS_VARIANT[integration.status] ?? "secondary"} size="sm">
								{integration.status}
							</Badge>
						</div>
						<p className="mt-0.5 text-sm text-ink-dull">
							{integration.description}
						</p>
					</div>
				</div>
				<div className="flex items-center gap-3">
					<Switch
						size="sm"
						checked={integration.enabled}
						onCheckedChange={() => toggleMutation.mutate()}
						disabled={toggleMutation.isPending}
					/>
					<Button
						variant="outline"
						size="sm"
						onClick={() => setExpanded(!expanded)}
					>
						{expanded ? "Hide" : "Configure"}
					</Button>
				</div>
			</div>

			{expanded && (
				<div className="border-t border-app-line px-4 pb-4">
					{integration.id === "opencode" ? (
						<OpenCodeConfigForm
							integration={integration}
							onSaved={() => {}}
						/>
					) : integration.id === "spacedrive" ? (
						<SpacedriveConfigForm
							integration={integration}
							onSaved={() => {}}
						/>
					) : integration.id === "voicebox" ? (
						<VoiceboxConfigForm
							integration={integration}
							onSaved={() => {}}
						/>
					) : null}
				</div>
			)}
		</div>
	);
}

export function IntegrationsSection() {
	const {data, isLoading} = useQuery({
		queryKey: ["integrations"],
		queryFn: api.integrations,
		staleTime: 5_000,
	});

	return (
		<div className="mx-auto max-w-2xl px-6 py-6">
			<div className="mb-6">
				<h2 className="font-plex text-sm font-semibold text-ink">
					Integrations
				</h2>
				<p className="mt-1 text-sm text-ink-dull">
					Manage external service integrations. Enable, disable, and
					configure each integration below.
				</p>
			</div>

			{isLoading ? (
				<div className="flex items-center gap-2 text-ink-dull">
					<div className="h-2 w-2 animate-pulse rounded-full bg-accent" />
					Loading integrations...
				</div>
			) : (
				<div className="flex flex-col gap-3">
					{data?.integrations.map((integration) => (
						<IntegrationCard
							key={integration.id}
							integration={integration}
						/>
					))}
				</div>
			)}
		</div>
	);
}
