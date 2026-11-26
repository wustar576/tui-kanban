-- tui-kanban schema, version 8: kb no longer depends on a terminal program
-- (docs/board-v2.md section 24). The terminal fields come from the optional variables
-- KB_TERMINAL_ID, KB_TERMINAL_SPACE and KB_TERMINAL_TAB, so their columns and the
-- workspace source get neutral names.
--
-- Not additive: binaries built for versions 1 to 7 read the old column names and stop
-- working against this schema. Upgrade the binary together with the schema.
--
-- No function, trigger or view of versions 1 to 7 references the renamed columns or the
-- value 'herdr' (kb_board_notify reads id, workspace_id and rev only), so none is
-- recreated here.

ALTER TABLE board RENAME COLUMN herdr_space TO terminal_space;
ALTER TABLE board RENAME COLUMN herdr_tab TO terminal_tab;
ALTER TABLE board RENAME COLUMN herdr_terminal TO terminal_id;

-- workspace.source: 'herdr' becomes 'space'. The CHECK of version 1 is an unnamed column
-- constraint; drop it by looking up its name instead of assuming one.
DO $$
DECLARE
    c record;
BEGIN
    FOR c IN
        SELECT con.conname
        FROM pg_constraint con
        JOIN pg_attribute att
          ON att.attrelid = con.conrelid AND att.attnum = ANY (con.conkey)
        WHERE con.conrelid = 'workspace'::regclass
          AND con.contype = 'c'
          AND att.attname = 'source'
    LOOP
        EXECUTE format('ALTER TABLE workspace DROP CONSTRAINT %I', c.conname);
    END LOOP;
END;
$$;

-- Workspace keys of herdr spaces: herdr:<id> becomes space:<id>.
UPDATE workspace SET source = 'space' WHERE source = 'herdr';
UPDATE workspace
SET key = 'space:' || substr(key, length('herdr:') + 1)
WHERE key LIKE 'herdr:%';

ALTER TABLE workspace
    ADD CONSTRAINT workspace_source_check CHECK (source IN ('space', 'dir', 'manual'));
