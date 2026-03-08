# Channel Attachment Persistence

Persistent file/image storage for channel attachments, with history integration and a recall tool so the channel can reference saved files across conversation turns without re-downloading them.

## Problem

Today, when a user sends an image or file in a channel (Discord, Slack, Telegram), the attachment is downloaded, converted to base64 (images) or inlined text, and injected into the current LLM turn as `UserContent`. Once that turn completes, the actual file data is gone. The LLM's response may describe the image, but the raw file is not retained anywhere.

This creates several problems:

1. **No recall.** If the user says "let's talk about that image from earlier," the channel has no way to re-analyze it. The image only exists as whatever the LLM said about it in its previous response.
2. **No handoff.** If the channel needs to delegate work involving a file to a worker ("resize these images", "extract data from this PDF"), it has no path to give the worker access to the actual file. The channel would need to spawn a worker, and that worker would have no way to get the file.
3. **No persistence.** Platform URLs (especially Slack's `url_private`) expire. If history is replayed or the channel restarts, the URLs are dead.
4. **No file identity.** The LLM sees base64 blobs or inline text with a filename, but there's no stable identifier linking a history entry to an on-disk file.

## Design

### Saved Attachments Table

A new `saved_attachments` table tracks every file persisted to disk. This is the source of truth for what's been saved and where it lives.

```sql
CREATE TABLE IF NOT EXISTS saved_attachments (
    id TEXT PRIMARY KEY,              -- UUID
    channel_id TEXT NOT NULL,         -- which channel received the file
    message_id TEXT,                  -- conversation_messages.id (nullable, for files from non-message sources)
    original_filename TEXT NOT NULL,  -- the filename as it appeared on the platform
    saved_filename TEXT NOT NULL,     -- the actual filename on disk (may have suffix for dedup)
    mime_type TEXT NOT NULL,
    size_bytes INTEGER NOT NULL,
    disk_path TEXT NOT NULL,          -- absolute path on disk
    created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    FOREIGN KEY (channel_id) REFERENCES channels(id) ON DELETE CASCADE
);

CREATE INDEX idx_saved_attachments_channel ON saved_attachments(channel_id, created_at);
CREATE INDEX idx_saved_attachments_message ON saved_attachments(message_id);
```

Key fields:

- `original_filename` — what the user named the file (e.g., `screenshot.png`). Preserved for display.
- `saved_filename` — what's on disk. Usually the same as `original_filename`, but with a numeric suffix when there's a name collision (e.g., `screenshot_2.png`).
- `disk_path` — absolute path. Redundant with `workspace/saved/{saved_filename}` but avoids assumptions about workspace location if it changes.
- `message_id` — links back to the conversation message that contained this attachment. Nullable because files could come from non-message sources in the future.

### Disk Layout

Saved attachments live in a new `saved/` directory under the workspace:

```text
workspace/
  ingest/          -- existing: user drops files for ingestion
  skills/          -- existing: skill files
  saved/           -- NEW: persisted channel attachments
    screenshot.png
    diagram.png
    report_2.pdf   -- deduped name (second "report.pdf" received)
```

Note: Identity files (`SOUL.md`, `IDENTITY.md`, `ROLE.md`) live in the agent root directory, not in the workspace.

The `saved/` directory is created at startup alongside `ingest/` and `skills/`.

A `saved_dir()` helper is added to `ResolvedAgentConfig`:

```rust
pub fn saved_dir(&self) -> PathBuf {
    self.workspace.join("saved")
}
```

### Filename Deduplication

When saving a file, if `saved/{original_filename}` already exists on disk AND the existing file has a different `id` in the database:

1. Strip the extension: `report` from `report.pdf`
2. Append `_N` where N starts at 2: `report_2.pdf`
3. Increment until a unique name is found

This is a filesystem + DB check, not just filesystem — if a file was saved and then deleted from disk, the DB record still exists and the name is still "taken" to avoid confusion in history references.

### Channel Behavior Setting: `save_attachments`

A new boolean field on `ChannelConfig`:

```rust
pub struct ChannelConfig {
    pub listen_only_mode: bool,
    pub save_attachments: bool,   // NEW — default: false
}
```

TOML schema:

```toml
[channel]
listen_only_mode = false
save_attachments = true
```

When `save_attachments` is `true` for a channel, every attachment received in that channel is:

1. Downloaded (as it is today)
2. Saved to `workspace/saved/` with dedup
3. Recorded in `saved_attachments` table
4. Processed for the LLM as today (base64 image, inline text, etc.)
5. The history entry includes a metadata annotation with the attachment ID and saved filename

When `false` (default), behavior is unchanged — files are downloaded, processed for the current turn, and discarded.

### History Integration

The key insight: when an attachment is saved, the conversation history entry for that message needs to carry metadata that the LLM can see on future turns, even after the base64 image data is gone from the context window.

#### User Message Logging

When `save_attachments` is enabled and attachments are processed, the `ConversationLogger::log_user_message()` call includes attachment metadata in the `metadata` JSON blob:

```json
{
  "platform": "discord",
  "attachments": [
    {
      "id": "a1b2c3d4-...",
      "filename": "screenshot.png",
      "saved_filename": "screenshot.png",
      "mime_type": "image/png",
      "size_bytes": 245760
    },
    {
      "id": "e5f6g7h8-...",
      "filename": "diagram.png",
      "saved_filename": "diagram.png",
      "mime_type": "image/png",
      "size_bytes": 189440
    }
  ]
}
```

#### History Reconstruction

When history is loaded for LLM context (the `Vec<Message>` that gets passed to `agent.prompt().with_history()`), messages with attachment metadata get an annotation appended to their content:

```text
[User sent 2 attachments: screenshot.png (image/png, 240 KB, id:a1b2c3d4), diagram.png (image/png, 185 KB, id:e5f6g7h8)]
```

This is a text annotation — not the actual image. The LLM can see *that* images were sent, *what* they were named, and *how* to reference them. On the turn when the image was originally sent, the LLM also sees the base64 image data (as today). On subsequent turns, only the annotation remains.

This gives the LLM enough context to:
- Know which files exist in the conversation
- Reference them by name or ID
- Use the recall tool to re-analyze them or get their paths

### Recall Tool: `attachment_recall`

A new channel-level tool that lets the channel retrieve saved attachment info and optionally re-load file content.

```rust
pub struct AttachmentRecallTool {
    pool: SqlitePool,
    workspace: PathBuf,
}
```

**Arguments:**

```json
{
  "type": "object",
  "properties": {
    "action": {
      "type": "string",
      "enum": ["list", "get_path", "get_content"],
      "description": "What to do: list recent attachments, get the disk path of a specific file, or re-load its content for analysis."
    },
    "attachment_id": {
      "type": "string",
      "description": "The attachment ID (from history metadata). Required for get_path and get_content."
    },
    "filename": {
      "type": "string",
      "description": "Alternative to attachment_id — look up by original filename. If multiple matches, returns the most recent."
    },
    "limit": {
      "type": "integer",
      "default": 10,
      "description": "For list action: how many recent attachments to return."
    }
  },
  "required": ["action"]
}
```

**Actions:**

- **`list`** — Returns a summary of recent saved attachments for the current channel. Output: JSON array of `{id, filename, saved_filename, mime_type, size_bytes, disk_path, created_at}`.

- **`get_path`** — Returns the absolute disk path of a specific attachment (by ID or filename). This is the primary use case for delegation: the channel calls `get_path`, gets `/home/user/.spacebot/agents/mybot/workspace/saved/screenshot.png`, and includes that path in a worker task description.

- **`get_content`** — Re-loads the file from disk and returns it as `UserContent` (base64 for images, inline text for text files). This lets the channel re-analyze an image that was sent many turns ago. **Size limit: 10 MB.** Files larger than this return an error directing the channel to delegate to a worker instead.

**Tool availability:** Channel-level only (added/removed per-turn like `send_file`). Branches and workers don't need this — the channel passes paths to workers via task descriptions, and branches can see the attachment metadata in the cloned history.

### Processing Flow

#### Inbound (attachment received)

```text
User sends message with attachments
  → Adapter extracts Attachment structs (unchanged)
    → Channel receives InboundMessage with MessageContent::Media
      → download_attachments() runs (unchanged — produces Vec<UserContent>)
      → IF save_attachments is enabled for this channel:
        → save_channel_attachments() runs in parallel:
          → For each attachment:
            → Download bytes (reuses download_attachment_bytes())
            → Compute saved_filename (dedup against DB + disk)
            → Write to workspace/saved/{saved_filename}
            → INSERT into saved_attachments table
          → Returns Vec<SavedAttachment> metadata
        → Attachment metadata is merged into the message metadata HashMap
      → log_user_message() persists content + metadata (including attachment info)
      → run_agent_turn() with attachment UserContent (unchanged)
```

The save operation runs concurrently with the download-for-LLM operation. Since both need the raw bytes, the download happens once and the bytes are shared (via `Arc<Vec<u8>>` or by saving first and reading from disk for the LLM).

Actually, simpler: download once, save to disk, then read from disk for base64 encoding. The disk write is fast (local SSD) and avoids holding large byte buffers in memory. This also means the save is the source of truth — if the base64 encoding fails, the file is still on disk.

Revised flow:

```text
For each attachment (when save_attachments = true):
  1. Download raw bytes from URL
  2. Compute saved_filename (dedup)
  3. Write bytes to workspace/saved/{saved_filename}
  4. INSERT row into saved_attachments
  5. Read bytes back from disk for base64/inline processing → UserContent
```

For step 5, images are re-read and base64 encoded. Text files are re-read and inlined. This is a trivial read from local disk. When `save_attachments = false`, the existing direct-from-download flow is unchanged.

#### Recall (channel wants to reference a past attachment)

```text
User: "Can you analyze that screenshot I sent earlier?"
  → Channel sees attachment annotation in history:
    [User sent 1 attachment: screenshot.png (image/png, 240 KB, id:a1b2c3d4)]
  → Channel calls attachment_recall(action: "get_content", attachment_id: "a1b2c3d4")
    → Tool reads file from workspace/saved/screenshot.png
    → Returns UserContent::Image (base64) — injected into current turn
  → Channel can now see and analyze the image again
```

#### Delegation (channel passes file to worker)

```text
User: "Resize that screenshot to 800x600"
  → Channel sees attachment annotation in history
  → Channel calls attachment_recall(action: "get_path", attachment_id: "a1b2c3d4")
    → Returns: "/home/user/.spacebot/agents/mybot/workspace/saved/screenshot.png"
  → Channel calls spawn_worker with task:
    "Resize the image at /home/user/.spacebot/agents/mybot/workspace/saved/screenshot.png to 800x600.
     Save the result in the same directory."
  → Worker has file tool access to the workspace, can read/write the file
```

### What the LLM Sees

**Turn 1 (image sent):**
```text
[User content]
<base64 image data>          ← actual image for vision analysis
screenshot.png (image/png)

"Here's a screenshot of the bug"
```

**Turn 3 (later in conversation, image data is gone from context):**
```text
[History entry for Turn 1]
Jamie: Here's a screenshot of the bug
[Attachments: screenshot.png (image/png, 240 KB, id:a1b2c3d4)]
```

The LLM sees the annotation, knows the file exists, and can use `attachment_recall` to get it back if needed.

### Edge Cases

**Large files.** The `get_content` action has a 10 MB limit. For larger files, the channel should use `get_path` and delegate to a worker. The tool error message guides this.

**Deleted files.** If someone manually deletes a file from `workspace/saved/`, the DB record still exists. `get_content` and `get_path` return an error indicating the file is missing on disk. The history annotation remains (it's in the conversation log).

**Disk space.** No automatic cleanup in v1. The `saved/` directory grows indefinitely. Future work: configurable retention, size limits, or LRU eviction. The DB table makes cleanup queries easy (`DELETE FROM saved_attachments WHERE created_at < ?` + unlink files).

**Concurrent saves.** Two messages arriving simultaneously with the same filename: the dedup logic uses a DB check (`SELECT COUNT(*) FROM saved_attachments WHERE saved_filename = ?`) inside a transaction, so concurrent saves get unique names.

**Non-image/non-text files.** PDFs, videos, archives, etc. are saved to disk and recorded in the DB, but `get_content` only supports images and text files (same as today's processing). For other file types, `get_content` returns a metadata description and the disk path, encouraging delegation to a worker.

**Channel restart / history replay.** On restart, the channel loads history from the DB. Messages with attachment metadata show the `[Attachments: ...]` annotation. The files are on disk. The `attachment_recall` tool works immediately. No re-download needed.

## Implementation

### New Files

- `migrations/2026MMDD000001_saved_attachments.sql` — table + indexes
- No new Rust source files — attachment saving logic goes in `channel_attachments.rs`, the tool goes in `tools/attachment_recall.rs`, and the config change is a one-line addition to `ChannelConfig`

### Modified Files

| File | Change |
|------|--------|
| `src/config/types.rs` | Add `save_attachments: bool` to `ChannelConfig`, add `saved_dir()` to `ResolvedAgentConfig` |
| `src/config/toml_schema.rs` | Add `save_attachments: Option<bool>` to `TomlChannelConfig` |
| `src/config/runtime.rs` | Wire through `save_attachments` in `ChannelConfig` ArcSwap |
| `src/main.rs` | Create `saved/` directory at startup |
| `src/agent/channel_attachments.rs` | Add `save_channel_attachments()` function, `SavedAttachment` struct |
| `src/agent/channel.rs` | Call `save_channel_attachments()` when enabled, merge metadata into message log, add history annotation reconstruction |
| `src/tools/attachment_recall.rs` | New tool implementation |
| `src/tools.rs` | Register `AttachmentRecallTool` in channel tool server (add/remove per-turn) |
| `src/conversation/history.rs` | Add helper to reconstruct attachment annotations from message metadata during history load |
| `prompts/tools/attachment_recall.md` | Tool description for LLM |

### Phases

**Phase 1: Storage Layer**
- Migration
- `SavedAttachment` struct and `save_channel_attachments()` in `channel_attachments.rs`
- `saved_dir()` helper, `save_attachments` config field
- Startup directory creation
- Filename dedup logic

**Phase 2: Channel Integration**
- Wire save flow into `channel.rs` attachment processing
- Merge attachment metadata into message metadata
- History annotation reconstruction (when loading history, append `[Attachments: ...]` to messages that have attachment metadata)

**Phase 3: Recall Tool**
- `AttachmentRecallTool` implementation (list, get_path, get_content)
- Register in channel tool server
- Prompt file

**Phase 4: Polish**
- API endpoint to list saved attachments for a channel (for the dashboard)
- Dashboard UI: attachment gallery or inline previews in timeline
- Configurable retention / size limits

### Phase Ordering

```text
Phase 1 (storage)    — standalone
Phase 2 (channel)    — depends on Phase 1
Phase 3 (tool)       — depends on Phase 1, independent of Phase 2
Phase 4 (polish)     — depends on Phases 2 + 3
```

Phases 2 and 3 can run in parallel after Phase 1.

## Open Questions

**Save all channels or opt-in?** Current design is opt-in via `save_attachments` per-channel config. Alternative: save by default, with an opt-out. The opt-in approach is safer for disk space and avoids surprises, but means users have to discover and enable the setting. Could default to `true` in a future version once retention policies exist.

**Attachment dedup by content hash.** Two users sending the same image would create two copies on disk. A content-hash-based dedup (SHA-256 of the bytes, store once, reference twice) would save disk space but adds complexity. Not worth it for v1 — most attachments are unique.

**Branch access to files.** Branches clone the channel history, so they see attachment annotations. Should branches also get the `attachment_recall` tool? Currently no — branches don't need to re-analyze images (that's the channel's job) and they can see the metadata in history to include paths in worker task descriptions. If a branch needs to spawn a worker with a file path, it can read the path from the history annotation. Revisit if this becomes a friction point.

**Audio transcription.** Transcribed audio attachments are currently ephemeral too. Should the original audio file be saved to `saved/` alongside images and documents? Probably yes — the transcription text is in history, but the audio file itself might be useful for re-processing or delegation. The design supports this naturally since audio has a MIME type and the save logic is MIME-agnostic.

**Video files.** Currently unsupported by the LLM processing pipeline (no vision model handles video). Saving them to disk is still useful for worker delegation ("extract frames from this video"). The save logic handles them; `get_content` returns metadata + path instead of trying to base64 a video.
