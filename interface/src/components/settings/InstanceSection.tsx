import {useState, useEffect} from "react";
import {useMutation, useQueryClient} from "@tanstack/react-query";
import {api} from "@/api/client";
import {Button, Input} from "@spacedrive/primitives";
import type {GlobalSettingsSectionProps} from "./types";

export function InstanceSection({settings, isLoading}: GlobalSettingsSectionProps) {
	const queryClient = useQueryClient();
	const [companyName, setCompanyName] = useState(
		settings?.company_name ?? "My Company",
	);
	const [message, setMessage] = useState<{
		text: string;
		type: "success" | "error";
	} | null>(null);

	useEffect(() => {
		if (settings) {
			setCompanyName(settings.company_name ?? "My Company");
		}
	}, [settings]);

	const updateMutation = useMutation({
		mutationFn: api.updateGlobalSettings,
		onSuccess: (result) => {
			if (result.success) {
				setMessage({text: result.message, type: "success"});
				queryClient.invalidateQueries({queryKey: ["global-settings"]});
			} else {
				setMessage({text: result.message, type: "error"});
			}
		},
		onError: (error) => {
			setMessage({text: `Failed: ${error.message}`, type: "error"});
		},
	});

	const handleSave = () => {
		updateMutation.mutate({company_name: companyName.trim() || "My Company"});
	};

	return (
		<div className="mx-auto max-w-2xl px-6 py-6">
			<div className="mb-6">
				<h2 className="font-plex text-sm font-semibold text-ink">
					Instance Settings
				</h2>
				<p className="mt-1 text-sm text-ink-dull">
					Configure your Spacebot instance identity.
				</p>
			</div>

			{isLoading ? (
				<div className="flex items-center gap-2 text-ink-dull">
					<div className="h-2 w-2 animate-pulse rounded-full bg-accent" />
					Loading settings...
				</div>
			) : (
				<div className="flex flex-col gap-4">
					<div className="rounded-lg border border-app-line bg-app-box p-4">
						<label className="block">
							<span className="text-sm font-medium text-ink">
								Company Name
							</span>
							<p className="mt-0.5 text-sm text-ink-dull">
								Displayed in the sidebar and top bar as your organization name.
							</p>
							<Input
								type="text"
								value={companyName}
								onChange={(e) => setCompanyName(e.target.value)}
								placeholder="My Company"
								className="mt-2"
								onKeyDown={(e) => {
									if (e.key === "Enter") handleSave();
								}}
							/>
						</label>
					</div>

					<Button onClick={handleSave} loading={updateMutation.isPending}>
						Save Changes
					</Button>
				</div>
			)}

			{message && (
				<div
					className={`mt-4 rounded-md border px-3 py-2 text-sm ${
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
