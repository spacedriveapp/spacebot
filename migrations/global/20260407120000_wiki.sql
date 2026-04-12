-- Instance-wide wiki: shared knowledge base across all agents and users.
-- Every edit is versioned permanently — nothing is ever deleted.

CREATE TABLE IF NOT EXISTS wiki_pages (
    id          TEXT PRIMARY KEY DEFAULT (lower(hex(randomblob(16)))),
    slug        TEXT NOT NULL UNIQUE,
    title       TEXT NOT NULL,
    page_type   TEXT NOT NULL CHECK (page_type IN ('entity', 'concept', 'decision', 'project', 'reference')),
    content     TEXT NOT NULL DEFAULT '',
    related     TEXT NOT NULL DEFAULT '[]',    -- JSON array of related page slugs
    created_by  TEXT NOT NULL,                 -- agent_id or user_id
    updated_by  TEXT NOT NULL,
    version     INTEGER NOT NULL DEFAULT 1,
    archived    INTEGER NOT NULL DEFAULT 0,
    created_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

CREATE TABLE IF NOT EXISTS wiki_page_versions (
    id           TEXT PRIMARY KEY DEFAULT (lower(hex(randomblob(16)))),
    page_id      TEXT NOT NULL REFERENCES wiki_pages(id),
    version      INTEGER NOT NULL,
    content      TEXT NOT NULL,
    edit_summary TEXT,
    author_type  TEXT NOT NULL CHECK (author_type IN ('agent', 'user')),
    author_id    TEXT NOT NULL,
    created_at   TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    UNIQUE (page_id, version)
);

-- Full-text search across title + content
CREATE VIRTUAL TABLE IF NOT EXISTS wiki_pages_fts USING fts5(
    slug UNINDEXED,
    title,
    content,
    content='wiki_pages',
    content_rowid='rowid'
);

-- Keep FTS in sync
CREATE TRIGGER IF NOT EXISTS wiki_pages_fts_insert AFTER INSERT ON wiki_pages BEGIN
    INSERT INTO wiki_pages_fts(rowid, slug, title, content) VALUES (new.rowid, new.slug, new.title, new.content);
END;

CREATE TRIGGER IF NOT EXISTS wiki_pages_fts_update AFTER UPDATE ON wiki_pages BEGIN
    INSERT INTO wiki_pages_fts(wiki_pages_fts, rowid, slug, title, content) VALUES ('delete', old.rowid, old.slug, old.title, old.content);
    INSERT INTO wiki_pages_fts(rowid, slug, title, content) VALUES (new.rowid, new.slug, new.title, new.content);
END;

CREATE TRIGGER IF NOT EXISTS wiki_pages_fts_delete AFTER DELETE ON wiki_pages BEGIN
    INSERT INTO wiki_pages_fts(wiki_pages_fts, rowid, slug, title, content) VALUES ('delete', old.rowid, old.slug, old.title, old.content);
END;

CREATE INDEX IF NOT EXISTS wiki_pages_type    ON wiki_pages(page_type);
CREATE INDEX IF NOT EXISTS wiki_pages_updated ON wiki_pages(updated_at DESC);
CREATE INDEX IF NOT EXISTS wiki_pages_archived ON wiki_pages(archived);
CREATE INDEX IF NOT EXISTS wiki_versions_page ON wiki_page_versions(page_id, version DESC);
