import { api, type ModelInfo } from "@/api/client";
import { Input } from "@/ui";
import { useQuery } from "@tanstack/react-query";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { ArrowDown01Icon, Search01Icon } from "@hugeicons/core-free-icons";
import { HugeiconsIcon } from "@hugeicons/react";

interface ModelSelectProps {
  label: string;
  description: string;
  value: string;
  onChange: (value: string) => void;
  provider?: string;
  capability?: "input_audio" | "voice_transcription";
}

const PROVIDER_LABELS: Record<string, string> = {
  anthropic: "Anthropic",
  openrouter: "OpenRouter",
  kilo: "Kilo Gateway",
  openai: "OpenAI",
  "openai-chatgpt": "ChatGPT Plus (OAuth)",
  deepseek: "DeepSeek",
  xai: "xAI",
  mistral: "Mistral",
  gemini: "Google Gemini",
  groq: "Groq",
  together: "Together AI",
  fireworks: "Fireworks AI",
  zhipu: "Z.ai (GLM)",
  ollama: "Ollama",
  "opencode-zen": "OpenCode Zen",
  "opencode-go": "OpenCode Go",
  minimax: "MiniMax",
  "minimax-cn": "MiniMax CN",
  "github-copilot": "GitHub Copilot",
};

function formatContextWindow(tokens: number | null): string {
  if (!tokens) return "";
  if (tokens >= 1_000_000) return `${(tokens / 1_000_000).toFixed(1)}M`;
  return `${Math.round(tokens / 1000)}K`;
}

const providerOrder = [
  "openrouter",
  "kilo",
  "anthropic",
  "openai",
  "openai-chatgpt",
  "github-copilot",
  "ollama",
  "deepseek",
  "xai",
  "mistral",
  "gemini",
  "groq",
  "together",
  "fireworks",
  "zhipu",
  "opencode-zen",
  "opencode-go",
  "minimax",
  "minimax-cn",
];

export function ModelSelect({
  label,
  description,
  value,
  onChange,
  provider,
  capability,
}: ModelSelectProps) {
  const [open, setOpen] = useState(false);
  const [filter, setFilter] = useState("");
  const [highlightIndex, setHighlightIndex] = useState(-1);
  const containerRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLInputElement>(null);
  const listRef = useRef<HTMLDivElement>(null);

  const { data, isLoading, isError } = useQuery({
    queryKey: ["models", provider ?? "configured", capability ?? "all"],
    queryFn: () => api.models(provider, capability),
    staleTime: 60_000,
  });

  const models = data?.models ?? [];

  // Filter and group models
  const filtered = useMemo(() => {
    const query = filter.toLowerCase();
    if (!query) return models;
    return models.filter(
      (m) =>
        m.id.toLowerCase().includes(query) ||
        m.name.toLowerCase().includes(query) ||
        m.provider.toLowerCase().includes(query),
    );
  }, [models, filter]);

  const providerRank = (p: string) => {
    const index = providerOrder.indexOf(p);
    return index === -1 ? Number.MAX_SAFE_INTEGER : index;
  };

  const grouped = useMemo(() => {
    const groups: Record<string, ModelInfo[]> = {};
    for (const model of filtered) {
      const key = model.provider;
      if (!groups[key]) groups[key] = [];
      groups[key].push(model);
    }
    for (const key of Object.keys(groups)) {
      groups[key].sort((a, b) => a.name.localeCompare(b.name));
    }
    return groups;
  }, [filtered]);

  const sortedProviders = useMemo(
    () => Object.keys(grouped).sort((a, b) => providerRank(a) - providerRank(b)),
    [grouped],
  );

  // Flat list for keyboard navigation
  const flatList = useMemo(() => {
    const items: ModelInfo[] = [];
    for (const p of sortedProviders) {
      for (const m of grouped[p]) {
        items.push(m);
      }
    }
    return items;
  }, [sortedProviders, grouped]);

  // Find display name for current value
  const selectedModel = useMemo(
    () => models.find((m) => m.id === value),
    [models, value],
  );

  // Close on outside click
  useEffect(() => {
    const handler = (e: MouseEvent) => {
      if (
        containerRef.current &&
        !containerRef.current.contains(e.target as Node)
      ) {
        setOpen(false);
        setFilter("");
        setHighlightIndex(-1);
      }
    };
    document.addEventListener("mousedown", handler);
    return () => document.removeEventListener("mousedown", handler);
  }, []);

  // Scroll highlighted item into view
  useEffect(() => {
    if (highlightIndex < 0 || !listRef.current) return;
    const items = listRef.current.querySelectorAll("[data-model-item]");
    items[highlightIndex]?.scrollIntoView({ block: "nearest" });
  }, [highlightIndex]);

  const handleSelect = useCallback(
    (modelId: string) => {
      onChange(modelId);
      setOpen(false);
      setFilter("");
      setHighlightIndex(-1);
    },
    [onChange],
  );

  const handleInputChange = (e: React.ChangeEvent<HTMLInputElement>) => {
    const val = e.target.value;
    setFilter(val);
    setHighlightIndex(-1);
    if (!open) setOpen(true);
    // Allow free-form input for custom model IDs
    onChange(val);
  };

  const handleFocus = () => {
    setOpen(true);
    setFilter(value);
    setHighlightIndex(-1);
  };

  const handleKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === "Escape") {
      setOpen(false);
      setFilter("");
      setHighlightIndex(-1);
      inputRef.current?.blur();
      return;
    }
    if (e.key === "ArrowDown") {
      e.preventDefault();
      if (!open) {
        setOpen(true);
        setFilter(value);
      }
      setHighlightIndex((prev) =>
        prev < flatList.length - 1 ? prev + 1 : 0,
      );
      return;
    }
    if (e.key === "ArrowUp") {
      e.preventDefault();
      setHighlightIndex((prev) =>
        prev > 0 ? prev - 1 : flatList.length - 1,
      );
      return;
    }
    if (e.key === "Enter") {
      e.preventDefault();
      if (highlightIndex >= 0 && highlightIndex < flatList.length) {
        handleSelect(flatList[highlightIndex].id);
        inputRef.current?.blur();
      }
    }
  };

  return (
    <div className="flex flex-col gap-1.5" ref={containerRef}>
      <label className="text-sm font-medium text-ink">{label}</label>
      <p className="text-tiny text-ink-faint">{description}</p>
      <div className="relative mt-1">
        <Input
          ref={inputRef}
          type="text"
          value={open ? filter : value}
          onChange={handleInputChange}
          onFocus={handleFocus}
          onKeyDown={handleKeyDown}
          placeholder="Search models..."
          className="border-app-line/50 bg-app-darkBox/30"
          icon={
            open ? (
              <HugeiconsIcon icon={Search01Icon} size={14} className="text-ink-faint" />
            ) : undefined
          }
          right={
            <button
              type="button"
              tabIndex={-1}
              className="flex items-center text-ink-faint hover:text-ink transition-colors"
              onMouseDown={(e) => {
                e.preventDefault();
                if (open) {
                  setOpen(false);
                  setFilter("");
                  setHighlightIndex(-1);
                  inputRef.current?.blur();
                } else {
                  inputRef.current?.focus();
                }
              }}
            >
              <HugeiconsIcon
                icon={ArrowDown01Icon}
                size={16}
                className={`transition-transform ${open ? "rotate-180" : ""}`}
              />
            </button>
          }
        />
        {/* Selected model badge (shown when closed and a known model is selected) */}
        {!open && selectedModel && selectedModel.id === value && (
          <div className="mt-1 flex items-center gap-1.5 text-xs text-ink-dull">
            <span className="text-ink-faint">
              {PROVIDER_LABELS[selectedModel.provider] ?? selectedModel.provider}
            </span>
            <span className="text-ink-faint/50">/</span>
            <span>{selectedModel.name}</span>
            {selectedModel.context_window && (
              <>
                <span className="text-ink-faint/50">·</span>
                <span className="text-ink-faint">
                  {formatContextWindow(selectedModel.context_window)}
                </span>
              </>
            )}
          </div>
        )}
        {open && (
          <div
            ref={listRef}
            className="absolute z-50 mt-1 w-full max-h-72 overflow-y-auto rounded-md border border-app-line bg-app-box shadow-lg"
          >
            {isLoading ? (
              <div className="px-3 py-4 text-center text-sm text-ink-faint">
                <div className="flex items-center justify-center gap-2">
                  <div className="h-2 w-2 animate-pulse rounded-full bg-accent" />
                  Loading models...
                </div>
              </div>
            ) : filtered.length === 0 ? (
              <div className="px-3 py-4 text-center text-sm text-ink-faint">
                {isError
                  ? "Failed to load models — check your connection"
                  : models.length === 0
                    ? "No models available — configure a provider first"
                    : "No models match your search"}
              </div>
            ) : (
              sortedProviders.map((prov) => (
                <div key={prov}>
                  <div className="sticky top-0 bg-app-box/95 backdrop-blur-sm px-3 py-1.5 text-xs font-semibold text-ink-dull border-b border-app-line/30">
                    {PROVIDER_LABELS[prov] ?? prov}
                  </div>
                  {grouped[prov].map((model) => {
                    const flatIdx = flatList.indexOf(model);
                    const isHighlighted = flatIdx === highlightIndex;
                    const isSelected = model.id === value;
                    return (
                      <button
                        key={model.id}
                        type="button"
                        data-model-item
                        className={`w-full text-left px-3 py-1.5 text-sm transition-colors flex items-center justify-between gap-2 ${
                          isHighlighted
                            ? "bg-accent/20 text-ink"
                            : isSelected
                              ? "bg-app-selected/50 text-ink"
                              : "text-ink hover:bg-app-selected"
                        }`}
                        onMouseDown={(e) => {
                          e.preventDefault();
                          handleSelect(model.id);
                        }}
                        onMouseEnter={() => setHighlightIndex(flatIdx)}
                      >
                        <div className="flex flex-col min-w-0">
                          <span className="truncate font-medium">
                            {model.name}
                          </span>
                          <span className="text-xs text-ink-faint truncate">
                            {model.id}
                          </span>
                        </div>
                        <div className="flex items-center gap-1.5 shrink-0">
                          {isSelected && (
                            <span className="text-[10px] px-1 py-0.5 rounded bg-accent/20 text-accent font-medium">
                              active
                            </span>
                          )}
                          {model.context_window && (
                            <span className="text-xs text-ink-faint">
                              {formatContextWindow(model.context_window)}
                            </span>
                          )}
                          {model.tool_call && (
                            <span
                              className="text-[10px] px-1 py-0.5 rounded bg-accent/15 text-accent font-medium"
                              title="Tool calling"
                            >
                              tools
                            </span>
                          )}
                          {model.reasoning && (
                            <span
                              className="text-[10px] px-1 py-0.5 rounded bg-purple-500/15 text-purple-400 font-medium"
                              title="Reasoning"
                            >
                              think
                            </span>
                          )}
                        </div>
                      </button>
                    );
                  })}
                </div>
              ))
            )}
          </div>
        )}
      </div>
    </div>
  );
}
