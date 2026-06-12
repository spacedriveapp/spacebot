import {useState} from "react";
import {useQuery, useMutation, useQueryClient} from "@tanstack/react-query";
import {
	api,
	type SecretCategory,
	type SecretListItem,
	type StoreState,
} from "@/api/client";
import {
	Badge,
	Button,
	Input,
	DialogRoot,
	DialogContent,
	DialogHeader,
	DialogTitle,
	DialogDescription,
	DialogFooter,
	SelectRoot,
	SelectTrigger,
	SelectValue,
	SelectContent,
	SelectItem,
} from "@spacedrive/primitives";

export function SecretsSection() {
	const queryClient = useQueryClient();

	// Store status.
	const {data: storeStatus, isLoading: statusLoading} = useQuery({
		queryKey: ["secrets-status"],
		queryFn: () => api.secretsStatus(),
		staleTime: 5_000,
	});

	// Secret list.
	const {data: secretsData, isLoading: secretsLoading} = useQuery({
		queryKey: ["secrets"],
		queryFn: () => api.listSecrets(),
		staleTime: 5_000,
	});

	const secrets = secretsData?.secrets ?? [];
	const isLoading = statusLoading || secretsLoading;
	const state: StoreState = storeStatus?.state ?? "unencrypted";
	const isLocked = state === "locked";
	const canMutate = !isLocked;

	// ── UI state ─────────────────────────────────────────────────────────
	const [addDialogOpen, setAddDialogOpen] = useState(false);
	const [editingSecret, setEditingSecret] = useState<string | null>(null);
	const [deleteTarget, setDeleteTarget] = useState<string | null>(null);
	const [nameInput, setNameInput] = useState("");
	const [valueInput, setValueInput] = useState("");
	const [categoryInput, setCategoryInput] = useState<SecretCategory>("tool");
	const [message, setMessage] = useState<{
		text: string;
		type: "success" | "error";
	} | null>(null);

	// Encryption flow state.
	const [encryptDialogOpen, setEncryptDialogOpen] = useState(false);
	const [masterKeyDisplay, setMasterKeyDisplay] = useState<string | null>(null);
	const [masterKeyCopied, setMasterKeyCopied] = useState(false);
	const [unlockKeyInput, setUnlockKeyInput] = useState("");
	const [rotateDialogOpen, setRotateDialogOpen] = useState(false);

	const [filterCategory, setFilterCategory] = useState<"all" | SecretCategory>(
		"all",
	);
	const [searchQuery, setSearchQuery] = useState("");

	// ── Mutations ────────────────────────────────────────────────────────
	const invalidateSecrets = () => {
		queryClient.invalidateQueries({queryKey: ["secrets"]});
		queryClient.invalidateQueries({queryKey: ["secrets-status"]});
	};

	const putMutation = useMutation({
		mutationFn: ({
			name,
			value,
			category,
		}: {
			name: string;
			value: string;
			category?: SecretCategory;
		}) => api.putSecret(name, value, category),
		onSuccess: (result) => {
			invalidateSecrets();
			setAddDialogOpen(false);
			setEditingSecret(null);
			setNameInput("");
			setValueInput("");
			setMessage({
				text: result.reload_required
					? `${result.name} saved (${result.category}). Restart required for system secrets to take effect.`
					: `${result.name} saved (${result.category}).`,
				type: "success",
			});
		},
		onError: (error) => {
			setMessage({text: `Failed: ${error.message}`, type: "error"});
		},
	});

	const deleteMutation = useMutation({
		mutationFn: (name: string) => api.deleteSecret(name),
		onSuccess: (result) => {
			invalidateSecrets();
			setDeleteTarget(null);
			setMessage({
				text: result.warning
					? `Deleted ${result.deleted}. ${result.warning}`
					: `Deleted ${result.deleted}.`,
				type: "success",
			});
		},
		onError: (error) => {
			setMessage({text: `Failed: ${error.message}`, type: "error"});
		},
	});

	const encryptMutation = useMutation({
		mutationFn: () => api.enableEncryption(),
		onSuccess: (result) => {
			invalidateSecrets();
			setMasterKeyDisplay(result.master_key);
			setMasterKeyCopied(false);
		},
		onError: (error) => {
			setMessage({text: `Failed: ${error.message}`, type: "error"});
			setEncryptDialogOpen(false);
		},
	});

	const unlockMutation = useMutation({
		mutationFn: (key: string) => api.unlockSecrets(key),
		onSuccess: () => {
			invalidateSecrets();
			setUnlockKeyInput("");
			setMessage({text: "Secret store unlocked.", type: "success"});
		},
		onError: (error) => {
			setMessage({text: `Unlock failed: ${error.message}`, type: "error"});
		},
	});

	const lockMutation = useMutation({
		mutationFn: () => api.lockSecrets(),
		onSuccess: () => {
			invalidateSecrets();
			setMessage({text: "Secret store locked.", type: "success"});
		},
		onError: (error) => {
			setMessage({text: `Failed: ${error.message}`, type: "error"});
		},
	});

	const rotateMutation = useMutation({
		mutationFn: () => api.rotateKey(),
		onSuccess: (result) => {
			invalidateSecrets();
			setRotateDialogOpen(false);
			setMasterKeyDisplay(result.master_key);
			setMasterKeyCopied(false);
			setEncryptDialogOpen(true);
		},
		onError: (error) => {
			setMessage({text: `Failed: ${error.message}`, type: "error"});
			setRotateDialogOpen(false);
		},
	});

	const migrateMutation = useMutation({
		mutationFn: () => api.migrateSecrets(),
		onSuccess: (result) => {
			invalidateSecrets();
			setMessage({
				text:
					result.migrated.length > 0
						? `Migrated ${result.migrated.length} secrets from config.toml.`
						: result.message,
				type: result.migrated.length > 0 ? "success" : "success",
			});
		},
		onError: (error) => {
			setMessage({text: `Migration failed: ${error.message}`, type: "error"});
		},
	});

	// ── Handlers ─────────────────────────────────────────────────────────
	const handleOpenAdd = () => {
		setNameInput("");
		setValueInput("");
		setCategoryInput("tool");
		setMessage(null);
		setAddDialogOpen(true);
	};

	const handleOpenEdit = (secret: SecretListItem) => {
		setEditingSecret(secret.name);
		setNameInput(secret.name);
		setValueInput("");
		setCategoryInput(secret.category);
		setMessage(null);
	};

	const handleSave = () => {
		const name = editingSecret ?? nameInput.trim().toUpperCase();
		if (!name || !valueInput) return;
		putMutation.mutate({name, value: valueInput, category: categoryInput});
	};

	const handleCopyKey = async () => {
		if (!masterKeyDisplay) return;
		try {
			await navigator.clipboard.writeText(masterKeyDisplay);
			setMasterKeyCopied(true);
		} catch {
			// Fallback
			const textarea = document.createElement("textarea");
			textarea.value = masterKeyDisplay;
			textarea.setAttribute("readonly", "");
			textarea.style.position = "absolute";
			textarea.style.left = "-9999px";
			document.body.appendChild(textarea);
			textarea.select();
			document.execCommand("copy");
			document.body.removeChild(textarea);
			setMasterKeyCopied(true);
		}
	};

	const filteredSecrets = secrets.filter((secret) => {
		if (filterCategory !== "all" && secret.category !== filterCategory)
			return false;
		if (
			searchQuery &&
			!secret.name.toLowerCase().includes(searchQuery.toLowerCase())
		)
			return false;
		return true;
	});

	return (
		<div className="mx-auto max-w-2xl px-6 py-6">
			{/* Header */}
			<div className="mb-6">
				<h2 className="font-plex text-sm font-semibold text-ink">Secrets</h2>
				<p className="mt-1 text-sm text-ink-dull">
					Manage credentials for LLM providers (system) and CLI tools used by
					workers (tool). System secrets are never exposed to worker
					subprocesses. Tool secrets are injected as environment variables.
				</p>
			</div>

			{/* Status bar */}
			{storeStatus && (
				<div className="mb-4 flex items-center gap-3 rounded-md border border-app-line bg-app-dark-box/20 px-4 py-3">
					<div className="flex items-center gap-2">
						<div
							className={`h-2 w-2 rounded-full ${
								state === "unlocked"
									? "bg-green-500"
									: state === "locked"
										? "bg-red-500"
										: "bg-amber-500"
							}`}
						/>
						<span className="text-sm font-medium text-ink">
							{state === "unlocked"
								? "Encrypted & Unlocked"
								: state === "locked"
									? "Encrypted & Locked"
									: "Unencrypted"}
						</span>
					</div>
					<div className="flex-1" />
					<span className="text-tiny text-ink-faint">
						{storeStatus.secret_count} secrets ({storeStatus.system_count}{" "}
						system, {storeStatus.tool_count} tool)
					</span>
				</div>
			)}

			{/* Encryption banner (unencrypted stores) */}
			{state === "unencrypted" && !storeStatus?.platform_managed && (
				<div className="mb-4 rounded-md border border-amber-500/20 bg-amber-500/5 px-4 py-3">
					<div className="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
						<div className="sm:pr-4">
							<p className="text-sm font-medium text-amber-400">
								Encryption not enabled
							</p>
							<p className="mt-0.5 text-sm text-ink-faint">
								Secrets are stored without encryption. Enable encryption for
								protection against volume compromise.
							</p>
						</div>
						<Button
							onClick={() => {
								setEncryptDialogOpen(true);
								setMasterKeyDisplay(null);
							}}
							variant="outline"
							className="w-full shrink-0 whitespace-nowrap sm:w-auto"
						>
							Enable Encryption
						</Button>
					</div>
				</div>
			)}

			{/* Unlock prompt (locked stores) */}
			{isLocked && (
				<div className="mb-4 rounded-md border border-red-500/20 bg-red-500/5 px-4 py-3">
					<p className="text-sm font-medium text-red-400">Secrets are locked</p>
					<p className="mt-0.5 text-sm text-ink-faint">
						Enter your master key to unlock encrypted secrets. You can view
						secret names but cannot add, edit, or read values while locked.
					</p>
					<div className="mt-3 flex items-center gap-2">
						<Input
							type="password"
							value={unlockKeyInput}
							onChange={(e) => setUnlockKeyInput(e.target.value)}
							placeholder="Paste master key (hex)"
							className="max-w-sm font-mono text-tiny"
							onKeyDown={(e) => {
								if (e.key === "Enter" && unlockKeyInput.trim())
									unlockMutation.mutate(unlockKeyInput.trim());
							}}
						/>
						<Button
							onClick={() => unlockMutation.mutate(unlockKeyInput.trim())}
							disabled={!unlockKeyInput.trim()}
							loading={unlockMutation.isPending}
							size="md"
						>
							Unlock
						</Button>
					</div>
				</div>
			)}

			{/* Feedback message */}
			{message && (
				<div
					className={`mb-4 rounded-md border px-3 py-2 text-sm ${
						message.type === "success"
							? "border-green-500/20 bg-green-500/10 text-green-400"
							: "border-red-500/20 bg-red-500/10 text-red-400"
					}`}
				>
					{message.text}
				</div>
			)}

			{/* Toolbar */}
			<div className="mb-3 flex items-center gap-2">
				<Input
					value={searchQuery}
					onChange={(e) => setSearchQuery(e.target.value)}
					placeholder="Filter secrets..."
					className="max-w-xs"
				/>
				<div className="flex gap-1">
					{(["all", "system", "tool"] as const).map((cat) => (
						<button
							key={cat}
							onClick={() => setFilterCategory(cat)}
							className={`rounded-full px-2.5 py-1 text-tiny font-medium transition-colors ${
								filterCategory === cat
									? "bg-accent/15 text-accent"
									: "text-ink-faint hover:text-ink-dull"
							}`}
						>
							{cat === "all"
								? "All"
								: cat.charAt(0).toUpperCase() + cat.slice(1)}
						</button>
					))}
				</div>
				<div className="flex-1" />
				{canMutate && (
					<Button onClick={handleOpenAdd} size="md">
						Add secret
					</Button>
				)}
			</div>

			{/* Secret list */}
			{isLoading ? (
				<div className="flex items-center gap-2 text-ink-dull">
					<div className="h-2 w-2 animate-pulse rounded-full bg-accent" />
					Loading secrets...
				</div>
			) : filteredSecrets.length === 0 ? (
				<div className="flex flex-col items-center rounded-lg border border-dashed border-app-line py-12">
					<p className="text-sm font-medium text-ink-dull">
						{secrets.length === 0 ? "No secrets yet" : "No matching secrets"}
					</p>
					<p className="mt-1 text-sm text-ink-faint">
						{secrets.length === 0
							? "Add credentials for LLM providers or CLI tools."
							: "Try a different filter."}
					</p>
					{secrets.length === 0 && canMutate && (
						<div className="mt-4 flex gap-2">
							<Button onClick={handleOpenAdd} size="md">
								Add secret
							</Button>
							<Button
								onClick={() => migrateMutation.mutate()}
								variant="outline"
								size="md"
								loading={migrateMutation.isPending}
							>
								Migrate from config
							</Button>
						</div>
					)}
				</div>
			) : (
				<div className="flex flex-col gap-1.5">
					{filteredSecrets.map((secret) => (
						<div
							key={secret.name}
							className="group flex items-center rounded-lg border border-app-line bg-app-box px-4 py-3"
						>
							<div className="flex-1 min-w-0">
								<div className="flex items-center gap-2">
									<code className="text-sm font-medium text-ink">
										{secret.name}
									</code>
									<Badge
										variant="outline"
										size="md"
										className="pointer-events-none transition-none"
									>
										{secret.category}
									</Badge>
								</div>
								<p className="mt-0.5 text-tiny text-ink-faint">
									Updated {new Date(secret.updated_at).toLocaleDateString()}
								</p>
							</div>
							{canMutate && (
								<div className="flex gap-1.5 opacity-0 pointer-events-none group-hover:opacity-100 group-hover:pointer-events-auto group-focus-within:opacity-100 group-focus-within:pointer-events-auto">
									<Button
										onClick={() => handleOpenEdit(secret)}
										variant="outline"
										size="md"
									>
										Update
									</Button>
									<Button
										onClick={() => setDeleteTarget(secret.name)}
										variant="outline"
										size="md"
									>
										Delete
									</Button>
								</div>
							)}
						</div>
					))}
				</div>
			)}

			{/* Bottom actions for encrypted stores */}
			{storeStatus?.encrypted && !storeStatus.platform_managed && canMutate && (
				<div className="mt-6 flex items-center gap-2 border-t border-app-line pt-4">
					<Button
						onClick={() => lockMutation.mutate()}
						variant="outline"
						size="md"
						loading={lockMutation.isPending}
					>
						Lock store
					</Button>
					<Button
						onClick={() => setRotateDialogOpen(true)}
						variant="outline"
						size="md"
					>
						Rotate master key
					</Button>
					<div className="flex-1" />
					<Button
						onClick={() => migrateMutation.mutate()}
						variant="outline"
						size="md"
						loading={migrateMutation.isPending}
					>
						Migrate from config
					</Button>
				</div>
			)}

			{/* Migrate button for unencrypted stores */}
			{state === "unencrypted" && secrets.length > 0 && (
				<div className="mt-4 flex justify-end">
					<Button
						onClick={() => migrateMutation.mutate()}
						variant="outline"
						size="md"
						loading={migrateMutation.isPending}
					>
						Migrate from config
					</Button>
				</div>
			)}

			{/* ── Add / Edit Dialog ─────────────────────────────────────── */}
			<DialogRoot
				open={addDialogOpen || !!editingSecret}
				onOpenChange={(open) => {
					if (!open) {
						setAddDialogOpen(false);
						setEditingSecret(null);
						setNameInput("");
						setValueInput("");
					}
				}}
			>
				<DialogContent className="max-w-md">
					<DialogHeader>
						<DialogTitle>
							{editingSecret ? "Update Secret" : "Add Secret"}
						</DialogTitle>
						<DialogDescription>
							{editingSecret
								? `Enter a new value for ${editingSecret}. The existing value will be overwritten.`
								: "Add a new credential. System secrets are internal (LLM keys, messaging tokens). Tool secrets are exposed to worker subprocesses as environment variables."}
						</DialogDescription>
					</DialogHeader>

					{!editingSecret && (
						<div className="space-y-1.5">
							<label className="text-sm font-medium text-ink">Name</label>
							<Input
								value={nameInput}
								onChange={(e) =>
									setNameInput(
										e.target.value.toUpperCase().replace(/[^A-Z0-9_]/g, ""),
									)
								}
								placeholder="GH_TOKEN"
								className="font-mono"
								autoFocus
							/>
							<p className="text-tiny text-ink-faint">
								UPPER_SNAKE_CASE. This is also the env var name for tool
								secrets.
							</p>
						</div>
					)}

					<div className="space-y-1.5">
						<label className="text-sm font-medium text-ink">Value</label>
						<Input
							type="password"
							value={valueInput}
							onChange={(e) => setValueInput(e.target.value)}
							placeholder={editingSecret ? "Enter new value" : "Secret value"}
							autoFocus={!!editingSecret}
							onKeyDown={(e) => {
								if (e.key === "Enter") handleSave();
							}}
						/>
					</div>

					<div className="space-y-1.5">
						<label className="text-sm font-medium text-ink">Category</label>
						<SelectRoot
							value={categoryInput}
							onValueChange={(value) =>
								setCategoryInput(value as SecretCategory)
							}
						>
							<SelectTrigger>
								<SelectValue />
							</SelectTrigger>
							<SelectContent>
								<SelectItem value="tool">
									Tool — exposed to workers as env vars
								</SelectItem>
								<SelectItem value="system">
									System — internal only, never exposed
								</SelectItem>
							</SelectContent>
						</SelectRoot>
						<p className="text-tiny text-ink-faint">
							{categoryInput === "tool"
								? "Workers will have access to this credential via environment variable."
								: "Only the Spacebot process can read this credential. Workers never see it."}
						</p>
					</div>

					<DialogFooter>
						<Button
							onClick={() => {
								setAddDialogOpen(false);
								setEditingSecret(null);
							}}
							variant="outline"
							size="md"
						>
							Cancel
						</Button>
						<Button
							onClick={handleSave}
							disabled={(!editingSecret && !nameInput.trim()) || !valueInput}
							loading={putMutation.isPending}
							size="md"
						>
							Save
						</Button>
					</DialogFooter>
				</DialogContent>
			</DialogRoot>

			{/* ── Delete Confirmation ───────────────────────────────────── */}
			<DialogRoot
				open={!!deleteTarget}
				onOpenChange={(open) => {
					if (!open) setDeleteTarget(null);
				}}
			>
				<DialogContent className="max-w-sm">
					<DialogHeader>
						<DialogTitle>Delete Secret</DialogTitle>
						<DialogDescription>
							Are you sure you want to delete{" "}
							<code className="font-mono text-ink">{deleteTarget}</code>? If
							this secret is referenced in config.toml, the reference will fail
							to resolve.
						</DialogDescription>
					</DialogHeader>
					<DialogFooter>
						<Button
							onClick={() => setDeleteTarget(null)}
							variant="outline"
							size="md"
						>
							Cancel
						</Button>
						<Button
							onClick={() => {
								if (deleteTarget) deleteMutation.mutate(deleteTarget);
							}}
							loading={deleteMutation.isPending}
							variant="accent"
							size="md"
						>
							Delete
						</Button>
					</DialogFooter>
				</DialogContent>
			</DialogRoot>

			{/* ── Enable Encryption / Master Key Display Dialog ─────────── */}
			<DialogRoot
				open={encryptDialogOpen}
				onOpenChange={(open) => {
					if (!open) {
						setEncryptDialogOpen(false);
						setMasterKeyDisplay(null);
						setMasterKeyCopied(false);
					}
				}}
			>
				<DialogContent className="max-w-md">
					{!masterKeyDisplay ? (
						<>
							<DialogHeader>
								<DialogTitle>Enable Encryption</DialogTitle>
								<DialogDescription>
									This will generate a master key and encrypt all secrets at
									rest using AES-256-GCM. On Linux, you will need the master key
									to unlock secrets after a reboot.
								</DialogDescription>
							</DialogHeader>
							<DialogFooter>
								<Button
									onClick={() => setEncryptDialogOpen(false)}
									variant="outline"
									size="md"
								>
									Cancel
								</Button>
								<Button
									onClick={() => encryptMutation.mutate()}
									loading={encryptMutation.isPending}
									size="md"
								>
									Enable encryption
								</Button>
							</DialogFooter>
						</>
					) : (
						<>
							<DialogHeader>
								<DialogTitle>Master Key Generated</DialogTitle>
								<DialogDescription>
									Save this key somewhere safe. On Linux, you will need it to
									unlock the secret store after a reboot. This is the only time
									the key will be shown.
								</DialogDescription>
							</DialogHeader>
							<div className="space-y-3">
								<div className="flex items-center gap-2">
									<code className="flex-1 rounded border border-app-line bg-app-darkerBox px-3 py-2 font-mono text-tiny text-ink break-all select-all">
										{masterKeyDisplay}
									</code>
									<Button
										onClick={handleCopyKey}
										size="md"
										variant={masterKeyCopied ? "gray" : "outline"}
									>
										{masterKeyCopied ? "Copied" : "Copy"}
									</Button>
								</div>
								<div className="rounded-md border border-amber-500/20 bg-amber-500/5 px-3 py-2 text-sm text-amber-400">
									If you lose this key and the OS credential store is cleared
									(e.g. after a Linux reboot), you will not be able to access
									your encrypted secrets.
								</div>
							</div>
							<DialogFooter>
								<Button
									onClick={() => {
										setEncryptDialogOpen(false);
										setMasterKeyDisplay(null);
										setMasterKeyCopied(false);
									}}
									size="md"
								>
									Done
								</Button>
							</DialogFooter>
						</>
					)}
				</DialogContent>
			</DialogRoot>

			{/* ── Rotate Key Confirmation ───────────────────────────────── */}
			<DialogRoot
				open={rotateDialogOpen}
				onOpenChange={(open) => {
					if (!open) setRotateDialogOpen(false);
				}}
			>
				<DialogContent className="max-w-sm">
					<DialogHeader>
						<DialogTitle>Rotate Master Key</DialogTitle>
						<DialogDescription>
							This will generate a new master key and re-encrypt all secrets.
							Your current master key will be invalidated. You will need to save
							the new key.
						</DialogDescription>
					</DialogHeader>
					<DialogFooter>
						<Button
							onClick={() => setRotateDialogOpen(false)}
							variant="outline"
							size="md"
						>
							Cancel
						</Button>
						<Button
							onClick={() => rotateMutation.mutate()}
							loading={rotateMutation.isPending}
							size="md"
						>
							Rotate key
						</Button>
					</DialogFooter>
				</DialogContent>
			</DialogRoot>
		</div>
	);
}
