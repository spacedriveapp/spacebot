import {useRef, useState} from "react";
import {useQuery} from "@tanstack/react-query";
import {cx} from "class-variance-authority";
import {
	Button,
	Input,
	TextArea,
	DialogRoot,
	DialogContent,
	DialogTitle,
} from "@spacedrive/primitives";
import {api, type TopologyHuman} from "@/api/client";
import {Markdown} from "@/components/Markdown";

interface HumanEditDialogProps {
	human: TopologyHuman | null;
	open: boolean;
	onOpenChange: (open: boolean) => void;
	onUpdate: (fields: {
		displayName: string;
		role: string;
		bio: string;
		description: string;
		discordId: string;
		telegramId: string;
		slackId: string;
		email: string;
	}) => void;
	onDelete: () => void;
}

export function HumanEditDialog({
	human,
	open,
	onOpenChange,
	onUpdate,
	onDelete,
}: HumanEditDialogProps) {
	const [displayName, setDisplayName] = useState("");
	const [role, setRole] = useState("");
	const [bio, setBio] = useState("");
	const [description, setDescription] = useState("");
	const [discordId, setDiscordId] = useState("");
	const [telegramId, setTelegramId] = useState("");
	const [slackId, setSlackId] = useState("");
	const [email, setEmail] = useState("");
	const [descriptionMode, setDescriptionMode] = useState<"edit" | "preview">(
		"edit",
	);

	const {data: messagingStatus} = useQuery({
		queryKey: ["messagingStatus"],
		queryFn: api.messagingStatus,
	});

	// Sync state when a different human is selected
	const prevId = useRef<string | null>(null);
	if (human && human.id !== prevId.current) {
		prevId.current = human.id;
		setDisplayName(human.display_name ?? "");
		setRole(human.role ?? "");
		setBio(human.bio ?? "");
		setDescription(human.description ?? "");
		setDiscordId(human.discord_id ?? "");
		setTelegramId(human.telegram_id ?? "");
		setSlackId(human.slack_id ?? "");
		setEmail(human.email ?? "");
		setDescriptionMode("edit");
	}

	if (!human) return null;

	return (
		<DialogRoot open={open} onOpenChange={onOpenChange}>
			<DialogContent className="!max-w-5xl !w-[90vw] !h-[85vh] !flex !flex-col !gap-0 !p-0 overflow-hidden">
				<div className="flex items-center justify-between border-b border-app-line/50 px-5 py-4 shrink-0">
					<div className="flex items-baseline gap-2">
						<DialogTitle className="text-sm font-semibold">
							Edit Human
						</DialogTitle>
						<span className="text-tiny text-ink-faint">{human.id}</span>
					</div>
				</div>
				<div className="flex flex-1 min-h-0">
					{/* Left column — metadata fields */}
					<div className="w-72 shrink-0 border-r border-app-line/50 p-5 flex flex-col gap-4 overflow-y-auto">
						<div>
							<label className="mb-1.5 block text-sm font-medium text-ink-dull">
								Display Name
							</label>
							<Input
								size="lg"
								value={displayName}
								onChange={(e) => setDisplayName(e.target.value)}
								placeholder={human.id}
							/>
						</div>
						<div>
							<label className="mb-1.5 block text-sm font-medium text-ink-dull">
								Role
							</label>
							<Input
								size="lg"
								value={role}
								onChange={(e) => setRole(e.target.value)}
								placeholder="e.g. CEO, Lead Developer"
							/>
						</div>
						<div>
							<label className="mb-1.5 block text-sm font-medium text-ink-dull">
								Bio
							</label>
							<textarea
								value={bio}
								onChange={(e) => setBio(e.target.value)}
								placeholder="A short one-liner..."
								rows={2}
								className="w-full rounded-md bg-app-input px-3 py-2 text-sm text-ink outline-none border border-app-line/50 focus:border-accent/50 resize-none"
							/>
						</div>
						{/* Platform identity fields — only shown for configured platforms */}
						{messagingStatus &&
							(messagingStatus.discord.configured ||
								messagingStatus.telegram.configured ||
								messagingStatus.slack.configured ||
								messagingStatus.email.configured) && (
								<div className="border-t border-app-line/50 pt-3 flex flex-col gap-3">
									<label className="block text-xs font-medium text-ink-faint uppercase tracking-wide">
										Platform IDs
									</label>
									{messagingStatus.discord.configured && (
										<div>
											<label className="mb-1 block text-sm font-medium text-ink-dull">
												Discord
											</label>
											<Input
												size="lg"
												value={discordId}
												onChange={(e) => setDiscordId(e.target.value)}
												placeholder="Discord user ID"
											/>
										</div>
									)}
									{messagingStatus.telegram.configured && (
										<div>
											<label className="mb-1 block text-sm font-medium text-ink-dull">
												Telegram
											</label>
											<Input
												size="lg"
												value={telegramId}
												onChange={(e) => setTelegramId(e.target.value)}
												placeholder="Telegram user ID"
											/>
										</div>
									)}
									{messagingStatus.slack.configured && (
										<div>
											<label className="mb-1 block text-sm font-medium text-ink-dull">
												Slack
											</label>
											<Input
												size="lg"
												value={slackId}
												onChange={(e) => setSlackId(e.target.value)}
												placeholder="Slack member ID"
											/>
										</div>
									)}
									{messagingStatus.email.configured && (
										<div>
											<label className="mb-1 block text-sm font-medium text-ink-dull">
												Email
											</label>
											<Input
												size="lg"
												value={email}
												onChange={(e) => setEmail(e.target.value)}
												placeholder="email@example.com"
											/>
										</div>
									)}
								</div>
							)}
						<div className="flex-1" />
						<div className="flex items-center gap-2 pt-2 border-t border-app-line/50">
							<Button variant="accent" size="sm" onClick={onDelete}>
								Delete
							</Button>
							<div className="flex-1" />
							<Button
								variant="bare"
								size="sm"
								onClick={() => onOpenChange(false)}
							>
								Cancel
							</Button>
							<Button
								size="sm"
								onClick={() =>
									onUpdate({
										displayName,
										role,
										bio,
										description,
										discordId,
										telegramId,
										slackId,
										email,
									})
								}
							>
								Save
							</Button>
						</div>
					</div>
					{/* Right column — description markdown editor */}
					<div className="flex-1 flex flex-col min-w-0">
						<div className="flex items-center justify-between border-b border-app-line/50 bg-app-dark-box/20 px-5 py-2.5 shrink-0">
							<div className="flex items-center gap-3">
								<h3 className="text-sm font-medium text-ink">Description</h3>
								<span className="rounded bg-app-dark-box px-1.5 py-0.5 font-mono text-tiny text-ink-faint">
									Markdown
								</span>
							</div>
							<div className="flex items-center gap-3">
								<div className="flex items-center rounded border border-app-line/50 text-tiny">
									<button
										onClick={() => setDescriptionMode("edit")}
										className={cx(
											"px-2 py-0.5 rounded-l transition-colors",
											descriptionMode === "edit"
												? "bg-app-dark-box text-ink"
												: "text-ink-faint hover:text-ink",
										)}
									>
										Edit
									</button>
									<button
										onClick={() => setDescriptionMode("preview")}
										className={cx(
											"px-2 py-0.5 rounded-r transition-colors",
											descriptionMode === "preview"
												? "bg-app-dark-box text-ink"
												: "text-ink-faint hover:text-ink",
										)}
									>
										Preview
									</button>
								</div>
							</div>
						</div>
						<div className="flex-1 overflow-y-auto p-4">
							{descriptionMode === "edit" ? (
								<TextArea
									value={description}
									onChange={(e) => setDescription(e.target.value)}
									placeholder={
										"Write a full description of this person...\n\nBackground, preferences, communication style, timezone, working hours, areas of expertise — anything that helps agents interact with them effectively."
									}
									className="h-full w-full resize-none border-transparent bg-app-dark-box/30 px-4 py-3 font-mono leading-relaxed placeholder:text-ink-faint/40"
									spellCheck={false}
								/>
							) : (
								<div className="prose-sm px-4 py-3">
									{description.trim() ? (
										<Markdown>{description}</Markdown>
									) : (
										<span className="text-ink-faint/40 text-sm">
											Nothing to preview.
										</span>
									)}
								</div>
							)}
						</div>
					</div>
				</div>
			</DialogContent>
		</DialogRoot>
	);
}
