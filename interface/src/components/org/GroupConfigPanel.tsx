import {useState} from "react";
import {motion} from "framer-motion";
import {Button} from "@spacedrive/primitives";
import type {TopologyGroup} from "@/api/client";

interface GroupConfigPanelProps {
	group: TopologyGroup;
	allAgents: string[];
	onUpdate: (agentIds: string[], name: string) => void;
	onDelete: () => void;
	onClose: () => void;
}

export function GroupConfigPanel({
	group,
	allAgents,
	onUpdate,
	onDelete,
	onClose,
}: GroupConfigPanelProps) {
	const [name, setName] = useState(group.name);
	const [agentIds, setAgentIds] = useState<Set<string>>(
		new Set(group.agent_ids),
	);

	const toggleAgent = (id: string) => {
		setAgentIds((prev) => {
			const next = new Set(prev);
			if (next.has(id)) next.delete(id);
			else next.add(id);
			return next;
		});
	};

	return (
		<motion.div
			initial={{opacity: 0, y: 8}}
			animate={{opacity: 1, y: 0}}
			exit={{opacity: 0, y: 8}}
			transition={{duration: 0.15}}
			className="absolute right-4 top-4 z-20 w-64 rounded-lg border border-app-line/50 bg-app-dark-box/95 p-4 shadow-xl backdrop-blur-sm"
		>
			<div className="mb-3 flex items-center justify-between">
				<span className="text-sm font-medium text-ink">Group Settings</span>
				<button
					onClick={onClose}
					className="text-ink-faint hover:text-ink transition-colors text-sm"
				>
					Close
				</button>
			</div>

			{/* Name */}
			<div className="mb-3">
				<label className="mb-1 block text-tiny font-medium text-ink-dull">
					Name
				</label>
				<input
					value={name}
					onChange={(e) => setName(e.target.value)}
					className="w-full rounded bg-app-input px-2.5 py-1.5 text-sm text-ink outline-none border border-app-line/50 focus:border-accent/50"
				/>
			</div>

			{/* Agent membership */}
			<div className="mb-4">
				<label className="mb-1 block text-tiny font-medium text-ink-dull">
					Agents
				</label>
				<div className="flex flex-col gap-1 max-h-40 overflow-y-auto">
					{allAgents.map((id) => (
						<button
							key={id}
							onClick={() => toggleAgent(id)}
							className={`flex items-center gap-2 rounded px-2 py-1.5 text-tiny transition-colors text-left ${
								agentIds.has(id)
									? "bg-accent/15 text-accent"
									: "bg-app-box text-ink-faint hover:text-ink-dull"
							}`}
						>
							<span
								className={`h-2 w-2 rounded-full flex-shrink-0 ${
									agentIds.has(id) ? "bg-accent" : "bg-app-line"
								}`}
							/>
							{id}
						</button>
					))}
				</div>
			</div>

			<div className="flex gap-2">
				<Button
					onClick={() => onUpdate([...agentIds], name)}
					size="sm"
					className="flex-1 bg-accent/15 text-tiny text-accent hover:bg-accent/25"
				>
					Save
				</Button>
				<Button
					onClick={onDelete}
					size="sm"
					variant="accent"
					className="text-tiny"
				>
					Delete
				</Button>
			</div>
		</motion.div>
	);
}
