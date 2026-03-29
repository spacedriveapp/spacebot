import { useMemo } from "react";

interface DiffViewerProps {
	diff: string;
	maxHeight?: string;
}

interface DiffLine {
	type: "add" | "remove" | "context" | "header" | "hunk";
	content: string;
	lineNumber?: number;
}

export function DiffViewer({ diff, maxHeight = "400px" }: DiffViewerProps) {
	const lines = useMemo(() => parseDiff(diff), [diff]);

	if (!diff.trim()) {
		return (
			<div className="flex items-center justify-center rounded-md border border-app-line bg-app-darkBox p-4 text-sm text-ink-faint">
				No changes to display
			</div>
		);
	}

	return (
		<div
			className="overflow-auto rounded-md border border-app-line bg-app-darkBox font-mono text-xs"
			style={{ maxHeight }}
		>
			{lines.map((line, i) => (
				<div
					key={i}
					className={`whitespace-pre px-3 py-0.5 ${lineClass(line.type)}`}
				>
					{line.content}
				</div>
			))}
		</div>
	);
}

function lineClass(type: DiffLine["type"]): string {
	switch (type) {
		case "add":
			return "bg-emerald-500/10 text-emerald-300";
		case "remove":
			return "bg-red-500/10 text-red-300";
		case "header":
			return "bg-accent/10 text-accent font-semibold sticky top-0";
		case "hunk":
			return "bg-app-box text-ink-faint";
		default:
			return "text-ink-dull";
	}
}

function parseDiff(raw: string): DiffLine[] {
	return raw.split("\n").map((content) => {
		if (content.startsWith("+++") || content.startsWith("---")) {
			return { type: "header" as const, content };
		}
		if (content.startsWith("diff ")) {
			return { type: "header" as const, content };
		}
		if (content.startsWith("@@")) {
			return { type: "hunk" as const, content };
		}
		if (content.startsWith("+")) {
			return { type: "add" as const, content };
		}
		if (content.startsWith("-")) {
			return { type: "remove" as const, content };
		}
		return { type: "context" as const, content };
	});
}
