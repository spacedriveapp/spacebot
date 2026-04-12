# Attachment Persistence: Portal Integration & Remaining Gaps

The core attachment system described in `channel-attachment-persistence.md` is implemented: the `saved_attachments` table, `channel_attachments.rs`, the `attachment_recall` tool, and adapter extraction across Telegram, Discord, Slack, and email are all in production.

This doc covers what remains:

1. **Default-on** — `save_attachments` currently defaults to `false`; it should default to `true`
2. **Image re-injection** — the recall tool's `get_content` action returns a metadata description for images instead of actual image content
3. **Portal chat upload** — no file input in the composer
4. **Portal timeline display** — no attachment previews in message history
5. **Attachment serving API** — no endpoints to retrieve attachment files for the dashboard

---

## 1. Default On

Change the default from `false` to `true` in `ChannelConfig`:

```rust
pub struct ChannelConfig {
    pub save_attachments: bool,  // was: false, now: true
}
```

And in the TOML config resolution — `save_attachments` omitted from config means `true`.

**Rationale:** The user's intent is that no attachment sent to the agent, from any platform, should ever be lost. Opt-in defeats this — most users won't discover or enable the setting. The risk the original doc cited ("surprises for disk space") is acceptable given that attachments are typically small and the workspace directory is already user-managed. Retention policy can come later; the default behavior should be safe and complete from day one.

No migration needed. The table already exists. Channels that currently have no `save_attachments` config will start saving attachments on next message.

---

## 2. Image Re-injection Fix

The recall tool's `get_content` action handles text files correctly (returns inline content) but for images currently returns a metadata description only:

```
"summary": "Image file screenshot.png (image/png, 240 KB). Use get_path to get the disk path for worker delegation."
```

This is wrong. The channel called `get_content` because it wants to see the image again. It should get the image.

The problem is that tool output is JSON — it can't directly inject a `UserContent::Image` block. The fix is a side-channel: the recall tool writes the image content to a pending injection buffer on the tool context, and the channel's per-turn tool result processing checks this buffer and appends the image content to the next LLM turn.

This is the same mechanism used for any tool that needs to inject rich content (images, files) back into the LLM context after a tool call — the tool call result carries the text summary, and the injection buffer carries the actual `UserContent`.

**Tool output (text, for LLM context):**
```json
{
  "action": "get_content",
  "summary": "Image screenshot.png loaded for analysis (240 KB).",
  "attachment_id": "a1b2c3d4"
}
```

**Injected alongside the tool result:** `UserContent::Image` (base64 PNG)

The channel renders this as an additional image block in the same turn as the tool result, so the LLM sees both the tool confirmation and the image.

**Size limit:** 10 MB, same as today. Files larger than this return the metadata description and direct the channel to delegate to a worker with `get_path`.

**Non-image files:** No change. Text files are already returned inline correctly.

---

## 3. Portal Chat Upload

The portal composer needs a file input. Users should be able to attach files to messages in portal chat the same way they can on Telegram or Discord.

### Composer Changes

The `PortalComposer` wraps `ChatComposer` from `@spacedrive/ai`. File attachment needs to be surfaced at the portal layer:

- A paperclip button in the composer toolbar, left of the send button
- Clicking it opens a native file picker (no drag-and-drop in v1)
- Selected files show as chips above the text input: `[filename.png ×]`
- Files can be removed before sending by clicking ×
- Multiple files supported (same as Telegram)
- No file type or size restriction in the UI — the backend handles limits

### Sending Files

Files are uploaded to a new API endpoint before the message is sent. On send:

1. Upload each attachment file: `POST /agents/{agent_id}/channels/{channel_id}/attachments/upload`
2. Receive back an attachment reference with a temporary token
3. Send the message with the attachment references included in the message payload

**Upload endpoint:**
```
POST /agents/{agent_id}/channels/{channel_id}/attachments/upload
Content-Type: multipart/form-data

→ { id, original_filename, mime_type, size_bytes }
```

This uploads the file to `workspace/saved/` and creates the `saved_attachments` record immediately, before the message is sent. The returned `id` is included in the message payload so the channel can associate it with the conversation message.

**Message send payload** (extends existing send message API):
```json
{
  "content": "Here's that screenshot",
  "attachment_ids": ["a1b2c3d4-..."]
}
```

The channel picks up the attachment IDs, looks them up in `saved_attachments`, and injects them into the LLM turn as `UserContent` exactly as it does for platform-delivered attachments. No base64 is sent over the wire in the message payload — the files are already on disk.

### Portal Attachment Flow vs Platform Attachment Flow

Platform adapters (Telegram, Discord) download files from external URLs during message processing. Portal uploads are pre-uploaded. The channel processes them identically after that point — both paths produce `SavedAttachment` records and `UserContent` blocks.

---

## 4. Portal Timeline Display

Message bubbles in the portal timeline need to show attachments.

### Layout

Attachments render below the message text, above the timestamp:

```
┌─────────────────────────────────┐
│ Here's that screenshot          │
│                                 │
│ ┌─────────────────┐             │
│ │  [thumbnail]    │             │
│ │  screenshot.png │             │
│ │  240 KB         │             │
│ └─────────────────┘             │
│                         2:34 PM │
└─────────────────────────────────┘
```

**Images:** Thumbnail preview, click to open full-size in a lightbox or new tab.

**Other files:** Icon + filename + size. Click to download.

**Voice/audio:** Waveform placeholder or audio player. The transcription text is already in the message content (wrapped in `<voice_transcript>` tags) — strip the tags and render it as a quoted block below the player.

### Data Source

The portal timeline reads attachment metadata from the message's `metadata.attachments` array. This is already stored in `conversation_messages.metadata` for every message where `save_attachments` was enabled.

To display the actual file content (thumbnails, downloads), the timeline calls:

```
GET /agents/{agent_id}/attachments/{attachment_id}
→ streams the file from disk with correct Content-Type
```

Thumbnails use the same endpoint with a `?thumbnail=true` query param — the API generates a 200×200 JPEG on the fly for image files. For non-image files, `?thumbnail=true` returns a generic file-type icon.

### History Messages

Older messages that were sent before attachment persistence was enabled will have no `metadata.attachments`. These render normally with no attachment section. The `[Attachments: ...]` text annotation (added by history reconstruction for the LLM's benefit) should be stripped from the portal display — it's internal context for the LLM, not UI copy.

---

## 5. Attachment Serving API

Two new endpoints:

### `GET /agents/{agent_id}/attachments/{attachment_id}`

Streams the saved file from disk.

- Looks up `attachment_id` in `saved_attachments` for this agent's channels
- Streams `disk_path` with `Content-Type: {mime_type}`
- Adds `Content-Disposition: inline; filename="{original_filename}"` for browser display
- `?download=true` → `Content-Disposition: attachment` (forces download)
- `?thumbnail=true` → returns 200×200 JPEG thumbnail for image files; generic icon PNG for others
- Returns 404 if ID not found or file missing from disk

### `GET /agents/{agent_id}/channels/{channel_id}/attachments`

Lists saved attachments for a channel. Used by the portal timeline to fetch attachment metadata when loading history.

```json
{
  "attachments": [
    {
      "id": "a1b2c3d4-...",
      "original_filename": "screenshot.png",
      "mime_type": "image/png",
      "size_bytes": 245760,
      "message_id": "msg_xyz",
      "created_at": "2026-04-05T14:23:00Z"
    }
  ]
}
```

Optional query params: `?message_id={id}` to filter to attachments from a specific message, `?limit=N&before={created_at}` for pagination.

---

## Summary of Changes

| Change | Location | Notes |
|---|---|---|
| Default `save_attachments = true` | `src/config/types.rs` | One-line change |
| Image re-injection in recall tool | `src/tools/attachment_recall.rs` + channel turn loop | Injection buffer mechanism |
| File upload endpoint | `src/api/channels.rs` | New route |
| Attachment serve endpoint | `src/api/` | New route, thumbnail generation |
| Attachment list endpoint | `src/api/channels.rs` | New route |
| Portal composer file picker | `interface/src/components/portal/PortalComposer.tsx` | File input + chips UI |
| Portal timeline attachment display | `interface/src/components/portal/PortalTimeline.tsx` | Thumbnail/file rendering |
| Voice transcript display | Portal timeline | Strip `<voice_transcript>` tags, render as quote |
