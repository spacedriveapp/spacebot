import { api, type ModelInfo } from "@/api/client";
import { Input } from "@/ui";
import { useQuery } from "@tanstack/react-query";
import { useEffect, useMemo, useRef, useState } from "react";

interface ModelSelectProps {
  label: string;
  description: string;
  value: string;
  onChange: (value: string) => void;
}

const PROVIDER_LABELS: Record<string, string> = {
  anthropic: "Anthropic",
  openrouter: "OpenRouter",
  openai: "OpenAI",
  deepseek: "DeepSeek",
  xai: "xAI",
  mistral: "Mistral",
  groq: "Groq",
  together: "Together AI",
  fireworks: "Fireworks AI",
  zhipu: "Z.ai (GLM)",
  ollama: "Ollama",
  "opencode-zen": "OpenCode Zen",
};

function formatContextWindow(tokens: number | null): string {
  if (!tokens) return "";
  if (tokens >= 1_000_000) return `${(tokens / 1_000_000).toFixed(1)}M`;
  return `${Math.round(tokens / 1000)}K`;
}

export function ModelSelect({
  label,
  description,
  value,
  onChange,
}: ModelSelectProps) {
  const [open, setOpen] = useState(false);
  const [filter, setFilter] = useState("");
  const containerRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLInputElement>(null);

  const { data } = useQuery({
    queryKey: ["models"],
    queryFn: api.models,
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

  // Close on outside click
  useEffect(() => {
    const handler = (e: MouseEvent) => {
      if (
        containerRef.current &&
        !containerRef.current.contains(e.target as Node)
      ) {
        setOpen(false);
        setFilter("");
      }
    };
    document.addEventListener("mousedown", handler);
    return () => document.removeEventListener("mousedown", handler);
  }, []);

  const handleSelect = (modelId: string) => {
    onChange(modelId);
    setOpen(false);
    setFilter("");
  };

  const handleInputChange = (e: React.ChangeEvent<HTMLInputElement>) => {
    const val = e.target.value;
    setFilter(val);
    if (!open) setOpen(true);
    // Allow free-form input for custom model IDs
    onChange(val);
  };

  const handleFocus = () => {
    setOpen(true);
    // Start filtering from current value
    setFilter(value);
  };

  const handleKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === "Escape") {
      setOpen(false);
      setFilter("");
      inputRef.current?.blur();
    }
  };

  const providerOrder = [
    "openrouter",
    "anthropic",
    "openai",
    "ollama",
    "deepseek",
    "xai",
    "mistral",
    "groq",
    "together",
    "fireworks",
    "zhipu",
    "opencode-zen",
  ];
  const sortedProviders = Object.keys(grouped).sort(
    (a, b) =>
      (providerOrder.indexOf(a) ?? 99) - (providerOrder.indexOf(b) ?? 99),
  );

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
          placeholder="Type to search models..."
          className="border-app-line/50 bg-app-darkBox/30"
        />
        {open && filtered.length > 0 && (
          <div className="absolute z-50 mt-1 w-full max-h-72 overflow-y-auto rounded-md border border-app-line bg-app-box shadow-lg">
            {sortedProviders.map((provider) => (
              <div key={provider}>
                <div className="sticky top-0 bg-app-box/95 backdrop-blur-sm px-3 py-1.5 text-xs font-semibold text-ink-dull border-b border-app-line/30">
                  {PROVIDER_LABELS[provider] ?? provider}
                </div>
                {grouped[provider].map((model) => (
                  <button
                    key={model.id}
                    type="button"
                    className={`w-full text-left px-3 py-1.5 text-sm hover:bg-app-selected transition-colors flex items-center justify-between gap-2 ${
                      model.id === value
                        ? "bg-app-selected/50 text-ink"
                        : "text-ink"
                    }`}
                    onMouseDown={(e) => {
                      e.preventDefault();
                      handleSelect(model.id);
                    }}
                  >
                    <div className="flex flex-col min-w-0">
                      <span className="truncate font-medium">{model.name}</span>
                      <span className="text-xs text-ink-faint truncate">
                        {model.id}
                      </span>
                    </div>
                    <div className="flex items-center gap-1.5 shrink-0">
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
                ))}
              </div>
            ))}
          </div>
        )}
      </div>
    </div>
  );
}
