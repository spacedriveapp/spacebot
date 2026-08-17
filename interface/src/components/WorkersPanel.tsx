import {useMemo, useState} from "react";
import {useQueries, useQuery} from "@tanstack/react-query";
import {CaretLeft, MagnifyingGlass, Queue} from "@phosphor-icons/react";
import {
	CircleButton,
	PopoverContent,
	PopoverRoot,
	PopoverTrigger,
} from "@spacedrive/primitives";
import {cx} from "class-variance-authority";
import {api} from "@/api/client";
import {
	ProcessCard,
	ProcessDetail,
	type ProcessRunDisplay,
	type ProcessSelection,
} from "@/components/processes/ProcessRunView";
import {useLiveContext} from "@/hooks/useLiveContext";

type Tab = "active" | "history";

interface SelectedProcess extends ProcessSelection {
	agentId: string;
	fallback: ProcessRunDisplay;
}

export function WorkersPanelButton() {
	const [open, setOpen] = useState(false);
	const {activeWorkers, activeBranches} = useLiveContext();
	const activeCount = Object.keys(activeWorkers).length + Object.keys(activeBranches).length;

	return (
		<PopoverRoot open={open} onOpenChange={setOpen}>
			<PopoverTrigger asChild>
				<CircleButton
					icon={Queue}
					title="Process activity"
					variant={activeCount > 0 ? "active" : "default"}
				/>
			</PopoverTrigger>
			<PopoverContent
				align="start"
				side="right"
				sideOffset={8}
				collisionPadding={16}
				className="w-[440px] p-0"
			>
				<WorkersPanelContent />
			</PopoverContent>
		</PopoverRoot>
	);
}

export function WorkersPanelContent() {
	const [tab, setTab] = useState<Tab>("history");
	const [search, setSearch] = useState("");
	const [selected, setSelected] = useState<SelectedProcess | null>(null);
	const {activeWorkers, activeBranches, liveTranscripts} = useLiveContext();
	const {data: agentsData} = useQuery({
		queryKey: ["agents"],
		queryFn: api.agents,
		staleTime: 30_000,
	});
	const agents = agentsData?.agents ?? [];
	const agentNames = useMemo(
		() => Object.fromEntries(agents.map((agent) => [agent.id, agent.display_name ?? agent.id])),
		[agents],
	);
	const processQueries = useQueries({
		queries: agents.map((agent) => ({
			queryKey: ["processes-panel", agent.id],
			queryFn: () => api.processesList(agent.id, {limit: 100}),
			staleTime: 10_000,
			refetchInterval: 10_000,
		})),
	});

	const history = useMemo(() => {
		const rows: Array<ProcessRunDisplay & {agentId: string; agentName: string}> = [];
		for (let index = 0; index < agents.length; index++) {
			const agent = agents[index];
			if (!agent) continue;
			for (const process of processQueries[index]?.data?.processes ?? []) {
				rows.push({...process, agentId: agent.id, agentName: agentNames[agent.id] ?? agent.id});
			}
		}
		return rows.sort((left, right) => new Date(right.started_at).getTime() - new Date(left.started_at).getTime());
	}, [agents, agentNames, processQueries]);

	const active = useMemo(() => {
		const rows: Array<ProcessRunDisplay & {agentId: string; agentName: string}> = [];
		for (const worker of Object.values(activeWorkers)) {
			if (!worker.runtimeAttached) continue;
			rows.push({
				kind: "worker",
				id: worker.id,
				input: worker.task,
				status: worker.runtimeState === "waiting_for_input" ? "idle" : "running",
				process_type: worker.workerType,
				started_at: new Date(worker.startedAt).toISOString(),
				tool_calls: worker.toolCalls,
				interactive: worker.interactive,
				agentId: worker.agentId,
				agentName: agentNames[worker.agentId] ?? worker.agentId,
			});
		}
		for (const branch of Object.values(activeBranches)) {
			rows.push({
				kind: "branch",
				id: branch.id,
				input: branch.description,
				status: "running",
				started_at: new Date(branch.startedAt).toISOString(),
				tool_calls: branch.toolCalls,
				agentId: branch.agentId,
				agentName: agentNames[branch.agentId] ?? branch.agentId,
			});
		}
		return rows.sort((left, right) => new Date(right.started_at).getTime() - new Date(left.started_at).getTime());
	}, [activeWorkers, activeBranches, agentNames]);

	const term = search.trim().toLowerCase();
	const rows = (tab === "active" ? active : history).filter((process) =>
		term ? process.input.toLowerCase().includes(term) || process.agentName.toLowerCase().includes(term) : true,
	);

	return (
		<div className="relative h-[540px] overflow-hidden">
			<div className="absolute inset-0 flex flex-col transition-transform duration-200" style={{transform: selected ? "translateX(-100%)" : "translateX(0)"}}>
				<div className="flex items-center gap-2 border-b border-app-line px-3 py-2.5">
					<MagnifyingGlass className="size-3.5 shrink-0 text-ink-faint" />
					<input value={search} onChange={(event) => setSearch(event.target.value)} placeholder="Search branches and workers..." className="flex-1 bg-transparent text-sm text-ink outline-none placeholder:text-ink-faint" />
				</div>
				<div className="flex border-b border-app-line">
					{(["history", "active"] as const).map((value) => (
						<button key={value} type="button" onClick={() => setTab(value)} className={cx("flex-1 py-2 text-xs font-medium capitalize transition-colors", tab === value ? "bg-app-hover/40 text-ink" : "text-ink-faint hover:text-ink-dull")}>{value}{value === "active" && active.length > 0 ? ` · ${active.length}` : ""}</button>
					))}
				</div>
				<div className="flex-1 overflow-y-auto py-1">
					{rows.length === 0 ? <div className="py-12 text-center text-sm text-ink-faint">No {tab} processes</div> : rows.map((process) => {
						const liveWorker = process.kind === "worker" ? activeWorkers[process.id] : undefined;
						const liveBranch = process.kind === "branch" ? activeBranches[process.id] : undefined;
						return <div key={`${process.kind}:${process.id}`}><div className="px-[72px] pt-1 text-[10px] font-medium uppercase tracking-wide text-ink-faint">{process.agentName}</div><ProcessCard kind={process.kind} id={process.id} title={process.input} status={liveWorker ? (liveWorker.runtimeState === "waiting_for_input" ? "idle" : "running") : liveBranch ? "running" : process.status} startedAt={process.started_at} toolCalls={liveWorker?.toolCalls ?? liveBranch?.toolCalls ?? process.tool_calls} currentTool={liveWorker?.currentTool ?? liveBranch?.currentTool} processType={process.process_type} selected={false} onSelect={() => setSelected({kind: process.kind, id: process.id, agentId: process.agentId, fallback: process})} /></div>;
					})}
				</div>
			</div>
			<div className="absolute inset-0 flex flex-col transition-transform duration-200" style={{transform: selected ? "translateX(0)" : "translateX(100%)"}}>
				{selected && <><div className="absolute left-2 top-2 z-10"><CircleButton icon={CaretLeft} title="Back to activity" onClick={() => setSelected(null)} variant="default" /></div><ProcessDetail agentId={selected.agentId} selection={selected} fallback={selected.fallback} liveTranscript={liveTranscripts[selected.id]} onClose={() => setSelected(null)} /></>}
			</div>
		</div>
	);
}
