---
name: writing-guide
description: Use when writing or editing any Spacebot copy — README sections, docs, release notes, marketing text, design doc summaries. Covers voice, tone, patterns to avoid, and what good Spacebot writing sounds like.
---

# Writing Guide

Spacebot copy should sound like a confident engineer wrote it, not a language model. The test: would a developer reading the README think "someone knows what they're talking about" or "this was AI-generated"? These rules exist because the second outcome is common and costs credibility.

## Voice

Direct. Technical. No hedging. Short sentences with real content. Lead with the fact, not the framing. State what something is — not what it isn't.

The tagline sets the tone: "The agent harness that runs teams, communities, and companies." That's a claim. It doesn't explain itself. Good Spacebot copy makes claims and lets the detail below earn them.

## Patterns to Avoid

These are the specific patterns that make copy sound AI-generated. Avoid all of them.

**Em dashes in prose sentences.** Em dashes are fine in bullet point labels ("**Shell** — run arbitrary commands") but not inside sentences. Replace with a comma, a period, or restructure.

Bad: "The cortex sees across all channels — the only process with full system scope."
Good: "The cortex is the only process that sees across all channels."

**"Not X. Not Y." openers.** Starting a description by saying what something isn't.

Bad: "Not markdown files. Not unstructured vectors. Spacebot's memory is..."
Good: "Spacebot's memory is a typed, graph-connected knowledge system."

**"This isn't X, it's Y."** Classic AI construction. Just say the thing.

Bad: "This isn't a generic claim — it's four specific mechanisms."
Good: "Spacebot builds on itself through four specific mechanisms."

**"The result is..." and "The through-line:"** Setup language that delays the actual point.

Bad: "The result is an agent that works out of the box."
Good: "It works out of the box."

**"No X. No Y." closers.** Ending a paragraph with a string of negatives.

Bad: "No heartbeat.json. No drift."
Good: Just cut it, or fold it into the sentence that precedes it.

**"This is the most important X."** Let the reader decide what's important.

Bad: "This is the most important structural difference between Spacebot and every other agent harness."
Good: Just state the difference.

**Semicolons in prose.** Use a period.

Bad: "The agent proposes; you decide."
Good: "The agent proposes. You decide."

**Parallel triplets.** Three consecutive "same X, same Y, same Z" constructions sound mechanical.

Bad: "Same context, same memories, same understanding."
Good: "They have the full conversation history."

**"Not only X, but also Y."** Pick one.

**Generic improvement claims.** Never say "self-improving" or "gets smarter." Name the actual mechanism.

Bad: "Spacebot learns from experience."
Good: "After a conversation goes idle, a background branch reviews the history and saves skills and memories worth keeping."

## What Spacebot Is Opinionated About

When describing Spacebot's opinions, name them specifically: the process model, memory schema, and task lifecycle. Don't say its opinions are about "one thing" — they cover multiple things. The unifying idea is that state belongs in structured storage, not markdown files the LLM manages.

## What Is and Isn't a Differentiator

**Is a differentiator:**
- The task system as the structural foundation of autonomy
- True process-level concurrency across users
- Typed memory graph in SQLite with graph edges and hybrid search
- Autonomy channel with full context on wake, state tracked through tasks not files
- Spacedrive integration for cross-device execution and safe data access

**Is not a differentiator:**
- Skills (every harness has skills, the format is not special)
- "Self-improving" as a generic claim
- Internal implementation details the user doesn't see

Don't advertise internal improvements as external features. If Spacebot previously sent cold context to workers and now sends full context, that's an internal fix. The user-visible claim is "the autonomy channel wakes with full context" — not "unlike the old approach."

## The Spacedrive Story

Two layers:

**What exists today:** multi-device access via P2P, remote execution via Spacedrive's permission system, file system intelligence via context nodes, safe data access via Prompt Guard 2 screening.

**Where it's going:** team + personal library switching, org graph delegation, full company deployment model.

Be honest about which is which. Don't present the vision as current capability.

## Prose Structure

Opening paragraphs should be 3-4 sentences. Lead with the strongest claim. No setup sentences ("In this section, we'll cover..."). No closing sentences that summarize what was just said.

Bullet points are for lists of discrete items. Prose is for explanation, argument, and narrative. Don't bullet-point things that belong in prose.

Section headers are short nouns or short sentences. No gerunds ("Building the Memory System"). No questions ("What Is the Task System?").

## Words to Avoid

comprehensive, utilize, harness (as a verb), unlock, revolutionary, groundbreaking, remarkable, pivotal, powerful, exciting, cutting-edge, seamless, robust, leverage, paradigm, ecosystem (when not literally true), journey, dive deep, explore, embark.

Also avoid: "at its core," "under the hood," "out of the box" (unless used once, intentionally), "world-class," "best-in-class."
