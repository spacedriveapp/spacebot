import type {NodeProps} from "@xyflow/react";
import {GROUP_HEADER} from "./constants";

export function GroupNode({data, selected}: NodeProps) {
	const color = (data.color as string) ?? "#6366f1";
	const name = data.label as string;

	return (
		<div
			className={`rounded-2xl border transition-all ${
				selected ? "border-opacity-60" : "border-opacity-30"
			}`}
			style={{
				width: data.width as number,
				height: data.height as number,
				borderColor: color,
				backgroundColor: `${color}08`,
			}}
		>
			<div
				className="flex items-center gap-2 rounded-t-2xl px-4"
				style={{
					height: GROUP_HEADER,
					background: `linear-gradient(135deg, ${color}18, ${color}08)`,
				}}
			>
				<span
					className="h-2 w-2 rounded-full"
					style={{backgroundColor: color}}
				/>
				<span
					className="text-[11px] font-semibold uppercase tracking-wider"
					style={{color}}
				>
					{name}
				</span>
			</div>
		</div>
	);
}
