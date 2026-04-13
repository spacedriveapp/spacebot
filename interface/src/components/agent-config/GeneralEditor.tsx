import {useCallback, useEffect, useState, useRef} from "react";
import {useQuery, useMutation, useQueryClient} from "@tanstack/react-query";
import {cx} from "class-variance-authority";
import {Button, Input} from "@spacedrive/primitives";
import {api} from "@/api/client";
import {ProfileAvatar, seedGradient} from "@/components/ProfileAvatar";
import {GRADIENT_PRESETS} from "./constants";
import type {GeneralEditorProps} from "./types";

export function GeneralEditor({
	agentId,
	displayName,
	role,
	gradientStart,
	gradientEnd,
	detail,
	onDirtyChange,
	saveHandlerRef,
	onSave,
}: GeneralEditorProps) {
	const queryClient = useQueryClient();
	const [localDisplayName, setLocalDisplayName] = useState(displayName);
	const [localRole, setLocalRole] = useState(role);
	const [localGradientStart, setLocalGradientStart] = useState(gradientStart);
	const [localGradientEnd, setLocalGradientEnd] = useState(gradientEnd);
	const [localDirty, setLocalDirty] = useState(false);
	const [avatarPreview, setAvatarPreview] = useState<string | null>(null);
	const fileInputRef = useRef<HTMLInputElement>(null);

	const avatarUrl = api.agentAvatarUrl(agentId);
	const {data: avatarExists} = useQuery({
		queryKey: ["agentAvatar", agentId],
		queryFn: async () => {
			try {
				const response = await fetch(avatarUrl, {method: "HEAD"});
				return response.ok;
			} catch {
				return false;
			}
		},
	});

	const uploadAvatarMutation = useMutation({
		mutationFn: (file: File) => api.uploadAvatar(agentId, file),
		onSuccess: () => {
			queryClient.invalidateQueries({queryKey: ["agentAvatar", agentId]});
			queryClient.invalidateQueries({queryKey: ["agents"]});
			queryClient.invalidateQueries({queryKey: ["topology"]});
			setAvatarPreview(null);
		},
	});

	const deleteAvatarMutation = useMutation({
		mutationFn: () => api.deleteAvatar(agentId),
		onSuccess: () => {
			queryClient.invalidateQueries({queryKey: ["agentAvatar", agentId]});
			queryClient.invalidateQueries({queryKey: ["agents"]});
			queryClient.invalidateQueries({queryKey: ["topology"]});
			setAvatarPreview(null);
		},
	});

	useEffect(() => {
		if (!localDirty) {
			setLocalDisplayName(displayName);
			setLocalRole(role);
			setLocalGradientStart(gradientStart);
			setLocalGradientEnd(gradientEnd);
		}
	}, [displayName, role, gradientStart, gradientEnd, localDirty]);

	useEffect(() => {
		onDirtyChange(localDirty);
	}, [localDirty, onDirtyChange]);

	const handleSave = useCallback(() => {
		onSave({
			display_name: localDisplayName,
			role: localRole,
			gradient_start: localGradientStart || undefined,
			gradient_end: localGradientEnd || undefined,
		});
		setLocalDirty(false);
	}, [onSave, localDisplayName, localRole, localGradientStart, localGradientEnd]);

	const handleRevert = useCallback(() => {
		setLocalDisplayName(displayName);
		setLocalRole(role);
		setLocalGradientStart(gradientStart);
		setLocalGradientEnd(gradientEnd);
		setLocalDirty(false);
	}, [displayName, role, gradientStart, gradientEnd]);

	useEffect(() => {
		saveHandlerRef.current.save = handleSave;
		saveHandlerRef.current.revert = handleRevert;
		return () => {
			saveHandlerRef.current.save = undefined;
			saveHandlerRef.current.revert = undefined;
		};
	}, [handleSave, handleRevert]);

	const handleFileChange = (event: React.ChangeEvent<HTMLInputElement>) => {
		const file = event.target.files?.[0];
		if (!file) return;
		const reader = new FileReader();
		reader.onload = () => setAvatarPreview(reader.result as string);
		reader.readAsDataURL(file);
		uploadAvatarMutation.mutate(file);
	};

	const [seedC1, seedC2] = seedGradient(agentId);
	const previewC1 = localGradientStart || seedC1;
	const previewC2 = localGradientEnd || seedC2;

	return (
		<>
			<div className="flex items-center justify-between border-b border-app-line/50 bg-app-dark-box/20 px-5 py-2.5">
				<div className="flex items-center gap-3">
					<h3 className="text-sm font-medium text-ink">General</h3>
					<span className="text-tiny text-ink-faint">Agent metadata</span>
				</div>
				{localDirty ? (
					<span className="text-tiny text-amber-400">Unsaved changes</span>
				) : (
					<span className="text-tiny text-ink-faint/50">
						Changes saved to config.toml
					</span>
				)}
			</div>
			<div className="flex-1 overflow-y-auto px-8 py-8">
				<div className="mb-6 rounded-lg border border-app-line/30 bg-app-dark-box/20 px-5 py-4">
					<p className="text-sm leading-relaxed text-ink-dull">{detail}</p>
				</div>
				<div className="grid gap-4">
					<div className="flex flex-col gap-1.5">
						<label className="text-sm font-medium text-ink">Agent ID</label>
						<p className="text-tiny text-ink-faint">
							The config key identifier. This cannot be changed.
						</p>
						<Input
							value={agentId}
							disabled
							className="border-app-line/50 bg-app-dark-box/30 text-ink-faint"
						/>
					</div>
					<div className="flex flex-col gap-1.5">
						<label className="text-sm font-medium text-ink">Display Name</label>
						<p className="text-tiny text-ink-faint">
							Human-friendly name shown in the UI and messaging platforms.
						</p>
						<Input
							value={localDisplayName}
							onChange={(e) => {
								setLocalDisplayName(e.target.value);
								setLocalDirty(true);
							}}
							placeholder={agentId}
						/>
					</div>
					<div className="flex flex-col gap-1.5">
						<label className="text-sm font-medium text-ink">Role</label>
						<p className="text-tiny text-ink-faint">
							Describes the agent's purpose. Shown in the topology view and
							agent listings.
						</p>
						<Input
							value={localRole}
							onChange={(e) => {
								setLocalRole(e.target.value);
								setLocalDirty(true);
							}}
							placeholder="e.g. Research Assistant, Code Reviewer"
						/>
					</div>

					{/* Avatar */}
					<div className="flex flex-col gap-1.5">
						<label className="text-sm font-medium text-ink">Avatar</label>
						<p className="text-tiny text-ink-faint">
							Upload a custom image, or use the generated gradient avatar.
						</p>
						<div className="flex items-center gap-4">
							<ProfileAvatar
								seed={agentId}
								name={localDisplayName || agentId}
								size={64}
								className="rounded-full shrink-0"
								gradientStart={localGradientStart || undefined}
								gradientEnd={localGradientEnd || undefined}
								avatarUrl={
									avatarPreview ??
									(avatarExists ? `${avatarUrl}&t=${Date.now()}` : undefined)
								}
							/>
							<div className="flex flex-col gap-2">
								<div className="flex items-center gap-2">
									<Button
										variant="outline"
										size="sm"
										onClick={() => fileInputRef.current?.click()}
									>
										{avatarExists ? "Change Image" : "Upload Image"}
									</Button>
									{avatarExists && (
										<Button
											variant="bare"
											size="sm"
											onClick={() => deleteAvatarMutation.mutate()}
											className="text-ink-faint hover:text-red-400"
										>
											Remove
										</Button>
									)}
								</div>
								<p className="text-tiny text-ink-faint/60">
									PNG, JPEG, WebP, GIF, or SVG. Max 5 MB.
								</p>
							</div>
							<input
								ref={fileInputRef}
								type="file"
								accept="image/png,image/jpeg,image/gif,image/webp,image/svg+xml"
								onChange={handleFileChange}
								className="hidden"
							/>
						</div>
					</div>

					{/* Gradient Color */}
					<div className="flex flex-col gap-1.5">
						<label className="text-sm font-medium text-ink">Gradient</label>
						<p className="text-tiny text-ink-faint">
							Choose a gradient for the avatar and banner. Only used when no
							image is uploaded.
						</p>
						<div className="flex flex-wrap gap-2">
							<button
								onClick={() => {
									setLocalGradientStart("");
									setLocalGradientEnd("");
									setLocalDirty(true);
								}}
								className={cx(
									"flex items-center gap-2 rounded-md border px-3 py-1.5 text-sm transition-colors",
									!localGradientStart
										? "border-accent bg-accent/10 text-ink"
										: "border-app-line/50 text-ink-faint hover:border-app-line",
								)}
							>
								<span
									className="h-5 w-5 rounded-full shrink-0"
									style={{
										background: `linear-gradient(135deg, ${seedC1}, ${seedC2})`,
									}}
								/>
								Auto
							</button>
							{GRADIENT_PRESETS.map((preset) => (
								<button
									key={preset.label}
									onClick={() => {
										setLocalGradientStart(preset.start);
										setLocalGradientEnd(preset.end);
										setLocalDirty(true);
									}}
									className={cx(
										"flex items-center gap-2 rounded-md border px-3 py-1.5 text-sm transition-colors",
										localGradientStart === preset.start &&
											localGradientEnd === preset.end
											? "border-accent bg-accent/10 text-ink"
											: "border-app-line/50 text-ink-faint hover:border-app-line",
									)}
								>
									<span
										className="h-5 w-5 rounded-full shrink-0"
										style={{
											background: `linear-gradient(135deg, ${preset.start}, ${preset.end})`,
										}}
									/>
									{preset.label}
								</button>
							))}
						</div>
						<div className="mt-2 flex items-center gap-3">
							<span className="text-tiny text-ink-faint">Preview:</span>
							<span
								className="h-6 w-20 rounded"
								style={{
									background: `linear-gradient(135deg, ${previewC1}, ${previewC2})`,
								}}
							/>
						</div>
					</div>
				</div>
			</div>
		</>
	);
}
