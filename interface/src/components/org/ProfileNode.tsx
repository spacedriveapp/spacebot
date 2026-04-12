import {Handle, Position, type NodeProps} from "@xyflow/react";
import {Link} from "@tanstack/react-router";
import {seedGradient, ProfileAvatar} from "@/components/ProfileAvatar";
import {NODE_WIDTH} from "./constants";

export function ProfileNode({data, selected}: NodeProps) {
	const nodeId = (data.nodeId as string) ?? "";
	const avatarSeed = (data.avatarSeed as string) ?? nodeId;
	const [defaultC1, defaultC2] = seedGradient(avatarSeed);
	const configDisplayName = data.configDisplayName as string | null;
	const configRole = data.configRole as string | null;
	const chosenName = data.chosenName as string | null;
	const bio = data.bio as string | null;
	const gradientStart = data.gradientStart as string | null;
	const gradientEnd = data.gradientEnd as string | null;
	const nodeAvatarUrl = data.avatarUrl as string | null;
	const c1 = gradientStart || defaultC1;
	const c2 = gradientEnd || defaultC2;
	const isOnline = data.isOnline as boolean;
	const channelCount = (data.channelCount as number) ?? 0;
	const memoryCount = (data.memoryCount as number) ?? 0;
	const connected = (data.connectedHandles as Record<string, boolean>) ?? {};
	const nodeKind = (data.nodeKind as string) ?? "agent";
	const isAgent = nodeKind === "agent";
	const primaryName = configDisplayName ?? nodeId;
	const onEdit = data.onEdit as (() => void) | undefined;

	return (
		<div
			className={`group/node relative rounded-xl border bg-app-dark-box transition-all ${
				selected
					? "border-accent shadow-lg shadow-accent/20"
					: "border-app-line hover:border-app-line/80"
			}`}
			style={{width: NODE_WIDTH}}
		>
			{/* Edit button (visible on hover) */}
			{onEdit && (
				<button
					onClick={(e) => {
						e.stopPropagation();
						onEdit();
					}}
					className="absolute top-2 right-2 z-10 rounded p-1 text-ink-faint opacity-0 transition-opacity hover:bg-ink/10 hover:text-ink group-hover/node:opacity-100"
				>
					<svg
						width="14"
						height="14"
						viewBox="0 0 16 16"
						fill="none"
						stroke="currentColor"
						strokeWidth="1.5"
						strokeLinecap="round"
						strokeLinejoin="round"
					>
						<path d="M11.5 1.5l3 3L5 14H2v-3L11.5 1.5z" />
					</svg>
				</button>
			)}

			{/* Gradient banner */}
			<div className="relative h-12 overflow-hidden rounded-t-xl">
				<svg
					className="absolute inset-0 h-full w-full"
					preserveAspectRatio="none"
					viewBox="0 0 240 48"
				>
					<defs>
						<linearGradient
							id={`node-grad-${nodeId}`}
							x1="0%"
							y1="0%"
							x2="100%"
							y2="100%"
						>
							<stop offset="0%" stopColor={c1} stopOpacity={0.35} />
							<stop offset="100%" stopColor={c2} stopOpacity={0.2} />
						</linearGradient>
					</defs>
					<rect width="240" height="48" fill={`url(#node-grad-${nodeId})`} />
				</svg>
			</div>

			{/* Avatar + badge row */}
			<div className="relative -mt-6 px-4 pt-3 flex items-end justify-between">
				<div className="relative inline-block">
					<ProfileAvatar
						seed={avatarSeed}
						name={primaryName}
						size={48}
						className="rounded-full border-[3px] border-app-dark-box"
						gradientStart={gradientStart ?? undefined}
						gradientEnd={gradientEnd ?? undefined}
						avatarUrl={isAgent && nodeAvatarUrl ? nodeAvatarUrl : undefined}
					/>
					{isAgent && (
						<div
							className={`absolute bottom-0 right-0 h-3.5 w-3.5 rounded-full border-[2px] border-app-dark-box ${
								isOnline ? "bg-green-500" : "bg-gray-500"
							}`}
						/>
					)}
				</div>
				{/* Badge */}
				<span
					className={`mb-1 rounded px-1.5 py-0.5 text-[9px] font-bold uppercase tracking-wider ${
						isAgent
							? "bg-accent/20 text-accent"
							: "bg-ink-faint/10 text-ink-faint"
					}`}
				>
					{nodeKind}
				</span>
			</div>

			{/* Profile content */}
			<div className="px-4 pt-1.5 pb-3">
				{/* Primary name + self-chosen name inline */}
				{isAgent ? (
					<Link
						to="/agents/$agentId"
						params={{agentId: nodeId}}
						className="flex items-baseline gap-1.5 truncate"
						onClick={(e) => e.stopPropagation()}
					>
						<span className="font-plex text-sm font-semibold text-ink hover:text-accent transition-colors truncate">
							{primaryName}
						</span>
						{chosenName &&
							chosenName !== primaryName &&
							chosenName !== nodeId && (
								<span className="text-[11px] text-ink-faint truncate">
									"{chosenName}"
								</span>
							)}
					</Link>
				) : (
					<div className="flex items-baseline gap-1.5 truncate">
						<span className="font-plex text-sm font-semibold text-ink truncate">
							{primaryName}
						</span>
					</div>
				)}

				{/* Role */}
				{configRole && (
					<p className="mt-0.5 text-[11px] text-ink-dull truncate">
						{configRole}
					</p>
				)}

				{/* Bio */}
				{bio ? (
					<p className="mt-2 text-[11px] leading-relaxed text-ink-faint line-clamp-3">
						{bio}
					</p>
				) : (
					!isAgent &&
					!data.description && (
						<p className="mt-2 text-[11px] leading-relaxed text-ink-faint/60 italic">
							This is you, add your details.
						</p>
					)
				)}

				{/* Stats (agents only) */}
				{isAgent && (
					<div className="mt-2 flex items-center gap-3 border-t border-app-line/40 pt-2 text-[10px] text-ink-faint">
						<span>
							<span className="font-medium text-ink-dull">{channelCount}</span>{" "}
							channels
						</span>
						<span>
							<span className="font-medium text-ink-dull">
								{memoryCount >= 1000
									? `${(memoryCount / 1000).toFixed(1)}k`
									: memoryCount}
							</span>{" "}
							memories
						</span>
					</div>
				)}
			</div>

			{/* Handles on all four sides */}
			{(["top", "bottom", "left", "right"] as const).map((side) => (
				<Handle
					key={`src-${side}`}
					type="source"
					id={side}
					position={
						side === "top"
							? Position.Top
							: side === "bottom"
								? Position.Bottom
								: side === "left"
									? Position.Left
									: Position.Right
					}
					className={`!h-2.5 !w-2.5 !border-2 !border-app-dark-box ${connected[side] ? "!bg-accent" : "!bg-app-line"}`}
				/>
			))}
			{(["top", "bottom", "left", "right"] as const).map((side) => (
				<Handle
					key={`tgt-${side}`}
					type="target"
					id={side}
					position={
						side === "top"
							? Position.Top
							: side === "bottom"
								? Position.Bottom
								: side === "left"
									? Position.Left
									: Position.Right
					}
					className={`!h-2.5 !w-2.5 !border-2 !border-app-dark-box ${connected[side] ? "!bg-accent" : "!bg-app-line"}`}
				/>
			))}
		</div>
	);
}
