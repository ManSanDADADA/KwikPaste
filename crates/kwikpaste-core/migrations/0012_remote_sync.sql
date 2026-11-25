-- Remote sync through a sync server (sync/REMOTE.md). Records received from the server keep the
-- origin device's item key `<kind>:<content_hash>` here: images are recompressed on the way, so the
-- receiver's own hash differs. Records captured on this device leave it NULL (their key is their own
-- kind and hash).
ALTER TABLE clipboard_items ADD COLUMN sync_key TEXT;

CREATE INDEX idx_clipboard_items_sync_key
    ON clipboard_items (sync_key) WHERE sync_key IS NOT NULL;

-- Local last-writer-wins state per remote entry id: the latest value and HLC of metadata and
-- settings entries, delete tombstones, metadata buffered for records not received yet, and the last
-- HLC seen for each record. No foreign key: tombstones outlive the rows they deleted.
CREATE TABLE remote_sync_state (
    entry_id   TEXT PRIMARY KEY NOT NULL,
    kind       TEXT NOT NULL,
    item_key   TEXT,
    field      TEXT NOT NULL,
    value      TEXT,
    hlc        TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE INDEX idx_remote_sync_state_item_key
    ON remote_sync_state (item_key) WHERE item_key IS NOT NULL;
