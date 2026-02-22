import { describe, it, expect, vi } from "vitest";
import { render } from "@testing-library/react";
import { SettingSectionNav } from "./SettingSectionNav";

const singleGroup = [
	{
		label: "Settings",
		sections: [
			{ id: "a", label: "Section A" },
			{ id: "b", label: "Section B" },
			{ id: "c", label: "Section C" },
		],
	},
];

const multiGroup = [
	{
		label: "Group One",
		sections: [
			{ id: "a", label: "Section A" },
			{ id: "b", label: "Section B" },
		],
	},
	{
		label: "Group Two",
		sections: [
			{ id: "c", label: "Section C" },
			{ id: "d", label: "Section D" },
			{ id: "e", label: "Section E" },
		],
	},
];

describe("SettingSectionNav", () => {
	it("desktop sidebar div has hidden md:flex classes", () => {
		const { container } = render(
			<SettingSectionNav groups={singleGroup} activeSection="a" onSectionChange={vi.fn()} />,
		);
		const desktopDiv = container.children[0] as HTMLElement;
		expect(desktopDiv).toHaveClass("hidden");
		expect(desktopDiv).toHaveClass("md:flex");
	});

	it("mobile tab bar div has flex md:hidden classes", () => {
		const { container } = render(
			<SettingSectionNav groups={singleGroup} activeSection="a" onSectionChange={vi.fn()} />,
		);
		const mobileDiv = container.children[1] as HTMLElement;
		expect(mobileDiv).toHaveClass("flex");
		expect(mobileDiv).toHaveClass("md:hidden");
	});

	it("renders correct number of buttons for a single-group config", () => {
		const { container } = render(
			<SettingSectionNav groups={singleGroup} activeSection="a" onSectionChange={vi.fn()} />,
		);
		const desktopDiv = container.children[0] as HTMLElement;
		const mobileDiv = container.children[1] as HTMLElement;
		expect(desktopDiv.querySelectorAll("button")).toHaveLength(3);
		expect(mobileDiv.querySelectorAll("button")).toHaveLength(3);
	});

	it("renders correct number of buttons for a multi-group config", () => {
		const { container } = render(
			<SettingSectionNav groups={multiGroup} activeSection="a" onSectionChange={vi.fn()} />,
		);
		const desktopDiv = container.children[0] as HTMLElement;
		const mobileDiv = container.children[1] as HTMLElement;
		expect(desktopDiv.querySelectorAll("button")).toHaveLength(5);
		expect(mobileDiv.querySelectorAll("button")).toHaveLength(5);
	});

	it("group separator rendered in mobile bar when multiple groups provided", () => {
		const { container } = render(
			<SettingSectionNav groups={multiGroup} activeSection="a" onSectionChange={vi.fn()} />,
		);
		const mobileDiv = container.children[1] as HTMLElement;
		const separators = mobileDiv.querySelectorAll("div.w-px");
		expect(separators).toHaveLength(1);
	});

	it("no separator rendered in mobile bar for single group", () => {
		const { container } = render(
			<SettingSectionNav groups={singleGroup} activeSection="a" onSectionChange={vi.fn()} />,
		);
		const mobileDiv = container.children[1] as HTMLElement;
		const separators = mobileDiv.querySelectorAll("div.w-px");
		expect(separators).toHaveLength(0);
	});

	it("active section button has active styling class", () => {
		const { container } = render(
			<SettingSectionNav groups={singleGroup} activeSection="b" onSectionChange={vi.fn()} />,
		);
		const desktopDiv = container.children[0] as HTMLElement;
		const desktopButtons = desktopDiv.querySelectorAll("button");
		expect(desktopButtons[1]).toHaveClass("bg-app-darkBox");
		const mobileDiv = container.children[1] as HTMLElement;
		const mobileButtons = mobileDiv.querySelectorAll("button");
		expect(mobileButtons[1]).toHaveClass("text-ink");
	});
});
