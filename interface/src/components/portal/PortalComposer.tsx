import { useRef, useState } from "react";
import { ChatComposer, type ModelOption } from "@spacedrive/ai";
import { usePopover } from "@spacedrive/primitives";
import { Paperclip, X } from "@phosphor-icons/react";

interface PortalComposerProps {
	agentName: string;
	draft: string;
	onDraftChange: (value: string) => void;
	onSend: () => void;
	disabled: boolean;
	modelOptions: ModelOption[];
	selectedModel: string;
	onSelectModel: (model: string) => void;
	projectOptions: string[];
	selectedProject: string;
	onSelectProject: (project: string) => void;
	pendingFiles: File[];
	onAddFiles: (files: File[]) => void;
	onRemoveFile: (index: number) => void;
}

/**
 * Portal chat composer — wraps @spacedrive/ai's ChatComposer with spacebot
 * project + model selectors, a per-agent placeholder, and file attachment support.
 */
export function PortalComposer({
	agentName,
	draft,
	onDraftChange,
	onSend,
	disabled,
	modelOptions,
	selectedModel,
	onSelectModel,
	projectOptions,
	selectedProject,
	onSelectProject,
	pendingFiles,
	onAddFiles,
	onRemoveFile,
}: PortalComposerProps) {
	const projectPopover = usePopover();
	const fileInputRef = useRef<HTMLInputElement>(null);
	const [isDragging, setIsDragging] = useState(false);
	const dragCounter = useRef(0);

	const handleFileChange = (e: React.ChangeEvent<HTMLInputElement>) => {
		const files = Array.from(e.target.files ?? []);
		if (files.length > 0) onAddFiles(files);
		// Reset so the same file can be re-selected if removed.
		e.target.value = "";
	};

	const handleDragEnter = (e: React.DragEvent) => {
		e.preventDefault();
		dragCounter.current++;
		if (e.dataTransfer.types.includes("Files")) setIsDragging(true);
	};

	const handleDragOver = (e: React.DragEvent) => {
		e.preventDefault();
		e.dataTransfer.dropEffect = "copy";
	};

	const handleDragLeave = (e: React.DragEvent) => {
		e.preventDefault();
		dragCounter.current--;
		if (dragCounter.current === 0) setIsDragging(false);
	};

	const handleDrop = (e: React.DragEvent) => {
		e.preventDefault();
		dragCounter.current = 0;
		setIsDragging(false);
		const files = Array.from(e.dataTransfer.files);
		if (files.length > 0) onAddFiles(files);
	};

	const paperclipButton = (
		<>
			<input
				ref={fileInputRef}
				type="file"
				multiple
				className="hidden"
				onChange={handleFileChange}
			/>
			<button
				type="button"
				onClick={() => fileInputRef.current?.click()}
				className="text-ink-faint hover:text-ink flex h-9 w-9 items-center justify-center rounded-full transition-colors"
				title="Attach files"
			>
				<Paperclip size={16} />
			</button>
		</>
	);

	return (
		<div
			className="relative flex flex-col gap-2"
			onDragEnter={handleDragEnter}
			onDragOver={handleDragOver}
			onDragLeave={handleDragLeave}
			onDrop={handleDrop}
		>
			{isDragging && (
				<div className="border-accent bg-accent/10 pointer-events-none absolute inset-0 z-20 flex items-center justify-center rounded-2xl border-2 border-dashed">
					<p className="text-accent text-sm font-medium">Drop files to attach</p>
				</div>
			)}
			{pendingFiles.length > 0 && (
				<div className="flex flex-wrap gap-1.5 px-1">
					{pendingFiles.map((file, i) => (
						<div
							key={`${file.name}-${file.size}-${i}`}
							className="bg-app-box border-app-line flex items-center gap-1.5 rounded-full border px-2.5 py-1 text-xs"
						>
							<span className="text-ink max-w-[160px] truncate">{file.name}</span>
							<button
								type="button"
								onClick={() => onRemoveFile(i)}
								className="text-ink-faint hover:text-ink ml-0.5 flex-shrink-0 transition-colors"
							>
								<X size={12} weight="bold" />
							</button>
						</div>
					))}
				</div>
			)}
			<ChatComposer
				draft={draft}
				onDraftChange={onDraftChange}
				onSend={onSend}
				placeholder={disabled ? "Waiting for response..." : `Message ${agentName}...`}
				isSending={disabled}
				toolbarExtra={paperclipButton}
				projectSelector={
					projectOptions.length > 0
						? {
								value: selectedProject,
								options: projectOptions,
								onChange: onSelectProject,
								popover: projectPopover,
							}
						: undefined
				}
				modelSelector={
					modelOptions.length > 0
						? {
								value: selectedModel,
								options: modelOptions,
								onChange: onSelectModel,
							}
						: undefined
				}
			/>
		</div>
	);
}
