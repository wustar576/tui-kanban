-- tui-kanban schema, version 5: comments from the human and the background waiter
-- (docs/board-v2.md section 13). Purely additive:
--   - card_note.kind accepts one more value, 'comment';
--   - new nullable columns: card_note.delivered_at, card_note.announced_at,
--     question.announced_at;
--   - a new table kb_waiter and a new function kb_add_comment.
-- Binaries built for versions 1 to 4 keep working against this schema, except that they
-- cannot decode a note of kind 'comment' (they list notes with a fixed set of kinds).

-- --- Comments --------------------------------------------------------------------------------

ALTER TABLE card_note DROP CONSTRAINT card_note_kind_check;
ALTER TABLE card_note ADD CONSTRAINT card_note_kind_check
    CHECK (kind IN ('progress', 'decision', 'blocker', 'result', 'reply', 'comment'));

-- When the agent received a comment (kb inbox or a delivering hook), and when the waiter
-- woke the session for it. Only comments use them.
ALTER TABLE card_note
    ADD COLUMN delivered_at timestamptz,
    ADD COLUMN announced_at timestamptz;

-- Undelivered comments of a board are looked up often (every hook); keep that cheap.
CREATE INDEX card_note_comment_idx ON card_note (card_id, id) WHERE kind = 'comment';

-- --- The waiter (docs/board-v2.md sections 13.3 and 13.4) -----------------------------------

-- When the waiter woke the session for this answer. Never "delivered".
ALTER TABLE question ADD COLUMN announced_at timestamptz;

-- One row per session: who is waiting. Mutual exclusion itself is a session-level advisory
-- lock held on the waiter's connection; this row records the holder so that a new waiter
-- can tell "same Claude Code process" (exit) from "another one" (take over).
CREATE TABLE kb_waiter (
    session_key  text        PRIMARY KEY,
    token        text        NOT NULL,
    waiter_pid   bigint      NOT NULL,
    claude_pid   bigint,
    started_at   timestamptz NOT NULL DEFAULT now()
);

-- --- Adding a comment (the TUI's second write) ----------------------------------------------
--
-- Runs with the owner's privileges, so kb_reader can comment without any table privilege.
-- search_path is pinned and every table is schema-qualified. Error messages are part of the
-- contract (docs/board-v2.md section 13.7); the kb-core comment module maps them to typed
-- errors. Returns the id of the new note.
CREATE FUNCTION kb_add_comment(p_card_id bigint, p_text text)
RETURNS bigint
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, public, pg_temp
AS $$
DECLARE
    v_board_id  bigint;
    v_board     record;
    v_seq       integer;
    v_text      text;
    v_id        bigint;
BEGIN
    SELECT board_id, seq INTO v_board_id, v_seq FROM public.card WHERE id = p_card_id;
    IF NOT FOUND THEN
        RAISE EXCEPTION 'card not found';
    END IF;

    -- Same lock order as kb writers: the board first.
    SELECT id, status, turn_key INTO v_board FROM public.board WHERE id = v_board_id FOR UPDATE;
    IF v_board.status = 'archived' THEN
        RAISE EXCEPTION 'board is archived';
    END IF;

    -- Line breaks become spaces; then trim white space, including the ideographic space.
    v_text := btrim(
        regexp_replace(coalesce(p_text, ''), E'\r\n|\r|\n', ' ', 'g'),
        E' \t　'
    );
    IF v_text = '' THEN
        RAISE EXCEPTION 'comment text is empty';
    END IF;
    IF char_length(v_text) > 500 THEN
        RAISE EXCEPTION 'comment text is too long';
    END IF;

    INSERT INTO public.card_note (card_id, kind, body, author)
    VALUES (p_card_id, 'comment', v_text, 'user')
    RETURNING id INTO v_id;
    UPDATE public.card SET updated_at = now() WHERE id = p_card_id;

    INSERT INTO public.activity (board_id, card_id, actor, verb, detail, turn_key)
    VALUES (
        v_board_id, p_card_id, 'user', 'comment.add',
        jsonb_build_object('note', v_id, 'seq', v_seq, 'excerpt', left(v_text, 80)),
        v_board.turn_key
    );

    UPDATE public.board SET rev = rev + 1, updated_at = now() WHERE id = v_board_id;
    RETURN v_id;
END;
$$;

-- Only the kb roles may comment. The grants to kb_writer and kb_reader are made by
-- "kb db migrate" (role names come from the config).
REVOKE ALL ON FUNCTION kb_add_comment(bigint, text) FROM PUBLIC;
