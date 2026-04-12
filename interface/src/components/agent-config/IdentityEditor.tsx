import {useCallback, useEffect, useState} from "react";
import {cx} from "class-variance-authority";
import {TextArea} from "@spacedrive/primitives";
import {Markdown} from "@/components/Markdown";
import type {IdentityEditorProps} from "./types";

export function IdentityEditor({
	label,
	description,
	content,
	onDirtyChange,
	saveHandlerRef,
	onSave,
}: IdentityEditorProps) {
	const [value, setValue] = useState(content ?? "");
	const [localDirty, setLocalDirty] = useState(false);
	const [mode, setMode] = useState<"edit" | "preview">("edit");

	useEffect(() => {
		if (!localDirty) {
			setValue(content ?? "");
		}
	}, [content, localDirty]);

	useEffect(() => {
		onDirtyChange(localDirty);
	}, [localDirty, onDirtyChange]);

	const handleChange = useCallback(
		(event: React.ChangeEvent<HTMLTextAreaElement>) => {
			setValue(event.target.value);
			setLocalDirty(true);
		},
		[],
	);

	const handleSave = useCallback(() => {
		onSave(value);
		setLocalDirty(false);
	}, [onSave, value]);

	const handleRevert = useCallback(() => {
		setValue(content ?? "");
		setLocalDirty(false);
	}, [content]);

	const handleKeyDown = useCallback(
		(event: React.KeyboardEvent) => {
			if ((event.metaKey || event.ctrlKey) && event.key === "s") {
				event.preventDefault();
				if (localDirty) handleSave();
			}
		},
		[localDirty, handleSave],
	);

	useEffect(() => {
		saveHandlerRef.current.save = handleSave;
		saveHandlerRef.current.revert = handleRevert;
		return () => {
			saveHandlerRef.current.save = undefined;
			saveHandlerRef.current.revert = undefined;
		};
	}, [handleSave, handleRevert]);

	return (
		<>
			<div className="flex items-center justify-between border-b border-app-line/50 bg-app-dark-box/20 px-5 py-2.5">
				<div className="flex items-center gap-3">
					<h3 className="text-sm font-medium text-ink">{label}</h3>
					<span className="rounded bg-app-dark-box px-1.5 py-0.5 font-mono text-tiny text-ink-faint">
						{description}
					</span>
				</div>
				<div className="flex items-center gap-3">
					<div className="flex items-center rounded border border-app-line/50 text-tiny">
						<button
							onClick={() => setMode("edit")}
							className={cx(
								"px-2 py-0.5 rounded-l transition-colors",
								mode === "edit"
									? "bg-app-dark-box text-ink"
									: "text-ink-faint hover:text-ink",
							)}
						>
							Edit
						</button>
						<button
							onClick={() => setMode("preview")}
							className={cx(
								"px-2 py-0.5 rounded-r transition-colors",
								mode === "preview"
									? "bg-app-dark-box text-ink"
									: "text-ink-faint hover:text-ink",
							)}
						>
							Preview
						</button>
					</div>
					{localDirty ? (
						<span className="text-tiny text-amber-400">Unsaved changes</span>
					) : (
						<span className="text-tiny text-ink-faint/50">Cmd+S to save</span>
					)}
				</div>
			</div>
			<div className="flex-1 overflow-y-auto p-4">
				{mode === "edit" ? (
					<TextArea
						value={value}
						onChange={handleChange}
						onKeyDown={handleKeyDown}
						placeholder={`Write ${label.toLowerCase()} content here...`}
						className="h-full w-full resize-none border-transparent bg-app-dark-box/30 px-4 py-3 font-mono leading-relaxed placeholder:text-ink-faint/40"
						spellCheck={false}
					/>
				) : (
					<div className="prose-sm px-4 py-3">
						{value ? (
							<Markdown>{value}</Markdown>
						) : (
							<span className="text-ink-faint/40 text-sm">
								Nothing to preview.
							</span>
						)}
					</div>
				)}
			</div>
		</>
	);
}
