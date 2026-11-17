-- tui-kanban schema, version 4: a typed answer to a question (docs/board-v2.md section 7.1).
-- Purely additive: one nullable column, and kb_answer_question gains a fourth parameter with
-- a default. Binaries built for versions 1 to 3 keep working:
--   - they never name question.answer_text;
--   - their three-argument call kb_answer_question(id, options, chat) resolves to the new
--     function with p_text = NULL, which behaves exactly like the old one.
-- The old three-argument function is dropped rather than overloaded, so a three-argument call
-- is never ambiguous.

ALTER TABLE question ADD COLUMN answer_text text;

DROP FUNCTION kb_answer_question(bigint, bigint[], boolean);

-- Runs with the owner's privileges, so kb_reader can answer without any table privilege.
-- search_path is pinned and every table is schema-qualified. Error messages are part of the
-- contract; the kb-core answer module maps them to typed errors.
CREATE FUNCTION kb_answer_question(
    p_question_id bigint,
    p_option_ids  bigint[],
    p_chat        boolean,
    p_text        text DEFAULT NULL
)
RETURNS void
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, public, pg_temp
AS $$
DECLARE
    v_board_id  bigint;
    v_board     record;
    v_question  record;
    v_seq       integer;
    v_ids       bigint[];
    v_matched   integer;
    v_labels    jsonb;
    v_detail    jsonb;
    v_text      text;
BEGIN
    SELECT board_id INTO v_board_id FROM public.question WHERE id = p_question_id;
    IF NOT FOUND THEN
        RAISE EXCEPTION 'question not found';
    END IF;

    -- Same lock order as kb writers: board first, then the question.
    SELECT id, status, turn_key INTO v_board FROM public.board WHERE id = v_board_id FOR UPDATE;
    SELECT * INTO v_question FROM public.question WHERE id = p_question_id FOR UPDATE;

    IF v_question.status <> 'open' THEN
        RAISE EXCEPTION 'question is not open';
    END IF;
    IF v_board.status = 'archived' THEN
        RAISE EXCEPTION 'board is archived';
    END IF;

    SELECT seq INTO v_seq FROM public.card WHERE id = v_question.card_id;

    IF coalesce(p_chat, false) THEN
        -- 「在 session 裡說明」: the text is ignored.
        UPDATE public.question SET status = 'chat', answered_at = now() WHERE id = p_question_id;
        v_detail := jsonb_build_object('question', p_question_id, 'seq', v_seq, 'chat', true);
    ELSE
        -- The text without leading and trailing white space (including the ideographic
        -- space); empty counts as no text.
        v_text := nullif(btrim(p_text, E' \t\r\n　'), '');
        IF char_length(v_text) > 500 THEN
            RAISE EXCEPTION 'answer text is too long';
        END IF;
        SELECT coalesce(array_agg(DISTINCT x), '{}') INTO v_ids
        FROM unnest(coalesce(p_option_ids, '{}'::bigint[])) AS x
        WHERE x IS NOT NULL;
        IF cardinality(v_ids) = 0 AND v_text IS NULL THEN
            RAISE EXCEPTION 'no option selected';
        END IF;
        SELECT count(*) INTO v_matched
        FROM public.question_option
        WHERE question_id = p_question_id AND id = ANY (v_ids);
        IF v_matched <> cardinality(v_ids) THEN
            RAISE EXCEPTION 'option does not belong to question';
        END IF;
        IF NOT v_question.multi AND cardinality(v_ids) > 1 THEN
            RAISE EXCEPTION 'question accepts one option';
        END IF;
        UPDATE public.question_option SET selected = true
        WHERE question_id = p_question_id AND id = ANY (v_ids);
        UPDATE public.question SET status = 'answered', answered_at = now(), answer_text = v_text
        WHERE id = p_question_id;
        SELECT coalesce(jsonb_agg(label ORDER BY position), '[]'::jsonb) INTO v_labels
        FROM public.question_option
        WHERE question_id = p_question_id AND id = ANY (v_ids);
        v_detail := jsonb_build_object('question', p_question_id, 'seq', v_seq, 'labels', v_labels);
        IF v_text IS NOT NULL THEN
            v_detail := v_detail || jsonb_build_object('text', left(v_text, 80));
        END IF;
    END IF;

    INSERT INTO public.activity (board_id, card_id, actor, verb, detail, turn_key)
    VALUES (v_board_id, v_question.card_id, 'user', 'question.answer', v_detail, v_board.turn_key);

    UPDATE public.board SET rev = rev + 1, updated_at = now() WHERE id = v_board_id;
END;
$$;

-- Functions are executable by PUBLIC by default; only the kb roles may answer. The grants
-- to kb_writer and kb_reader are made by "kb db migrate" (role names come from the config).
REVOKE ALL ON FUNCTION kb_answer_question(bigint, bigint[], boolean, text) FROM PUBLIC;
