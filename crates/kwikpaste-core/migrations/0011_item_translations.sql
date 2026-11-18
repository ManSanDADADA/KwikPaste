CREATE TABLE item_translations (
    item_id TEXT PRIMARY KEY NOT NULL REFERENCES clipboard_items(id) ON DELETE CASCADE,
    source_hash TEXT NOT NULL,
    text TEXT NOT NULL,
    source_lang TEXT,
    target_lang TEXT NOT NULL,
    provider TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
