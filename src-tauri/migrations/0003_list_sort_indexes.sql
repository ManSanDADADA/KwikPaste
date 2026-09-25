-- Let list paging walk an index in sort order instead of scanning and sorting the whole table.
-- The composite columns mirror the ORDER BY built in db/items.rs (is_pinned DESC, then time DESC).

-- "All" tab with the default updated_at sort, including quick paste lookups.
CREATE INDEX IF NOT EXISTS idx_clipboard_items_pinned_updated
    ON clipboard_items (is_pinned, updated_at, created_at);

-- created_at sort, plus retention / max count / storage cleanup (is_pinned = 0 ... ORDER BY created_at).
CREATE INDEX IF NOT EXISTS idx_clipboard_items_pinned_created
    ON clipboard_items (is_pinned, created_at);

-- Text / image / files tabs with the default sort.
CREATE INDEX IF NOT EXISTS idx_clipboard_items_kind_pinned_updated
    ON clipboard_items (kind, is_pinned, updated_at, created_at);

-- Custom group view with the default sort.
CREATE INDEX IF NOT EXISTS idx_clipboard_items_group_pinned_updated
    ON clipboard_items (group_id, is_pinned, updated_at, created_at);
