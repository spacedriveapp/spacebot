# Repo Dependencies

Record that one repo depends on another, and use it to *suggest* — never to derive.
Task #29.

## Problem

A project holds many repos. Nothing records how they relate. "The web client is
generated from the api contract" exists only in the head of whoever wired a particular
workflow, and has to be re-remembered every time a pipeline touches both.

The pipeline machinery to act on it is already there: a workflow step names a repo, so
"regenerate the clients in `web` after the contract lands in `api`" is two steps and an
edge today. What is missing is the system knowing that the relationship exists, so it
can help you build that and notice when you have not.

## Design

### Declare only

```sql
CREATE TABLE repo_dependencies (
    project_id  TEXT NOT NULL,
    repo_id     TEXT NOT NULL,   -- depends on
    depends_on_repo_id TEXT NOT NULL,
    kind        TEXT,            -- generated_from | consumes | vendors | …
    note        TEXT,
    PRIMARY KEY (project_id, repo_id, depends_on_repo_id)
);
```

`kind` is an opaque label, like agent capabilities. A closed vocabulary invented now
will be wrong for the second project that uses this.

### Suggest, never derive

**This is the whole design decision.** The system may offer an edge; it must not create
one.

A wrongly derived edge makes work wait forever on something that was never going to
happen, and the author never asked for it — so when it stalls, nothing points at the
declaration that caused it. That failure is unrecoverable in the sense that matters:
the person debugging it has no reason to suspect a repo relationship they may not know
exists.

A suggestion is recoverable. It appears, you take it or you don't, and what runs is
what you agreed to.

Concretely:

- Authoring a step in `api` when `web` declares a dependency on it → offer to add a
  `web` step downstream.
- Adding an edge that contradicts a declared dependency → say so, and allow it. The
  declaration describes the repos, not the pipeline, and a template may legitimately
  disagree.
- Reviewing a template → note declared dependencies with no corresponding step, as a
  hint rather than an error.

### Where it shows

The project view, as a small graph — the repos and their arrows. That is also the
cheapest way for someone to notice a declaration is wrong, which matters because a
stale declaration that only ever produces suggestions is a nuisance, while one that
produced edges would be a fault.

## Build order

1. The table, CRUD, and the project view showing declared dependencies.
2. Suggestions in the workflow step editor.
3. The contradiction hint when an edge disagrees with a declaration.

## Risks

- **Derivation creeping in.** "It already knows, why not just add the edge" is the
  obvious next thought, and the reason not to belongs in a comment where the suggestion
  is generated, not only here.
- **Stale declarations.** They cost nothing while suggestions are all they produce.
  That property is worth keeping deliberately, not by accident.
