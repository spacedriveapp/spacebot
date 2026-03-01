import {useState, useRef, KeyboardEvent} from "react";
import {X} from "lucide-react";

interface TagInputProps {
	value: string[];
	onChange: (tags: string[]) => void;
	placeholder?: string;
	className?: string;
}

export function TagInput({value, onChange, placeholder, className}: TagInputProps) {
	const [inputValue, setInputValue] = useState("");
	const inputRef = useRef<HTMLInputElement>(null);

	const addTag = (tag: string) => {
		const trimmed = tag.trim();
		if (trimmed && !value.includes(trimmed)) {
			onChange([...value, trimmed]);
			setInputValue("");
		}
	};

	const removeTag = (tagToRemove: string) => {
		onChange(value.filter((tag) => tag !== tagToRemove));
	};

	const handleKeyDown = (e: KeyboardEvent<HTMLInputElement>) => {
		if (e.key === "Enter") {
			e.preventDefault();
			addTag(inputValue);
		} else if (e.key === "Backspace" && !inputValue && value.length > 0) {
			removeTag(value[value.length - 1]);
		}
	};

	const handleBlur = () => {
		if (inputValue.trim()) {
			addTag(inputValue);
		}
	};

	return (
		<div className={className}>
			<div className="flex flex-wrap gap-2 p-2 border border-app-line/50 rounded-md bg-app-darkBox/30 min-h-[42px] focus-within:border-accent/50">
				{value.map((tag) => (
					<div
						key={tag}
						className="flex items-center gap-1 px-2 py-1 bg-app-box border border-app-line/30 rounded text-sm text-ink"
					>
						<span>{tag}</span>
						<button
							type="button"
							onClick={() => removeTag(tag)}
							className="text-ink-faint hover:text-ink transition-colors"
						>
							<X size={14} />
						</button>
					</div>
				))}
				<input
					ref={inputRef}
					type="text"
					value={inputValue}
					onChange={(e) => setInputValue(e.target.value)}
					onKeyDown={handleKeyDown}
					onBlur={handleBlur}
					placeholder={value.length === 0 ? placeholder : ""}
					className="flex-1 min-w-[120px] bg-transparent border-none outline-none text-sm text-ink placeholder:text-ink-faint"
				/>
			</div>
		</div>
	);
}
