import { describe, it, expect, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import { AgentTabs } from "./AgentTabs";

vi.mock("@tanstack/react-router", () => ({
	useMatchRoute: () => () => false,
	Link: ({ children, className }: { children: React.ReactNode; className?: string; to?: string; params?: unknown }) => (
		<a className={className}>{children}</a>
	),
}));

describe("AgentTabs", () => {
	it("renders 10 tab links", () => {
		const { container } = render(<AgentTabs agentId="main" />);
		const links = container.querySelectorAll("a");
		expect(links).toHaveLength(10);
	});

	it("container has flex and h-12 classes", () => {
		const { container } = render(<AgentTabs agentId="main" />);
		const div = container.firstElementChild!;
		expect(div).toHaveClass("flex");
		expect(div).toHaveClass("h-12");
		expect(div).toHaveClass("border-b");
	});

	it("container has overflow-x-auto class", () => {
		const { container } = render(<AgentTabs agentId="main" />);
		const div = container.firstElementChild!;
		expect(div).toHaveClass("overflow-x-auto");
	});

	it("container has no-scrollbar class", () => {
		const { container } = render(<AgentTabs agentId="main" />);
		const div = container.firstElementChild!;
		expect(div).toHaveClass("no-scrollbar");
	});

	it("renders all tab labels", () => {
		render(<AgentTabs agentId="main" />);
		expect(screen.getByText("Overview")).toBeDefined();
		expect(screen.getByText("Chat")).toBeDefined();
		expect(screen.getByText("Config")).toBeDefined();
	});
});
