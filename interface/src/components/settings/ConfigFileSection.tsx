import {useState, useEffect, useRef} from "react";
import {useQuery, useMutation, useQueryClient} from "@tanstack/react-query";
import {api} from "@/api/client";
import {Button} from "@spacedrive/primitives";
import {parse as parseToml} from "smol-toml";

export function ConfigFileSection() {
	const queryClient = useQueryClient();
	const editorRef = useRef<HTMLDivElement>(null);
	const viewRef = useRef<import("@codemirror/view").EditorView | null>(null);
	const [originalContent, setOriginalContent] = useState("");
	const [currentContent, setCurrentContent] = useState("");
	const [validationError, setValidationError] = useState<string | null>(null);
	const [message, setMessage] = useState<{
		text: string;
		type: "success" | "error";
	} | null>(null);
	const [editorLoaded, setEditorLoaded] = useState(false);

	const {data, isLoading} = useQuery({
		queryKey: ["raw-config"],
		queryFn: api.rawConfig,
		staleTime: 5_000,
	});

	const updateMutation = useMutation({
		mutationFn: (content: string) => api.updateRawConfig(content),
		onSuccess: (result) => {
			if (result.success) {
				setOriginalContent(currentContent);
				setMessage({text: result.message, type: "success"});
				setValidationError(null);
				// Invalidate all config-related queries so other tabs pick up changes
				queryClient.invalidateQueries({queryKey: ["providers"]});
				queryClient.invalidateQueries({queryKey: ["global-settings"]});
				queryClient.invalidateQueries({queryKey: ["agents"]});
				queryClient.invalidateQueries({queryKey: ["overview"]});
			} else {
				setMessage({text: result.message, type: "error"});
			}
		},
		onError: (error) => {
			setMessage({text: `Failed: ${error.message}`, type: "error"});
		},
	});

	const isDirty = currentContent !== originalContent;

	// Initialize CodeMirror when data loads
	useEffect(() => {
		if (!data?.content || !editorRef.current || editorLoaded) return;

		const content = data.content;
		setOriginalContent(content);
		setCurrentContent(content);

		// Lazy-load CodeMirror to avoid SSR issues and keep initial bundle small
		Promise.all([
			import("@codemirror/view"),
			import("@codemirror/state"),
			import("codemirror"),
			import("@codemirror/theme-one-dark"),
			import("@codemirror/language"),
			import("@codemirror/legacy-modes/mode/toml"),
		]).then(([viewMod, stateMod, cm, themeMod, langMod, tomlMode]) => {
			if (!editorRef.current) return;

			const tomlLang = langMod.StreamLanguage.define(tomlMode.toml);

			const updateListener = viewMod.EditorView.updateListener.of((update) => {
				if (update.docChanged) {
					const newContent = update.state.doc.toString();
					setCurrentContent(newContent);
					try {
						parseToml(newContent);
						setValidationError(null);
					} catch (error: any) {
						setValidationError(error.message || "Invalid TOML");
					}
				}
			});

			const theme = viewMod.EditorView.theme({
				"&": {
					height: "100%",
					fontSize: "13px",
				},
				".cm-scroller": {
					fontFamily: "'IBM Plex Mono', monospace",
					overflow: "auto",
				},
				".cm-gutters": {
					backgroundColor: "transparent",
					borderRight: "1px solid hsl(var(--color-app-line) / 0.3)",
				},
				".cm-activeLineGutter": {
					backgroundColor: "transparent",
				},
			});

			const state = stateMod.EditorState.create({
				doc: content,
				extensions: [
					cm.basicSetup,
					tomlLang,
					themeMod.oneDark,
					theme,
					updateListener,
					viewMod.keymap.of([
						{
							key: "Mod-s",
							run: () => {
								// Trigger save via DOM event since we can't access React state here
								editorRef.current?.dispatchEvent(new CustomEvent("cm-save"));
								return true;
							},
						},
					]),
				],
			});

			const view = new viewMod.EditorView({
				state,
				parent: editorRef.current,
			});

			viewRef.current = view;
			setEditorLoaded(true);
		});

		return () => {
			viewRef.current?.destroy();
			viewRef.current = null;
		};
	}, [data?.content]);

	// Handle Cmd+S from CodeMirror
	useEffect(() => {
		const element = editorRef.current;
		if (!element) return;

		const handler = () => {
			if (isDirty && !validationError) {
				updateMutation.mutate(currentContent);
			}
		};

		element.addEventListener("cm-save", handler);
		return () => element.removeEventListener("cm-save", handler);
	}, [isDirty, validationError, currentContent]);

	const handleSave = () => {
		if (!isDirty || validationError) return;
		setMessage(null);
		updateMutation.mutate(currentContent);
	};

	const handleRevert = () => {
		if (!viewRef.current) return;
		const view = viewRef.current;
		view.dispatch({
			changes: {from: 0, to: view.state.doc.length, insert: originalContent},
		});
		setCurrentContent(originalContent);
		setValidationError(null);
		setMessage(null);
	};

	return (
		<div className="flex h-full flex-col">
			{/* Description + actions */}
			<div className="flex items-center justify-between px-6 py-4 border-b border-app-line/30">
				<p className="text-sm text-ink-dull">
					Edit the raw configuration file. Changes are validated as TOML before
					saving.
				</p>
				<div className="flex items-center gap-2 flex-shrink-0 ml-4">
					{isDirty && (
						<Button onClick={handleRevert} variant="outline" size="md">
							Revert
						</Button>
					)}
					<Button
						onClick={handleSave}
						disabled={!isDirty || !!validationError}
						loading={updateMutation.isPending}
						size="md"
					>
						Save
					</Button>
				</div>
			</div>

			{/* Validation / status bar */}
			{(validationError || message) && (
				<div
					className={`border-b px-6 py-2 text-sm ${
						validationError
							? "border-red-500/20 bg-red-500/5 text-red-400"
							: message?.type === "success"
								? "border-green-500/20 bg-green-500/5 text-green-400"
								: "border-red-500/20 bg-red-500/5 text-red-400"
					}`}
				>
					{validationError ? `Syntax error: ${validationError}` : message?.text}
				</div>
			)}

			{/* Editor */}
			<div className="flex-1 overflow-hidden">
				{isLoading ? (
					<div className="flex items-center gap-2 p-6 text-ink-dull">
						<div className="h-2 w-2 animate-pulse rounded-full bg-accent" />
						Loading config...
					</div>
				) : (
					<div ref={editorRef} className="h-full" />
				)}
			</div>
		</div>
	);
}
