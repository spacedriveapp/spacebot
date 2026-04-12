import { useState } from "react";
import { FontAwesomeIcon } from "@fortawesome/react-fontawesome";
import { faCodeBranch, faExternalLinkAlt } from "@fortawesome/free-solid-svg-icons";
import { Badge, Popover, SelectPill, OptionList, OptionListItem } from "@spacedrive/primitives";

// ---------------------------------------------------------------------------
// GitHub metadata helpers
// ---------------------------------------------------------------------------

interface GithubReference {
  kind: "issue" | "pr";
  label: string;
  url: string | null;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function toSafeExternalUrl(value: unknown): string | null {
  if (typeof value !== "string") return null;
  try {
    const parsed = new URL(value);
    if (parsed.protocol === "https:" || parsed.protocol === "http:") {
      return parsed.toString();
    }
    return null;
  } catch {
    return null;
  }
}

function readGithubReference(
  value: unknown,
  kind: GithubReference["kind"],
): GithubReference | null {
  if (!isRecord(value)) {
    return null;
  }

  const number = typeof value.number === "number" ? value.number : null;
  const repo = typeof value.repo === "string" ? value.repo : null;
  const url = toSafeExternalUrl(value.url);

  if (number === null && url === null && repo === null) {
    return null;
  }

  const noun = kind === "issue" ? "Issue" : "PR";
  const label = number !== null ? `${noun} #${number}` : repo ? `${noun} ${repo}` : noun;

  return { kind, label, url };
}

export function getGithubReferences(metadata: Record<string, unknown>): GithubReference[] {
  return [
    readGithubReference(metadata.github_issue, "issue"),
    readGithubReference(metadata.github_pr, "pr"),
  ].filter((reference): reference is GithubReference => reference !== null);
}

export function GithubMetadataBadges({
  metadata,
  references: precomputed,
  compact = false,
}: {
  metadata?: Record<string, unknown>;
  references?: GithubReference[];
  compact?: boolean;
}) {
  const references = precomputed ?? (metadata ? getGithubReferences(metadata) : []);
  if (references.length === 0) {
    return null;
  }

  return (
    <div className="flex flex-wrap items-center gap-1.5">
      {references.map((reference) => {
        const content = (
          <>
            <FontAwesomeIcon icon={faCodeBranch} className="text-[10px]" />
            <span>{reference.label}</span>
            {reference.url && (
              <FontAwesomeIcon icon={faExternalLinkAlt} className="text-[9px]" />
            )}
          </>
        );

        const className = compact
          ? "cursor-pointer hover:border-blue-400/50 hover:text-blue-300"
          : "cursor-pointer hover:border-blue-400/50 hover:bg-blue-500/20 hover:text-blue-300";

        if (reference.url) {
          return (
            <a
              key={`${reference.kind}-${reference.label}`}
              href={reference.url}
              target="_blank"
              rel="noopener noreferrer"
              className="inline-flex"
              onClick={(event) => event.stopPropagation()}
            >
              <Badge variant="info" size="sm" className={className}>
                {content}
              </Badge>
            </a>
          );
        }

        return (
          <Badge
            key={`${reference.kind}-${reference.label}`}
            variant="info"
            size="sm"
          >
            {content}
          </Badge>
        );
      })}
    </div>
  );
}

// ---------------------------------------------------------------------------
// InlineSelect — generic popover select pill
// ---------------------------------------------------------------------------

export function InlineSelect({
  value,
  options,
  onChange,
}: {
  value: string;
  options: { value: string; label: string }[];
  onChange: (value: string) => void;
}) {
  const [open, setOpen] = useState(false);
  const selectedLabel = options.find((o) => o.value === value)?.label ?? value;

  return (
    <Popover.Root open={open} onOpenChange={setOpen}>
      <Popover.Trigger asChild>
        <SelectPill size="sm" className="w-full">{selectedLabel}</SelectPill>
      </Popover.Trigger>
      <Popover.Content align="start" sideOffset={4} className="min-w-[160px] p-1.5">
        <OptionList>
          {options.map((opt) => (
            <OptionListItem
              key={opt.value}
              selected={opt.value === value}
              size="sm"
              onClick={() => {
                onChange(opt.value);
                setOpen(false);
              }}
            >
              {opt.label}
            </OptionListItem>
          ))}
        </OptionList>
      </Popover.Content>
    </Popover.Root>
  );
}
