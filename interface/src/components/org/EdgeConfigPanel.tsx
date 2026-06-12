import {useState} from "react";
import {motion} from "framer-motion";
import type {Edge} from "@xyflow/react";
import {Button} from "@spacedrive/primitives";
import type {LinkDirection, LinkKind} from "@/api/client";

interface EdgeConfigPanelProps {
	edge: Edge;
	onUpdate: (direction: LinkDirection, kind: LinkKind) => void;
	onDelete: () => void;
	onClose: () => void;
}

export function EdgeConfigPanel({
	edge,
	onUpdate,
	onDelete,
	onClose,
}: EdgeConfigPanelProps) {
	const [direction, setDirection] = useState<LinkDirection>(
		(edge.data?.direction as LinkDirection) ?? "two_way",
	);
	const [kind, setKind] = useState<LinkKind>(
		(edge.data?.kind as LinkKind) ?? "peer",
	);

	return (
		<motion.div
			initial={{opacity: 0, y: 8}}
			animate={{opacity: 1, y: 0}}
			exit={{opacity: 0, y: 8}}
			transition={{duration: 0.15}}
			className="absolute right-4 top-4 z-20 w-64 rounded-lg border border-app-line/50 bg-app-dark-box/95 p-4 shadow-xl backdrop-blur-sm"
		>
			<div className="mb-3 flex items-center justify-between">
				<span className="text-sm font-medium text-ink">Link Settings</span>
				<button
					onClick={onClose}
					className="text-ink-faint hover:text-ink transition-colors text-sm"
				>
					Close
				</button>
			</div>

			<div className="mb-2 text-tiny text-ink-faint">
				{edge.source} → {edge.target}
			</div>

			{/* Kind */}
			<div className="mb-3">
				<label className="mb-1 block text-tiny font-medium text-ink-dull">
					Kind
				</label>
				<div className="flex gap-1.5">
					{(["hierarchical", "peer"] as const).map((k) => (
						<button
							key={k}
							onClick={() => setKind(k)}
							className={`flex-1 rounded px-2 py-1.5 text-tiny transition-colors capitalize ${
								kind === k
									? "bg-ink/10 text-ink"
									: "bg-app-box/50 text-ink-faint hover:text-ink-dull"
							}`}
						>
							{k}
						</button>
					))}
				</div>
			</div>

			{/* Direction */}
			<div className="mb-4">
				<label className="mb-1 block text-tiny font-medium text-ink-dull">
					Direction
				</label>
				<div className="flex gap-1.5">
					{(["one_way", "two_way"] as const).map((d) => (
						<button
							key={d}
							onClick={() => setDirection(d)}
							className={`flex-1 rounded px-2 py-1.5 text-tiny transition-colors ${
								direction === d
									? "bg-ink/10 text-ink"
									: "bg-app-box/50 text-ink-faint hover:text-ink-dull"
							}`}
						>
							{d === "one_way" ? "One Way" : "Two Way"}
						</button>
					))}
				</div>
			</div>

			<div className="flex gap-2">
				<Button
					onClick={() => onUpdate(direction, kind)}
					size="sm"
					className="flex-1"
				>
					Save
				</Button>
				<Button
					onClick={onDelete}
					size="sm"
					variant="outline"
					className="text-tiny text-ink-faint"
				>
					Delete
				</Button>
			</div>
		</motion.div>
	);
}
