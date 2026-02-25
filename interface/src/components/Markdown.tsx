import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";

export function Markdown({
	children,
	className,
}: {
	children: string;
	className?: string;
}) {
	return (
		<div className={className ? `markdown ${className}` : "markdown"}>
			<ReactMarkdown
				remarkPlugins={[remarkGfm]}
				components={{
					a: ({ children, href, ...props }) => (
						<a href={href} target="_blank" rel="noopener noreferrer" {...props}>
							{children}
						</a>
					),
				}}
			>
				{children}
			</ReactMarkdown>
		</div>
	);
}
