-- tui-kanban schema, version 6: the last "next:" line shown to the session's agent
-- (docs/board-v2.md section 15.2). Purely additive: one nullable column.
-- Writing it never changes rev or status, so the kb_events trigger does not fire and the
-- TUI does not redraw. Binaries built for versions 1 to 5 keep working against this schema.

ALTER TABLE board ADD COLUMN last_next text;
