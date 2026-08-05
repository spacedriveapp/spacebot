# Worktree Provisioning

A step says it needs its own checkout, and gets one. Parallel work in one repo stops
trampling itself.

## Problem

Tasks bind to a project, a repo, and optionally a worktree, and that binding is
enforced: `resolve_worker_working_dir` refuses any directory the sandbox allowlist does
not cover, and a task whose binding points somewhere disallowed parks for a human
rather than running in the wrong tree. Workflow steps carry `repo_id`, so multi-repo
pipelines — "regenerate the clients in `web` after the contract lands in `api`" — are
already two steps and an edge.

Three things are missing, and they became urgent together:

- **`auto_create_worktrees` is dead config.** Ten references across `config/types.rs`,
  `config/load.rs`, `api/config.rs` and `projects/store.rs`, every one of them
  plumbing. Nothing creates a worktree. It is settable in the UI and does nothing.
- **A workflow step has `repo_id` but no worktree.** A step can name a repo. It cannot
  say "run in a checkout of your own".
- **Fan-out made this sharp.** Before dynamic fan-out, concurrent same-repo work was
  something you had to go out of your way to build. Now `for_each` over five repos —
  or five branches of one repo — is a single field, and every branch lands in the same
  checkout. Two agents editing one working tree is not a race that produces a bad
  result; it produces an incoherent one.

Command steps (`command-steps.md`) sharpen it again: `bun run lint` in a tree another
step is mid-edit reports on a state that never existed.

## Design

### A step declares what it needs

`workflow_steps.worktree_mode`:

| mode | meaning |
|---|---|
| `inherit` | default. Use whatever the task binding already says — today's behaviour, unchanged |
| `per_run` | one worktree for this step, created at launch, shared by nothing else |
| `per_branch` | one worktree per fan-out branch, created when the fan-out expands |

`inherit` being the default matters: every existing template keeps working, and a
pipeline that genuinely wants one shared checkout can still have one.

`per_branch` on a step that is not a fan-out is a template error, refused at launch.
Silently degrading it to `per_run` would give an author a pipeline that looks isolated
and is not.

### Naming

Deterministic, derived from the run and the branch key:

```
<project>/.worktrees/<run-id-short>-<step-key>[-<branch-key>]
```

Deterministic so it is greppable, re-derivable after a crash, and obvious in `git
worktree list` at three in the morning. Not random, because a leftover worktree with a
uuid name tells nobody what made it.

The branch git creates follows the same scheme. `create_worktree` already falls back to
attaching an existing branch when `-b` fails with "already exists" — good behaviour for
a human retrying, and something to watch for here, since a re-launch reusing a branch
name would silently share history between two runs. Run-scoped names avoid it.

### Base ref

A step names what it forks from — a branch, a tag, a sha — defaulting to the repo's
current `HEAD`. Explicit because "whatever was checked out when the run happened" is
not reproducible, and a pipeline whose starting point drifts under it is one whose
failures cannot be explained afterwards.

### The sandbox already covers it

Worktrees live under the project root, and `refresh_project_paths` injects project
roots into the allowlist, so a provisioned worktree is inside the boundary without any
new allowlist plumbing. `resolve_worker_working_dir` then enforces it exactly as it
does today.

This is worth stating precisely because it is the part most likely to be "helpfully"
widened later. **No new writable path should ever be added for a worktree.** A worktree
outside the project root would need one, which is the reason not to put it there.

## Lifecycle — the part with a real decision in it

### Never delete a dirty worktree

Uncommitted work from a failed run is **evidence**, not garbage. It is the thing you
want when the question is "what did it actually do before it broke".

The good news is that this is already the behaviour and the job is to keep it:
`remove_worktree` runs `git worktree remove` **without `--force`**, and git refuses on
a dirty tree. So the rule is a prohibition rather than a feature —

> **Never pass `--force`. Never add a flag that would.**

— and the reaper below simply lets git's refusal stand, records it, and moves on.

### What gets reaped, and when

On run completion, each worktree the run provisioned is offered for removal. Git
accepts the clean ones and refuses the rest. A refusal is not an error: it is recorded
against the run as "left behind, has uncommitted changes", and surfaced.

Committed-but-unmerged work needs no special handling — `git worktree remove` deletes
the checkout, not the branch, so commits survive in the repo regardless.

### Orphans

A crash between "worktree created" and "run recorded it" leaves a directory nothing
owns. The deterministic naming scheme is what makes these findable: anything under
`.worktrees/` whose run id is not a live run is an orphan, and can be listed for a
person without ever being deleted automatically.

Deliberately a *report*, not a sweep. The one thing worse than a stale worktree is a
background process that deletes directories.

### Disk

Worktrees are cheap in git terms and not free on disk, and a fan-out of fifty is fifty
checkouts. A cap on concurrent worktrees per run, refusing at expansion with a clear
message, beats discovering it as ENOSPC in the middle of a pipeline.

## Failure modes

| failure | response |
|---|---|
| `git worktree add` fails (dirty index, bad ref, disk) | block the task `capability` with git's own stderr — a person must fix the repo |
| base ref does not exist | refuse at **launch**, not at run time — it is knowable from the template |
| worktree removal refused | expected, recorded, not an error |
| two branches provisioning concurrently | run-scoped names make collision impossible by construction |

## Build order

1. `worktree_mode` on the step, plus launch validation (`per_branch` requires a
   fan-out; base ref must resolve).
2. Provisioning at launch for `per_run`, and inside the fan-out expansion transaction
   for `per_branch` — the same transaction that emits the branches, so a branch never
   exists without its checkout.
3. Reaping on run completion, with git's refusal respected and recorded.
4. Orphan listing.
5. **Delete `auto_create_worktrees`.** It has never done anything. Replacing dead
   config with real config is not the same as leaving both.
6. UI: worktree mode in the step editor, and the provisioned path on the run view —
   "which checkout did this actually run in" is the first question anyone asks.

## Risks

- **`--force` creeping in.** Someone will hit a stuck reaper and reach for it. The
  prohibition needs to be in a comment at the call site, not only in this document.
- **Provisioning outside the fan-out transaction.** A branch task that exists without
  its worktree runs in the wrong directory — precisely the failure the cwd enforcement
  was built to prevent, reintroduced one layer up.
- **Reaping while a task is still running.** A retry after a reap would find no
  checkout. Reaping is keyed on the *run* being finished, not on the step.
