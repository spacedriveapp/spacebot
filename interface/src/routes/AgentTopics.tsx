import { useCallback, useState } from "react";
import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import {
  api,
  type TopicItem,
  type TopicStatus,
  type TopicVersion,
  type CreateTopicRequest,
  type UpdateTopicRequest,
  type TopicCriteria,
  type MemoryItem,
  MEMORY_TYPES,
  type MemoryType,
} from "@/api/client";
import { Badge } from "@/ui/Badge";
import { Button } from "@/ui/Button";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogFooter,
} from "@/ui/Dialog";
import { Markdown } from "@/components/Markdown";
import { formatTimeAgo } from "@/lib/format";

const STATUS_COLORS: Record<
  TopicStatus,
  "green" | "amber" | "default"
> = {
  active: "green",
  paused: "amber",
  archived: "default",
};

const MAX_AGE_OPTIONS = [
  { value: "", label: "All time" },
  { value: "7d", label: "7 days" },
  { value: "30d", label: "30 days" },
  { value: "90d", label: "90 days" },
];

export function AgentTopics({ agentId }: { agentId: string }) {
  const queryClient = useQueryClient();

  const { data, isLoading } = useQuery({
    queryKey: ["topics", agentId],
    queryFn: () => api.listTopics(agentId),
    refetchInterval: 15_000,
  });

  const topics = data?.topics ?? [];

  const [createOpen, setCreateOpen] = useState(false);
  const [selectedTopicId, setSelectedTopicId] = useState<string | null>(null);
  const selectedTopic = selectedTopicId
    ? (topics.find((t) => t.id === selectedTopicId) ?? null)
    : null;

  const createMutation = useMutation({
    mutationFn: (request: CreateTopicRequest) =>
      api.createTopic(agentId, request),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["topics", agentId] });
      setCreateOpen(false);
    },
  });

  const updateMutation = useMutation({
    mutationFn: ({
      topicId,
      ...request
    }: UpdateTopicRequest & { topicId: string }) =>
      api.updateTopic(agentId, topicId, request),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["topics", agentId] });
    },
  });

  const deleteMutation = useMutation({
    mutationFn: (topicId: string) => api.deleteTopic(agentId, topicId),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["topics", agentId] });
      setSelectedTopicId(null);
    },
  });

  const syncMutation = useMutation({
    mutationFn: (topicId: string) => api.syncTopic(agentId, topicId),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["topics", agentId] });
    },
  });

  if (isLoading) {
    return (
      <div className="flex h-full items-center justify-center text-ink-faint">
        Loading topics...
      </div>
    );
  }

  if (topics.length === 0 && !createOpen) {
    return (
      <div className="flex h-full flex-col items-center justify-center gap-4 text-ink-faint">
        <p className="text-sm">No topics yet</p>
        <p className="max-w-md text-center text-xs">
          Topics are living context documents synthesized from memories. They
          replace the global bulletin with targeted, per-channel context.
        </p>
        <Button size="sm" onClick={() => setCreateOpen(true)}>
          Create Topic
        </Button>
        <CreateTopicDialog
          open={createOpen}
          onClose={() => setCreateOpen(false)}
          onCreate={(request) => createMutation.mutate(request)}
          isPending={createMutation.isPending}
        />
      </div>
    );
  }

  return (
    <div className="flex h-full flex-col">
      {/* Toolbar */}
      <div className="flex items-center justify-between border-b border-app-line px-4 py-2">
        <div className="flex items-center gap-3">
          <span className="text-sm text-ink-dull">
            {topics.length} topic{topics.length !== 1 ? "s" : ""}
          </span>
          {topics.filter((t) => t.status === "active").length > 0 && (
            <Badge variant="green" size="sm">
              {topics.filter((t) => t.status === "active").length} active
            </Badge>
          )}
        </div>
        <Button size="sm" onClick={() => setCreateOpen(true)}>
          New Topic
        </Button>
      </div>

      {/* Card Grid */}
      <div className="flex-1 overflow-y-auto p-4">
        <div className="grid grid-cols-1 gap-3 sm:grid-cols-2 lg:grid-cols-3">
          {topics.map((topic) => (
            <TopicCard
              key={topic.id}
              topic={topic}
              onSelect={() => setSelectedTopicId(topic.id)}
              onSync={() => syncMutation.mutate(topic.id)}
            />
          ))}
        </div>
      </div>

      {/* Create Dialog */}
      <CreateTopicDialog
        open={createOpen}
        onClose={() => setCreateOpen(false)}
        onCreate={(request) => createMutation.mutate(request)}
        isPending={createMutation.isPending}
      />

      {/* Detail Dialog */}
      {selectedTopic && (
        <TopicDetailDialog
          agentId={agentId}
          topic={selectedTopic}
          onClose={() => setSelectedTopicId(null)}
          onUpdate={(request) =>
            updateMutation.mutate({
              topicId: selectedTopic.id,
              ...request,
            })
          }
          onDelete={() => deleteMutation.mutate(selectedTopic.id)}
          onSync={() => syncMutation.mutate(selectedTopic.id)}
          isSyncing={syncMutation.isPending}
        />
      )}
    </div>
  );
}

// -- Topic Card --

function TopicCard({
  topic,
  onSelect,
  onSync,
}: {
  topic: TopicItem;
  onSelect: () => void;
  onSync: () => void;
}) {
  const wordCount = topic.content
    ? topic.content.split(/\s+/).filter(Boolean).length
    : 0;

  return (
    <div
      className="cursor-pointer rounded-lg border border-app-line/50 bg-app-darkBox/30 p-4 transition-colors hover:border-app-line"
      onClick={onSelect}
    >
      {/* Header */}
      <div className="flex items-start justify-between gap-2">
        <h3 className="text-sm font-medium text-ink leading-tight">
          {topic.title}
        </h3>
        <Badge variant={STATUS_COLORS[topic.status]} size="sm">
          {topic.status}
        </Badge>
      </div>

      {/* Content preview */}
      {topic.content && (
        <p className="mt-2 line-clamp-2 text-xs text-ink-dull">
          {topic.content.slice(0, 200)}
        </p>
      )}

      {/* Stats row */}
      <div className="mt-3 flex flex-wrap items-center gap-2 text-tiny text-ink-faint">
        {topic.last_synced_at && (
          <span>Synced {formatTimeAgo(topic.last_synced_at)}</span>
        )}
        {!topic.last_synced_at && <span>Never synced</span>}
        {wordCount > 0 && <span>{wordCount} words</span>}
      </div>

      {/* Quick actions */}
      <div className="mt-2 flex gap-1" onClick={(e) => e.stopPropagation()}>
        <button
          className="rounded px-1.5 py-0.5 text-tiny text-accent hover:bg-accent/10"
          onClick={onSync}
        >
          Sync Now
        </button>
      </div>
    </div>
  );
}

// -- Create Topic Dialog --

function CreateTopicDialog({
  open,
  onClose,
  onCreate,
  isPending,
}: {
  open: boolean;
  onClose: () => void;
  onCreate: (request: CreateTopicRequest) => void;
  isPending: boolean;
}) {
  const [title, setTitle] = useState("");
  const [query, setQuery] = useState("");
  const [selectedTypes, setSelectedTypes] = useState<MemoryType[]>([]);
  const [maxAge, setMaxAge] = useState("");
  const [maxWords, setMaxWords] = useState(1500);

  const handleSubmit = useCallback(() => {
    if (!title.trim()) return;
    const criteria: TopicCriteria = {};
    if (query.trim()) criteria.query = query.trim();
    if (selectedTypes.length > 0) criteria.memory_types = selectedTypes;
    if (maxAge) criteria.max_age = maxAge;
    onCreate({
      title: title.trim(),
      criteria: Object.keys(criteria).length > 0 ? criteria : undefined,
      max_words: maxWords,
    });
    setTitle("");
    setQuery("");
    setSelectedTypes([]);
    setMaxAge("");
    setMaxWords(1500);
  }, [title, query, selectedTypes, maxAge, maxWords, onCreate]);

  return (
    <Dialog open={open} onOpenChange={(v) => !v && onClose()}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>Create Topic</DialogTitle>
        </DialogHeader>
        <div className="flex flex-col gap-3 py-2">
          <div>
            <label className="mb-1 block text-xs text-ink-dull">Title</label>
            <input
              className="w-full rounded-md border border-app-line bg-app-darkBox px-3 py-2 text-sm text-ink placeholder:text-ink-faint focus:border-accent focus:outline-none"
              placeholder="e.g. Project Architecture"
              value={title}
              onChange={(e) => setTitle(e.target.value)}
              onKeyDown={(e) => e.key === "Enter" && handleSubmit()}
              autoFocus
            />
          </div>
          <div>
            <label className="mb-1 block text-xs text-ink-dull">
              Semantic Query
            </label>
            <input
              className="w-full rounded-md border border-app-line bg-app-darkBox px-3 py-2 text-sm text-ink placeholder:text-ink-faint focus:border-accent focus:outline-none"
              placeholder="Optional query for memory search..."
              value={query}
              onChange={(e) => setQuery(e.target.value)}
            />
          </div>
          <div>
            <label className="mb-1 block text-xs text-ink-dull">
              Memory Types
            </label>
            <div className="flex flex-wrap gap-1.5">
              {MEMORY_TYPES.map((type) => (
                <button
                  key={type}
                  className={`rounded-full px-2 py-0.5 text-tiny transition-colors ${
                    selectedTypes.includes(type)
                      ? "bg-accent/20 text-accent"
                      : "bg-app-darkBox text-ink-faint hover:text-ink-dull"
                  }`}
                  onClick={() =>
                    setSelectedTypes((prev) =>
                      prev.includes(type)
                        ? prev.filter((t) => t !== type)
                        : [...prev, type],
                    )
                  }
                >
                  {type}
                </button>
              ))}
            </div>
          </div>
          <div className="flex gap-4">
            <div className="flex-1">
              <label className="mb-1 block text-xs text-ink-dull">
                Max Age
              </label>
              <select
                className="w-full rounded-md border border-app-line bg-app-darkBox px-3 py-2 text-sm text-ink focus:border-accent focus:outline-none"
                value={maxAge}
                onChange={(e) => setMaxAge(e.target.value)}
              >
                {MAX_AGE_OPTIONS.map((opt) => (
                  <option key={opt.value} value={opt.value}>
                    {opt.label}
                  </option>
                ))}
              </select>
            </div>
            <div className="flex-1">
              <label className="mb-1 block text-xs text-ink-dull">
                Max Words
              </label>
              <input
                type="number"
                className="w-full rounded-md border border-app-line bg-app-darkBox px-3 py-2 text-sm text-ink focus:border-accent focus:outline-none"
                value={maxWords}
                onChange={(e) => setMaxWords(Number(e.target.value))}
                min={100}
                max={10000}
              />
            </div>
          </div>
        </div>
        <DialogFooter>
          <Button variant="outline" size="sm" onClick={onClose}>
            Cancel
          </Button>
          <Button
            size="sm"
            onClick={handleSubmit}
            disabled={!title.trim() || isPending}
          >
            {isPending ? "Creating..." : "Create"}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

// -- Topic Detail Dialog --

function TopicDetailDialog({
  agentId,
  topic,
  onClose,
  onUpdate,
  onDelete,
  onSync,
  isSyncing,
}: {
  agentId: string;
  topic: TopicItem;
  onClose: () => void;
  onUpdate: (request: UpdateTopicRequest) => void;
  onDelete: () => void;
  onSync: () => void;
  isSyncing: boolean;
}) {
  const [activeTab, setActiveTab] = useState<"content" | "config" | "versions">(
    "content",
  );

  const wordCount = topic.content
    ? topic.content.split(/\s+/).filter(Boolean).length
    : 0;

  return (
    <Dialog open={true} onOpenChange={(v) => !v && onClose()}>
      <DialogContent className="!flex max-h-[85vh] max-w-3xl !flex-col overflow-hidden">
        <DialogHeader className="shrink-0">
          <div className="flex items-center gap-3">
            <DialogTitle>{topic.title}</DialogTitle>
            <Badge variant={STATUS_COLORS[topic.status]} size="sm">
              {topic.status}
            </Badge>
          </div>
        </DialogHeader>

        {/* Tabs */}
        <div className="flex gap-1 border-b border-app-line pb-0">
          {(["content", "config", "versions"] as const).map((tab) => (
            <button
              key={tab}
              className={`relative px-3 py-1.5 text-xs capitalize transition-colors ${
                activeTab === tab
                  ? "text-ink"
                  : "text-ink-faint hover:text-ink-dull"
              }`}
              onClick={() => setActiveTab(tab)}
            >
              {tab}
              {activeTab === tab && (
                <span className="absolute bottom-0 left-0 right-0 h-px bg-accent" />
              )}
            </button>
          ))}
        </div>

        {/* Tab content */}
        <div className="min-h-0 flex-1 overflow-y-auto py-2">
          {activeTab === "content" && (
            <ContentTab
              topic={topic}
              wordCount={wordCount}
              onSync={onSync}
              isSyncing={isSyncing}
            />
          )}
          {activeTab === "config" && (
            <ConfigTab
              agentId={agentId}
              topic={topic}
              onUpdate={onUpdate}
            />
          )}
          {activeTab === "versions" && (
            <VersionsTab agentId={agentId} topicId={topic.id} />
          )}
        </div>

        <DialogFooter className="shrink-0">
          <div className="flex w-full items-center justify-between">
            <button
              className="text-xs text-red-400 hover:text-red-300"
              onClick={onDelete}
            >
              Delete
            </button>
            <Button size="sm" variant="outline" onClick={onClose}>
              Close
            </Button>
          </div>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

// -- Content Tab --

function ContentTab({
  topic,
  wordCount,
  onSync,
  isSyncing,
}: {
  topic: TopicItem;
  wordCount: number;
  onSync: () => void;
  isSyncing: boolean;
}) {
  return (
    <div className="flex flex-col gap-3">
      {/* Meta bar */}
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-3 text-xs text-ink-faint">
          <span>{wordCount} words</span>
          {topic.last_synced_at && (
            <span>Last synced {formatTimeAgo(topic.last_synced_at)}</span>
          )}
        </div>
        <Button
          size="sm"
          variant="outline"
          onClick={onSync}
          disabled={isSyncing}
        >
          {isSyncing ? "Syncing..." : "Sync Now"}
        </Button>
      </div>

      {/* Content */}
      {topic.content ? (
        <div className="rounded-lg border border-app-line/50 bg-app-darkBox/20 p-4">
          <Markdown className="text-sm text-ink">{topic.content}</Markdown>
        </div>
      ) : (
        <div className="flex items-center justify-center rounded-lg border border-dashed border-app-line/50 p-8 text-sm text-ink-faint">
          No content yet. Click "Sync Now" to generate.
        </div>
      )}
    </div>
  );
}

// -- Config Tab --

function ConfigTab({
  agentId,
  topic,
  onUpdate,
}: {
  agentId: string;
  topic: TopicItem;
  onUpdate: (request: UpdateTopicRequest) => void;
}) {
  const [title, setTitle] = useState(topic.title);
  const [status, setStatus] = useState<TopicStatus>(topic.status);
  const [query, setQuery] = useState(topic.criteria.query ?? "");
  const [selectedTypes, setSelectedTypes] = useState<MemoryType[]>(
    topic.criteria.memory_types ?? [],
  );
  const [minImportance, setMinImportance] = useState(
    topic.criteria.min_importance ?? 0,
  );
  const [maxAge, setMaxAge] = useState(topic.criteria.max_age ?? "");
  const [maxMemories, setMaxMemories] = useState(
    topic.criteria.max_memories ?? 30,
  );
  const [maxWords, setMaxWords] = useState(topic.max_words);
  const [pinIds, setPinIds] = useState<string[]>(topic.pin_ids);
  const [pinSearch, setPinSearch] = useState("");
  const [pinResults, setPinResults] = useState<MemoryItem[]>([]);
  const [isSearching, setIsSearching] = useState(false);

  const handlePinSearch = useCallback(async () => {
    if (!pinSearch.trim()) {
      setPinResults([]);
      return;
    }
    setIsSearching(true);
    try {
      const response = await api.searchMemories(agentId, pinSearch.trim(), {
        limit: 10,
      });
      setPinResults(response.results.map((r) => r.memory));
    } catch {
      setPinResults([]);
    } finally {
      setIsSearching(false);
    }
  }, [agentId, pinSearch]);

  const hasChanges =
    title !== topic.title ||
    status !== topic.status ||
    query !== (topic.criteria.query ?? "") ||
    JSON.stringify(selectedTypes) !==
      JSON.stringify(topic.criteria.memory_types ?? []) ||
    minImportance !== (topic.criteria.min_importance ?? 0) ||
    maxAge !== (topic.criteria.max_age ?? "") ||
    maxMemories !== (topic.criteria.max_memories ?? 30) ||
    maxWords !== topic.max_words ||
    JSON.stringify(pinIds.slice().sort()) !==
      JSON.stringify(topic.pin_ids.slice().sort());

  const handleSave = () => {
    const criteria: TopicCriteria = {
      max_memories: maxMemories,
    };
    if (query.trim()) criteria.query = query.trim();
    if (selectedTypes.length > 0) criteria.memory_types = selectedTypes;
    if (minImportance > 0) criteria.min_importance = minImportance;
    if (maxAge) criteria.max_age = maxAge;

    onUpdate({
      title: title !== topic.title ? title : undefined,
      status: status !== topic.status ? status : undefined,
      criteria,
      max_words: maxWords !== topic.max_words ? maxWords : undefined,
      pin_ids: pinIds,
    });
  };

  return (
    <div className="flex flex-col gap-4">
      {/* Title */}
      <div>
        <label className="mb-1 block text-xs text-ink-dull">Title</label>
        <input
          className="w-full rounded-md border border-app-line bg-app-darkBox px-3 py-2 text-sm text-ink focus:border-accent focus:outline-none"
          value={title}
          onChange={(e) => setTitle(e.target.value)}
        />
      </div>

      {/* Status */}
      <div>
        <label className="mb-1 block text-xs text-ink-dull">Status</label>
        <div className="flex gap-2">
          {(["active", "paused", "archived"] as TopicStatus[]).map((s) => (
            <button
              key={s}
              className={`rounded-md px-3 py-1.5 text-xs capitalize transition-colors ${
                status === s
                  ? "bg-accent/20 text-accent"
                  : "bg-app-darkBox text-ink-faint hover:text-ink-dull"
              }`}
              onClick={() => setStatus(s)}
            >
              {s}
            </button>
          ))}
        </div>
      </div>

      {/* Semantic Query */}
      <div>
        <label className="mb-1 block text-xs text-ink-dull">
          Semantic Query
        </label>
        <input
          className="w-full rounded-md border border-app-line bg-app-darkBox px-3 py-2 text-sm text-ink placeholder:text-ink-faint focus:border-accent focus:outline-none"
          placeholder="Search query for memory retrieval..."
          value={query}
          onChange={(e) => setQuery(e.target.value)}
        />
      </div>

      {/* Memory Types */}
      <div>
        <label className="mb-1 block text-xs text-ink-dull">
          Memory Types
        </label>
        <div className="flex flex-wrap gap-1.5">
          {MEMORY_TYPES.map((type) => (
            <button
              key={type}
              className={`rounded-full px-2 py-0.5 text-tiny transition-colors ${
                selectedTypes.includes(type)
                  ? "bg-accent/20 text-accent"
                  : "bg-app-darkBox text-ink-faint hover:text-ink-dull"
              }`}
              onClick={() =>
                setSelectedTypes((prev) =>
                  prev.includes(type)
                    ? prev.filter((t) => t !== type)
                    : [...prev, type],
                )
              }
            >
              {type}
            </button>
          ))}
        </div>
      </div>

      {/* Min Importance */}
      <div>
        <label className="mb-1 block text-xs text-ink-dull">
          Min Importance ({minImportance.toFixed(1)})
        </label>
        <input
          type="range"
          min="0"
          max="1"
          step="0.1"
          value={minImportance}
          onChange={(e) => setMinImportance(Number(e.target.value))}
          className="w-full accent-accent"
        />
      </div>

      {/* Max Age + Max Memories + Max Words */}
      <div className="flex gap-4">
        <div className="flex-1">
          <label className="mb-1 block text-xs text-ink-dull">Max Age</label>
          <select
            className="w-full rounded-md border border-app-line bg-app-darkBox px-3 py-2 text-sm text-ink focus:border-accent focus:outline-none"
            value={maxAge}
            onChange={(e) => setMaxAge(e.target.value)}
          >
            {MAX_AGE_OPTIONS.map((opt) => (
              <option key={opt.value} value={opt.value}>
                {opt.label}
              </option>
            ))}
          </select>
        </div>
        <div className="flex-1">
          <label className="mb-1 block text-xs text-ink-dull">
            Max Memories
          </label>
          <input
            type="number"
            className="w-full rounded-md border border-app-line bg-app-darkBox px-3 py-2 text-sm text-ink focus:border-accent focus:outline-none"
            value={maxMemories}
            onChange={(e) => setMaxMemories(Number(e.target.value))}
            min={5}
            max={100}
          />
        </div>
        <div className="flex-1">
          <label className="mb-1 block text-xs text-ink-dull">Max Words</label>
          <input
            type="number"
            className="w-full rounded-md border border-app-line bg-app-darkBox px-3 py-2 text-sm text-ink focus:border-accent focus:outline-none"
            value={maxWords}
            onChange={(e) => setMaxWords(Number(e.target.value))}
            min={100}
            max={10000}
          />
        </div>
      </div>

      {/* Pinned Memories */}
      <div>
        <label className="mb-1 block text-xs text-ink-dull">
          Pinned Memories ({pinIds.length})
        </label>
        <p className="mb-2 text-tiny text-ink-faint">
          Pinned memories are always included in the topic synthesis,
          regardless of search criteria.
        </p>

        {/* Current pins */}
        {pinIds.length > 0 && (
          <div className="mb-2 space-y-1 rounded-md border border-app-line/50 bg-app-darkBox/30 p-2">
            {pinIds.map((pinId) => (
              <div
                key={pinId}
                className="flex items-center justify-between rounded px-1.5 py-1 text-xs"
              >
                <span className="truncate font-mono text-tiny text-ink-dull">
                  {pinId}
                </span>
                <button
                  className="ml-2 shrink-0 text-red-400 hover:text-red-300"
                  onClick={() =>
                    setPinIds((prev) => prev.filter((id) => id !== pinId))
                  }
                >
                  Remove
                </button>
              </div>
            ))}
          </div>
        )}

        {/* Search to add pins */}
        <div className="flex gap-2">
          <input
            className="flex-1 rounded-md border border-app-line bg-app-darkBox px-3 py-1.5 text-xs text-ink placeholder:text-ink-faint focus:border-accent focus:outline-none"
            placeholder="Search memories to pin..."
            value={pinSearch}
            onChange={(e) => setPinSearch(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && handlePinSearch()}
          />
          <Button
            size="sm"
            variant="outline"
            onClick={handlePinSearch}
            disabled={isSearching || !pinSearch.trim()}
          >
            {isSearching ? "..." : "Search"}
          </Button>
        </div>

        {/* Search results */}
        {pinResults.length > 0 && (
          <div className="mt-2 max-h-48 space-y-1 overflow-y-auto rounded-md border border-app-line/50 bg-app-darkBox/30 p-2">
            {pinResults
              .filter((memory) => !pinIds.includes(memory.id))
              .map((memory) => (
                <button
                  key={memory.id}
                  className="flex w-full items-start gap-2 rounded px-1.5 py-1.5 text-left text-xs transition-colors hover:bg-app-darkBox"
                  onClick={() => {
                    setPinIds((prev) => [...prev, memory.id]);
                    setPinResults((prev) =>
                      prev.filter((m) => m.id !== memory.id),
                    );
                  }}
                >
                  <Badge
                    variant="default"
                    size="sm"
                    className="shrink-0 capitalize"
                  >
                    {memory.memory_type}
                  </Badge>
                  <span className="line-clamp-2 text-ink-dull">
                    {memory.content}
                  </span>
                </button>
              ))}
          </div>
        )}
      </div>

      {/* Save */}
      {hasChanges && (
        <div className="flex justify-end">
          <Button size="sm" onClick={handleSave}>
            Save Changes
          </Button>
        </div>
      )}
    </div>
  );
}

// -- Versions Tab --

function VersionsTab({
  agentId,
  topicId,
}: {
  agentId: string;
  topicId: string;
}) {
  const { data, isLoading } = useQuery({
    queryKey: ["topic-versions", agentId, topicId],
    queryFn: () => api.topicVersions(agentId, topicId, 50),
  });

  const versions = data?.versions ?? [];
  const [selectedVersion, setSelectedVersion] = useState<TopicVersion | null>(
    null,
  );

  if (isLoading) {
    return (
      <div className="flex items-center justify-center py-8 text-xs text-ink-faint">
        Loading versions...
      </div>
    );
  }

  if (versions.length === 0) {
    return (
      <div className="flex items-center justify-center py-8 text-xs text-ink-faint">
        No versions yet. Sync the topic to create the first version.
      </div>
    );
  }

  return (
    <div className="flex flex-col gap-3">
      {/* Version list */}
      <div className="space-y-1">
        {versions.map((version, index) => (
          <button
            key={version.id}
            className={`flex w-full items-center justify-between rounded-md px-3 py-2 text-left text-xs transition-colors ${
              selectedVersion?.id === version.id
                ? "bg-accent/10 text-ink"
                : "text-ink-dull hover:bg-app-darkBox"
            }`}
            onClick={() =>
              setSelectedVersion(
                selectedVersion?.id === version.id ? null : version,
              )
            }
          >
            <div className="flex items-center gap-2">
              <span className="text-ink-faint">v{versions.length - index}</span>
              <span>{formatTimeAgo(version.created_at)}</span>
            </div>
            <span className="text-ink-faint">
              {version.memory_count} memories
            </span>
          </button>
        ))}
      </div>

      {/* Selected version content */}
      {selectedVersion && (
        <div className="rounded-lg border border-app-line/50 bg-app-darkBox/20 p-4">
          <div className="mb-2 flex items-center gap-2 text-xs text-ink-faint">
            <span>{formatTimeAgo(selectedVersion.created_at)}</span>
            <span>{selectedVersion.memory_count} memories</span>
            <span>
              {selectedVersion.content.split(/\s+/).filter(Boolean).length}{" "}
              words
            </span>
          </div>
          <Markdown className="text-sm text-ink">
            {selectedVersion.content}
          </Markdown>
        </div>
      )}
    </div>
  );
}
