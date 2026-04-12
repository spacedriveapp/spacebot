// Read-only CodeMirror 6 viewer with a line-range highlight overlay.
// Used by the Code Inspector panel to show the source around a selected
// graph node with its `line_start..line_end` range visually picked out.

import { useEffect, useRef } from "react";
import { EditorState, StateField, RangeSetBuilder } from "@codemirror/state";
import { EditorView, Decoration, type DecorationSet, lineNumbers } from "@codemirror/view";
import { oneDark } from "@codemirror/theme-one-dark";
import { rust } from "@codemirror/lang-rust";
import { javascript } from "@codemirror/lang-javascript";
import { python } from "@codemirror/lang-python";
import { go } from "@codemirror/lang-go";
import { cpp } from "@codemirror/lang-cpp";

interface Props {
	/** File contents (possibly sliced by the backend). */
	content: string;
	/** Language slug as returned by the backend (`rust`, `typescript`, …). */
	language: string;
	/** 1-indexed line number of the first line in `content`. */
	startLine: number;
	/** 1-indexed inclusive start of the highlighted range (document-absolute). */
	highlightStart?: number;
	/** 1-indexed inclusive end of the highlighted range. Defaults to `highlightStart`. */
	highlightEnd?: number;
}

// Build the list of extensions for a given language slug. CodeMirror lang
// packages are reasonably small so importing all five at module load is
// acceptable; if bundle size becomes a concern this can move to dynamic
// import.
const languageExtension = (language: string) => {
	switch (language) {
		case "rust":
			return rust();
		case "typescript":
		case "javascript":
			return javascript({ typescript: language === "typescript", jsx: true });
		case "python":
			return python();
		case "go":
			return go();
		case "cpp":
		case "c":
			return cpp();
		default:
			return null;
	}
};

// ---------------------------------------------------------------------------
// Highlight decoration — a single StateField that emits line decorations
// across the requested range. The field is immutable (no updates) because
// we tear down and recreate the editor when props change below.
// ---------------------------------------------------------------------------

const highlightLineClass = Decoration.line({ class: "cm-cg-highlight" });

const makeHighlightField = (
	highlightStart: number | undefined,
	highlightEnd: number | undefined,
	startLine: number,
): StateField<DecorationSet> =>
	StateField.define<DecorationSet>({
		create(state) {
			if (highlightStart === undefined) return Decoration.none;
			const builder = new RangeSetBuilder<Decoration>();
			const endLine = highlightEnd ?? highlightStart;
			// Translate from absolute document lines to lines inside the
			// sliced content. `startLine` is 1-indexed.
			const firstDocLine = Math.max(1, highlightStart - startLine + 1);
			const lastDocLine = Math.min(state.doc.lines, endLine - startLine + 1);
			for (let ln = firstDocLine; ln <= lastDocLine; ln++) {
				if (ln < 1 || ln > state.doc.lines) continue;
				const line = state.doc.line(ln);
				builder.add(line.from, line.from, highlightLineClass);
			}
			return builder.finish();
		},
		update(value) {
			return value;
		},
		provide: (f) => EditorView.decorations.from(f),
	});

export function CodeViewer({
	content,
	language,
	startLine,
	highlightStart,
	highlightEnd,
}: Props) {
	const containerRef = useRef<HTMLDivElement>(null);
	const viewRef = useRef<EditorView | null>(null);

	useEffect(() => {
		if (!containerRef.current) return;

		const langExt = languageExtension(language);
		const highlightField = makeHighlightField(highlightStart, highlightEnd, startLine);

		const state = EditorState.create({
			doc: content,
			extensions: [
				EditorState.readOnly.of(true),
				EditorView.editable.of(false),
				lineNumbers({
					formatNumber: (n) => String(n + startLine - 1),
				}),
				highlightField,
				oneDark,
				EditorView.theme({
					"&": {
						height: "100%",
						fontSize: "13px",
						backgroundColor: "#0a0a10",
					},
					".cm-scroller": {
						fontFamily: '"JetBrains Mono", "Fira Code", monospace',
					},
					".cm-cg-highlight": {
						backgroundColor: "rgba(6, 182, 212, 0.14)",
						borderLeft: "3px solid #06b6d4",
					},
				}),
				...(langExt ? [langExt] : []),
			],
		});

		const view = new EditorView({
			state,
			parent: containerRef.current,
		});
		viewRef.current = view;

		// Scroll the highlight into view after the editor mounts. Defer to
		// the next frame so layout has settled.
		if (highlightStart !== undefined) {
			requestAnimationFrame(() => {
				const firstDocLine = Math.max(1, highlightStart - startLine + 1);
				if (firstDocLine >= 1 && firstDocLine <= state.doc.lines) {
					const line = state.doc.line(firstDocLine);
					view.dispatch({
						effects: EditorView.scrollIntoView(line.from, { y: "center" }),
					});
				}
			});
		}

		return () => {
			view.destroy();
			viewRef.current = null;
		};
	}, [content, language, startLine, highlightStart, highlightEnd]);

	return <div ref={containerRef} className="h-full w-full overflow-auto" />;
}
