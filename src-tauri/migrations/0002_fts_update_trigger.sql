-- Rebuild FTS rows only for updates that write an indexed column (search_text or note).
DROP TRIGGER IF EXISTS clipboard_items_au;
CREATE TRIGGER clipboard_items_au AFTER UPDATE OF search_text, note ON clipboard_items BEGIN
    INSERT INTO clipboard_items_fts(clipboard_items_fts, rowid, search_text, note)
    VALUES ('delete', old.rowid, old.search_text, old.note);
    INSERT INTO clipboard_items_fts(rowid, search_text, note)
    VALUES (new.rowid, new.search_text, new.note);
END;
