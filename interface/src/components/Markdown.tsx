import { memo } from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import rehypeRaw from "rehype-raw";

// Stable module-level references so ReactMarkdown never re-renders due to
// new array/object identities on every call.
const remarkPlugins = [remarkGfm];
const rehypePlugins = [rehypeRaw];
const markdownComponents = {
	a: ({ children, href, ...props }: React.ComponentPropsWithoutRef<"a">) => (
		<a href={href} target="_blank" rel="noopener noreferrer" {...props}>
			{children}
		</a>
	),
};

export const Markdown = memo(function Markdown({
	children,
	className,
}: {
	children: string;
	className?: string;
}) {
	return (
		<div className={className ? `markdown ${className}` : "markdown"}>
			<ReactMarkdown
				remarkPlugins={remarkPlugins}
				rehypePlugins={rehypePlugins}
				components={markdownComponents}
			>
				{children}
			</ReactMarkdown>
		</div>
	);
});
