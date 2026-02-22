import { describe, it, expect, vi } from "vitest";
import { render } from "@testing-library/react";
import { Sidebar } from "./Sidebar";

vi.mock("framer-motion", () => ({
  motion: {
    nav: ({
      children,
      className,
      initial,
    }: {
      children: React.ReactNode;
      className?: string;
      initial?: { width?: number };
      animate?: unknown;
      transition?: unknown;
    }) => (
      <nav className={className} style={{ width: initial?.width }}>
        {children}
      </nav>
    ),
  },
}));

vi.mock("@tanstack/react-router", () => ({
  useMatchRoute: () => () => false,
  Link: ({
    children,
    className,
  }: {
    children: React.ReactNode;
    to?: string;
    params?: unknown;
    className?: string;
    style?: unknown;
    title?: string;
  }) => <a className={className}>{children}</a>,
}));

vi.mock("@tanstack/react-query", () => ({
  useQuery: () => ({ data: undefined }),
}));

vi.mock("@dnd-kit/core", () => ({
  DndContext: ({ children }: { children: React.ReactNode }) => <>{children}</>,
  closestCenter: vi.fn(),
  KeyboardSensor: vi.fn(),
  PointerSensor: vi.fn(),
  useSensor: vi.fn(),
  useSensors: vi.fn(() => []),
}));

vi.mock("@dnd-kit/sortable", () => ({
  SortableContext: ({ children }: { children: React.ReactNode }) => (
    <>{children}</>
  ),
  sortableKeyboardCoordinates: vi.fn(),
  arrayMove: vi.fn(),
  useSortable: () => ({
    attributes: {},
    listeners: {},
    setNodeRef: vi.fn(),
    transform: null,
    transition: null,
    isDragging: false,
  }),
  verticalListSortingStrategy: vi.fn(),
}));

vi.mock("@dnd-kit/utilities", () => ({
  CSS: { Transform: { toString: vi.fn(() => "") } },
}));

vi.mock("@/api/client", () => ({
  api: { agents: vi.fn(), channels: vi.fn() },
  BASE_PATH: "",
}));

vi.mock("@/hooks/useAgentOrder", () => ({
  useAgentOrder: () => [[], vi.fn()],
}));

vi.mock("@/ui", () => ({
  Button: ({
    children,
    onClick,
  }: {
    children: React.ReactNode;
    onClick?: () => void;
    variant?: string;
    size?: string;
    className?: string;
  }) => <button onClick={onClick}>{children}</button>,
}));

vi.mock("@hugeicons/core-free-icons", () => ({
  ArrowLeft01Icon: vi.fn(),
  DashboardSquare01Icon: vi.fn(),
  Settings01Icon: vi.fn(),
}));

vi.mock("@hugeicons/react", () => ({
  HugeiconsIcon: ({
    icon,
    size,
  }: {
    icon: unknown;
    size?: number;
    className?: string;
  }) => <svg data-icon={String(icon)} style={{ width: size, height: size }} />,
}));

vi.mock("@/components/CreateAgentDialog", () => ({
  CreateAgentDialog: ({
    open,
    onOpenChange,
  }: {
    open: boolean;
    onOpenChange: (v: boolean) => void;
  }) =>
    open ? <div role="dialog" onClick={() => onOpenChange(false)} /> : null,
}));

describe("Sidebar", () => {
  it("root nav has shrink-0 class when collapsed", () => {
    const { container } = render(
      <Sidebar collapsed={true} liveStates={{}} onToggle={vi.fn()} />,
    );
    const nav = container.querySelector("nav");
    expect(nav).toHaveClass("shrink-0");
  });

  it("root nav has shrink-0 class when expanded", () => {
    const { container } = render(
      <Sidebar collapsed={false} liveStates={{}} onToggle={vi.fn()} />,
    );
    const nav = container.querySelector("nav");
    expect(nav).toHaveClass("shrink-0");
  });

  it("root nav initial width is 56 when collapsed", () => {
    const { container } = render(
      <Sidebar collapsed={true} liveStates={{}} onToggle={vi.fn()} />,
    );
    const nav = container.querySelector("nav") as HTMLElement;
    expect(nav.style.width).toBe("56px");
  });

  it("root nav initial width is 224 when expanded", () => {
    const { container } = render(
      <Sidebar collapsed={false} liveStates={{}} onToggle={vi.fn()} />,
    );
    const nav = container.querySelector("nav") as HTMLElement;
    expect(nav.style.width).toBe("224px");
  });
});
