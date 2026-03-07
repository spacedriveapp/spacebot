# Projects

> First-draft design document — March 2026

## Problem

Today, telling Spacebot where to work is manual and repetitive. Every session starts with "we're working in `~/Projects/spacebot`", "we're on the `feat/foo` worktree", "spawn an OpenCode worker there". The agent has no persistent model of *what* you're building, *which repos* are involved, or *where* work trees live. Multi-repo projects (like Spacebot itself, with `spacebot/`, `spacebot-platform/`, `spacebot-dash/`, `spacebot-web/`) require the human to be the directory oracle every time.

## Vision

Projects give Spacebot a first-class understanding of the developer's workspace layout. When you say "let's start a new feature on Spacebot", the agent already knows the repos, can create a worktree, spawn an OpenCode worker in the right directory, and track everything — without you specifying a single path.

```
~/Projects/spacebot/            ← project root
├── spacebot/                   ← repo (core agent)
├── spacebot-platform/          ← repo (control plane)
├── spacebot-dash/              ← repo (dashboard)
├── spacebot-web/               ← repo (marketing site)
├── feat-projects/              ← worktree of spacebot/ for this feature
└── fix-auth-bug/               ← worktree of spacebot-platform/
```

A project is a **folder** containing **repos** and **worktrees**. The agent tracks all of them, understands their relationships, and uses that knowledge to autonomously route work to the right place.

---

## Concepts

### Project

A named, tracked workspace folder. Every project maps to exactly one directory on disk. Projects are scoped to an agent — different agents can have different projects (or share the same project root if their `workspace` configs overlap).

| Field | Description |
|-------|-------------|
| `id` | UUID |
| `agent_id` | Owning agent |
| `name` | Human-readable name (e.g., "Spacebot") |
| `description` | Optional rich description (markdown) |
| `icon` | Optional icon identifier or emoji |
| `tags` | JSON array of string tags for categorization |
| `root_path` | Absolute path to the project folder |
| `settings` | JSON — per-project overrides (see Settings) |
| `status` | `active` / `archived` |
| `created_at` | Timestamp |
| `updated_at` | Timestamp |

### Repo

A git repository within a project folder. Discovered automatically via `git rev-parse --git-dir` or registered manually.

| Field | Description |
|-------|-------------|
| `id` | UUID |
| `project_id` | Parent project |
| `name` | Directory name (e.g., "spacebot-dash") |
| `path` | Path relative to project root |
| `remote_url` | Primary remote URL (origin) |
| `default_branch` | e.g., "main" |
| `description` | Optional — from repo or user-provided |
| `created_at` | Timestamp |
| `updated_at` | Timestamp |

### Worktree

A git worktree checked out at the project root level. Worktrees are linked to their parent repo.

| Field | Description |
|-------|-------------|
| `id` | UUID |
| `project_id` | Parent project |
| `repo_id` | Source repo |
| `name` | Directory name |
| `path` | Path relative to project root |
| `branch` | Branch name |
| `created_by` | `user` / `agent` |
| `created_at` | Timestamp |
| `updated_at` | Timestamp |

### Worker ↔ Project Link

Workers gain optional project awareness. When a worker is spawned within a project context, metadata tracks the association.

| Addition to `worker_runs` | Description |
|---------------------------|-------------|
| `project_id` | Optional FK — which project this worker operates in |
| `worktree_id` | Optional FK — which worktree, if applicable |

---

## Settings

Projects support per-project settings that control how Spacebot manages the workspace. These are stored as JSON in the project's `settings` column and merged with agent-level defaults.

### Agent-Level Defaults

New fields on `AgentConfig` (also configurable via TOML `[agent.projects]` and the UI):

```toml
[agent.projects]
# Base directory where project folders live.
# Default: agent's workspace_dir
projects_dir = "~/Projects"

# Whether to use git worktrees for feature branches.
# When true, "start a new feature" creates a worktree at the project root.
# When false, the agent works on branches within the repo directory.
use_worktrees = true

# Worktree naming convention. Variables: {branch}, {feature}, {repo}
worktree_name_template = "{branch}"

# Whether the agent can create new worktrees autonomously
# or should ask for confirmation first.
auto_create_worktrees = false

# Whether to auto-discover repos when a project is created
# by scanning the project root for git repositories.
auto_discover_repos = true

# Whether to auto-discover existing worktrees by running
# `git worktree list` on each known repo.
auto_discover_worktrees = true

# Maximum disk usage warning threshold (bytes).
# The UI shows a warning when a project exceeds this.
disk_usage_warning_threshold = 53687091200  # 50 GB
```

### Per-Project Overrides

Any of the above can be overridden per-project in the `settings` JSON:

```json
{
  "use_worktrees": true,
  "worktree_name_template": "wt-{branch}",
  "auto_create_worktrees": true
}
```

---

## Database Schema

### Migration: `YYYYMMDD000001_projects.sql`

```sql
-- Projects
CREATE TABLE projects (
    id TEXT PRIMARY KEY,
    agent_id TEXT NOT NULL,
    name TEXT NOT NULL,
    description TEXT DEFAULT '',
    icon TEXT DEFAULT '',
    tags TEXT DEFAULT '[]',
    root_path TEXT NOT NULL,
    settings TEXT DEFAULT '{}',
    status TEXT NOT NULL DEFAULT 'active',
    created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
);
CREATE INDEX idx_projects_agent ON projects(agent_id);
CREATE INDEX idx_projects_status ON projects(agent_id, status);
CREATE UNIQUE INDEX idx_projects_root_path ON projects(agent_id, root_path);

-- Repos
CREATE TABLE project_repos (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL,
    name TEXT NOT NULL,
    path TEXT NOT NULL,
    remote_url TEXT DEFAULT '',
    default_branch TEXT DEFAULT 'main',
    description TEXT DEFAULT '',
    created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    FOREIGN KEY (project_id) REFERENCES projects(id) ON DELETE CASCADE
);
CREATE INDEX idx_project_repos_project ON project_repos(project_id);
CREATE UNIQUE INDEX idx_project_repos_path ON project_repos(project_id, path);

-- Worktrees
CREATE TABLE project_worktrees (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL,
    repo_id TEXT NOT NULL,
    name TEXT NOT NULL,
    path TEXT NOT NULL,
    branch TEXT NOT NULL,
    created_by TEXT NOT NULL DEFAULT 'user',
    created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    FOREIGN KEY (project_id) REFERENCES projects(id) ON DELETE CASCADE,
    FOREIGN KEY (repo_id) REFERENCES project_repos(id) ON DELETE CASCADE
);
CREATE INDEX idx_project_worktrees_project ON project_worktrees(project_id);
CREATE INDEX idx_project_worktrees_repo ON project_worktrees(repo_id);
CREATE UNIQUE INDEX idx_project_worktrees_path ON project_worktrees(project_id, path);
```

### Migration: `YYYYMMDD000002_worker_project_link.sql`

```sql
ALTER TABLE worker_runs ADD COLUMN project_id TEXT;
ALTER TABLE worker_runs ADD COLUMN worktree_id TEXT;
CREATE INDEX idx_worker_runs_project ON worker_runs(project_id);
```

---

## Prompt Injection

Projects are injected into the **channel system prompt** so the LLM knows what's available when deciding where to route work. This is analogous to how available topics are listed in the status block.

### Channel Prompt Fragment

New template: `prompts/en/fragments/projects_context.md.j2`

```markdown
{% if projects %}
## Active Projects

{% for project in projects %}
### {{ project.name }}
- **Root:** `{{ project.root_path }}`
{% if project.description %}- **Description:** {{ project.description }}{% endif %}
{% if project.tags %}- **Tags:** {{ project.tags | join(", ") }}{% endif %}

**Repos:**
{% for repo in project.repos %}
- `{{ repo.name }}` → `{{ repo.path }}` ({{ repo.default_branch }}){% if repo.remote_url %} — {{ repo.remote_url }}{% endif %}

{% endfor %}
{% if project.worktrees %}
**Active worktrees:**
{% for wt in project.worktrees %}
- `{{ wt.name }}` → `{{ wt.path }}` (branch: `{{ wt.branch }}`, repo: `{{ wt.repo_name }}`)
{% endfor %}
{% endif %}
{% if project.active_workers %}
**Active workers:** {{ project.active_workers | length }}
{% for w in project.active_workers %}
- Worker `{{ w.id[:8] }}`: {{ w.task[:100] }}{% if w.worktree_name %} (worktree: `{{ w.worktree_name }}`){% endif %}

{% endfor %}
{% endif %}
{% endfor %}

When spawning workers for project tasks, use the appropriate repo or worktree directory.
{% if use_worktrees %}
For new features, create a worktree in the project root using the shell tool before spawning an OpenCode worker.
{% endif %}
{% endif %}
```

### Worker Prompt Enhancement

Workers spawned with a `project_id` receive additional context in their task preamble:

```markdown
## Project Context
- **Project:** {{ project.name }}
- **Root:** `{{ project.root_path }}`
- **Working in:** `{{ working_dir }}`
{% if worktree %}
- **Worktree:** `{{ worktree.name }}` (branch: `{{ worktree.branch }}`)
- **Source repo:** `{{ worktree.repo_name }}`
{% endif %}
{% if repos %}
**Other repos in this project:**
{% for repo in repos %}
- `{{ repo.name }}` at `{{ repo.path }}`
{% endfor %}
{% endif %}
```

This gives the worker awareness of the project layout even though it operates in a single directory.

---

## Tool Changes

### `spawn_worker` Tool Enhancements

The `SpawnWorkerArgs` struct gains:

```rust
/// Project ID to associate this worker with.
/// When set, the worker gets project context in its prompt.
/// If directory is not specified, defaults to the project root.
pub project_id: Option<String>,

/// Worktree ID within the project. If set, the worker's
/// directory is automatically set to the worktree path.
pub worktree_id: Option<String>,
```

Resolution logic in `spawn_opencode_worker_from_state()`:

1. If `worktree_id` is set → resolve to worktree's absolute path, set `project_id` from worktree
2. Else if `project_id` is set and `directory` is not → use project `root_path`
3. Else if `directory` is set → use that (existing behavior)
4. Record `project_id` and `worktree_id` in `worker_runs`

### New Tool: `project_manage`

A dedicated tool for project operations, available to channels:

```
project_manage:
  action: "create" | "scan" | "add_repo" | "create_worktree" | "remove_worktree" | "list" | "disk_usage"
  project_id?: string      # for operations on existing projects
  name?: string             # project name (create)
  root_path?: string        # project root (create)
  repo_path?: string        # path to clone/add (add_repo)
  repo_url?: string         # git clone URL (add_repo)
  branch?: string           # branch for worktree (create_worktree)
  worktree_name?: string    # custom worktree name (create_worktree)
  repo_id?: string          # which repo to branch from (create_worktree)
  worktree_id?: string      # for remove_worktree
```

**Actions:**

- **`create`**: Creates a project record. If `auto_discover_repos` is enabled, scans `root_path` for git repos and registers them. If `auto_discover_worktrees` is enabled, runs `git worktree list` on each repo.
- **`scan`**: Re-scans an existing project's root for new/removed repos and worktrees. Updates the database to match disk state.
- **`add_repo`**: Registers an existing repo or clones one into the project root.
- **`create_worktree`**: Runs `git worktree add` for a repo, creates the worktree at the project root level, and registers it.
- **`remove_worktree`**: Runs `git worktree remove`, deletes the database record.
- **`list`**: Returns all projects with their repos and worktrees.
- **`disk_usage`**: Calculates disk usage for the project (total and per-directory breakdown).

---

## API Endpoints

All scoped under `/api/agents/projects` with `agent_id` query parameter:

| Method | Path | Description |
|--------|------|-------------|
| GET | `/agents/projects` | List projects (optional `status` filter) |
| POST | `/agents/projects` | Create project |
| GET | `/agents/projects/{id}` | Get project with repos, worktrees, active workers |
| PUT | `/agents/projects/{id}` | Update project (name, description, icon, tags, settings, status) |
| DELETE | `/agents/projects/{id}` | Delete project (DB records only, not files) |
| POST | `/agents/projects/{id}/scan` | Re-scan project root, sync repos and worktrees |
| GET | `/agents/projects/{id}/disk-usage` | Calculate disk usage |
| POST | `/agents/projects/{id}/repos` | Add/register a repo |
| DELETE | `/agents/projects/{id}/repos/{repo_id}` | Remove repo record |
| POST | `/agents/projects/{id}/worktrees` | Create a worktree |
| DELETE | `/agents/projects/{id}/worktrees/{wt_id}` | Remove worktree (DB + git worktree remove) |
| GET | `/agents/projects/{id}/workers` | List workers associated with this project |

### Response Shapes

```typescript
interface Project {
  id: string;
  agent_id: string;
  name: string;
  description: string;
  icon: string;
  tags: string[];
  root_path: string;
  settings: ProjectSettings;
  status: "active" | "archived";
  repos: Repo[];
  worktrees: Worktree[];
  active_workers: WorkerRunInfo[];
  disk_usage?: DiskUsage;
  created_at: string;
  updated_at: string;
}

interface Repo {
  id: string;
  name: string;
  path: string;
  remote_url: string;
  default_branch: string;
  description: string;
}

interface Worktree {
  id: string;
  repo_id: string;
  repo_name: string;
  name: string;
  path: string;
  branch: string;
  created_by: "user" | "agent";
}

interface DiskUsage {
  total_bytes: number;
  entries: { name: string; bytes: number; is_dir: boolean }[];
}
```

---

## UI

### New Route: `/agents/$agentId/projects`

Add a "Projects" tab to `AgentTabs` (positioned after Workers, before Tasks).

### Project List View

Card grid (similar to Channels page). Each project card shows:

- **Icon** (emoji or generated) + **Name**
- **Description** (truncated)
- **Tags** as `Badge` pills
- **Stats row**: repo count, worktree count, active worker count
- **Disk usage** bar (if computed) with warning color at threshold
- **Last updated** relative time
- **Status** indicator (active = green dot, archived = muted)

Click → project detail.

### Project Detail View

Split into sections:

#### Header
- Project name (editable inline)
- Description (editable, markdown preview)
- Icon selector
- Tags editor (`TagInput` component)
- Root path display (monospace, with copy button)
- Status toggle (active/archived)
- "Scan" button (re-sync repos and worktrees from disk)

#### Repos Section
- Card list of repos. Each shows:
  - Name, relative path, remote URL, default branch
  - Worktree count spawned from this repo
  - "Add Worktree" action button
  - Remove button (with confirmation)
- "Add Repo" button → dialog (path or clone URL)

#### Worktrees Section
- Card list of active worktrees. Each shows:
  - Name, branch, source repo name
  - "Created by" badge (user/agent)
  - "Open Worker" action → spawns an OpenCode worker in this directory
  - Remove button (with confirmation — runs `git worktree remove`)
- "Create Worktree" button → dialog (select repo, enter branch name)

#### Active Workers Section
- Filtered worker list (same component as AgentWorkers, filtered by `project_id`)
- Shows workers currently running in this project's directories
- Click → navigates to worker detail

#### Disk Usage Section
- Expandable panel showing directory-level breakdown
- Total size prominently displayed
- Warning banner if above threshold
- "Refresh" button to recalculate

### Settings Integration

The agent config page (`/agents/$agentId/config`) gains a "Projects" section with the agent-level defaults:

- Projects directory path
- Use worktrees toggle
- Worktree name template
- Auto-create worktrees toggle
- Auto-discover repos toggle
- Auto-discover worktrees toggle
- Disk usage warning threshold

---

## Flow: "Let's start a new feature"

With projects configured, a typical conversation:

```
User: Let's start working on a new feature for the auth system in spacebot-platform

Channel LLM (internally):
  1. Checks active projects → finds "Spacebot" project
  2. Identifies "spacebot-platform" repo in the project
  3. use_worktrees = true → calls project_manage(create_worktree,
       repo_id=<platform_repo_id>, branch="feat/auth-improvements")
  4. Worktree created at ~/Projects/spacebot/feat-auth-improvements/
  5. Spawns OpenCode worker with project_id + worktree_id
  6. Worker receives project context and starts in the right directory

Channel: I've created a worktree `feat-auth-improvements` branching from
spacebot-platform's main branch and spawned a worker there. The worker
is now exploring the auth system. I'll relay its findings when it's done.
```

No paths. No directory instructions. The agent just knows.

---

## Flow: "What are we working on?"

```
User: What's the status of everything?

Channel LLM (internally):
  1. Checks active projects with their worktrees and workers
  2. Compiles a summary

Channel: Here's where things stand with the Spacebot project:
  
  **Active worktrees:**
  - `feat-auth-improvements` (spacebot-platform, branch: feat/auth-improvements)
    → Worker running: "Implement OAuth2 PKCE flow" (12 min, 34 tool calls)
  - `feat-projects` (spacebot, branch: feat/projects)
    → Idle (last worked on 2 hours ago)
  
  **Repos:** 4 (spacebot, spacebot-platform, spacebot-dash, spacebot-web)
  **Disk usage:** 12.4 GB
```

---

## Topics Integration

Topics and projects are complementary systems that can reference each other:

- A **topic** can have a `project_id` in its criteria metadata, scoping its memory synthesis to memories generated while working on that project.
- When spawning workers with both `project_id` and `topic_ids`, the worker gets spatial context (where) from the project and knowledge context (what) from the topics.
- The channel prompt lists both available projects and available topics. The LLM can match them naturally: "For the auth feature on spacebot-platform, I'll use the 'Auth System' topic and work in the Spacebot project."

Future: topics could be auto-associated with projects via tagging or memory source tracking (memories created by workers in project X are relevant to topics about project X).

---

## Implementation Phases

### Phase 1: Data Layer
- Migration for `projects`, `project_repos`, `project_worktrees` tables
- Migration to add `project_id`, `worktree_id` to `worker_runs`
- `ProjectStore` with CRUD operations
- Git helpers: repo discovery, worktree list/add/remove
- Add `ProjectStore` to `AgentDeps`

### Phase 2: Prompt Injection
- `fragments/projects_context.md.j2` template
- Inject active projects into channel system prompt (status block area)
- Worker project context preamble
- Add project data to `render_channel_prompt()` and `render_worker_prompt()`

### Phase 3: Tool Integration
- `project_manage` tool (create, scan, add_repo, create/remove worktree, list, disk_usage)
- Extend `spawn_worker` with `project_id` and `worktree_id`
- Worker directory resolution from project/worktree
- Record project/worktree in `worker_runs` on spawn

### Phase 4: API
- REST endpoints for projects, repos, worktrees
- Disk usage calculation endpoint
- Worker list filtered by project

### Phase 5: UI
- Projects tab and route
- Project card grid (list view)
- Project detail view (repos, worktrees, workers, disk usage)
- Agent config: project settings section
- Project-scoped worker list

### Phase 6: Settings & Configuration
- `ProjectsConfig` in agent config with TOML schema
- Per-project settings JSON
- Hot-reload support for project settings
- Sandbox allowlist integration (project root paths should be readable/writable)

### Phase 7: Polish & Advanced Features
- SSE events for project changes (worktree created, scan complete, etc.)
- Project health checks (detect deleted/moved directories)
- Worktree cleanup (remove merged branches)
- Cross-project awareness (when an agent has multiple projects)
- Integration with topics system (project-scoped topic criteria)

---

## Open Questions

1. **Worktree location flexibility**: The current design enforces worktrees at the project root level (siblings to repos). Should we support worktrees in arbitrary locations? This complicates discovery but some workflows may need it.

2. **Multi-agent project sharing**: If two agents have different `workspace_dir` values but work on the same project root, should they share the project record? Current design scopes projects to `agent_id`, meaning each agent has its own view. This is simpler but means duplicate records.

3. **Auto-project creation**: Should the agent auto-create a project when it notices it's repeatedly working in the same directory? Or should projects always be explicitly created? Probably explicit for now, auto-discovery as a later enhancement.

4. **Repo cloning**: Should `add_repo` support cloning from a URL into the project root? This requires network access and disk space. Useful but could surprise users. Probably gated behind a setting.

5. **Worktree branch naming**: When the agent creates a worktree, what naming convention for the branch? Use the `worktree_name_template` setting? Let the LLM decide? A combination?

6. **Sandbox implications**: If sandbox mode is active, the project root and all worktree paths need to be in the write allowlist. Should project creation automatically update the sandbox config?

7. **Project deletion semantics**: Deleting a project should remove DB records but leave files on disk. Should there be an option to also clean up worktrees (run `git worktree remove`)? Dangerous but useful.

8. **AGENTS.md per project**: Should each project support an `AGENTS.md` file at the project root that gets injected into worker context? This would let project-specific instructions live alongside the code.
