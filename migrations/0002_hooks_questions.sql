-- tui-kanban schema, version 2: turns (docs/hooks.md section 7) and question cards
-- (docs/questions.md section 4). Purely additive: new nullable columns, new tables, a new
-- function. Binaries built for version 1 keep working against this schema.

-- --- Turns ---------------------------------------------------------------------------------

ALTER TABLE board
    ADD COLUMN turn_key        text,
    ADD COLUMN turn_started_at timestamptz;

-- Rule checks read "this board's activity since the turn started".
CREATE INDEX activity_board_at_idx ON activity (board_id, at);

-- --- Questions -----------------------------------------------------------------------------

CREATE TABLE question (
    id            bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    board_id      bigint      NOT NULL REFERENCES board (id),
    card_id       bigint      NOT NULL REFERENCES card (id),
    text          text        NOT NULL,
    multi         boolean     NOT NULL DEFAULT false,
    status        text        NOT NULL DEFAULT 'open'
                              CHECK (status IN ('open', 'answered', 'chat', 'cancelled')),
    asked_by      text        NOT NULL DEFAULT 'agent',
    asked_at      timestamptz NOT NULL DEFAULT now(),
    answered_at   timestamptz,
    delivered_at  timestamptz
);

CREATE INDEX question_board_idx ON question (board_id, status);
CREATE INDEX question_card_idx ON question (card_id);

CREATE TABLE question_option (
    id           bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    question_id  bigint  NOT NULL REFERENCES question (id),
    position     integer NOT NULL CHECK (position >= 1),
    label        text    NOT NULL,
    description  text,
    selected     boolean NOT NULL DEFAULT false,
    CONSTRAINT question_option_position_uniq UNIQUE (question_id, position)
);

-- --- Answering (the only write the TUI makes) ---------------------------------------------
--
-- Runs with the owner's privileges, so kb_reader can answer without any table privilege.
-- search_path is pinned and every table is schema-qualified, so callers cannot substitute
-- objects. Error messages are part of the contract (docs/questions.md section 4.3); the
-- kb-core answer module maps them to typed errors.
CREATE FUNCTION kb_answer_question(p_question_id bigint, p_option_ids bigint[], p_chat boolean)
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
        UPDATE public.question SET status = 'chat', answered_at = now() WHERE id = p_question_id;
        v_detail := jsonb_build_object('question', p_question_id, 'seq', v_seq, 'chat', true);
    ELSE
        SELECT coalesce(array_agg(DISTINCT x), '{}') INTO v_ids
        FROM unnest(coalesce(p_option_ids, '{}'::bigint[])) AS x
        WHERE x IS NOT NULL;
        IF cardinality(v_ids) = 0 THEN
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
        UPDATE public.question SET status = 'answered', answered_at = now()
        WHERE id = p_question_id;
        SELECT jsonb_agg(label ORDER BY position) INTO v_labels
        FROM public.question_option
        WHERE question_id = p_question_id AND id = ANY (v_ids);
        v_detail := jsonb_build_object('question', p_question_id, 'seq', v_seq, 'labels', v_labels);
    END IF;

    INSERT INTO public.activity (board_id, card_id, actor, verb, detail, turn_key)
    VALUES (v_board_id, v_question.card_id, 'user', 'question.answer', v_detail, v_board.turn_key);

    UPDATE public.board SET rev = rev + 1, updated_at = now() WHERE id = v_board_id;
END;
$$;

-- Functions are executable by PUBLIC by default; only the kb roles may answer. The grants
-- to kb_writer and kb_reader are made by "kb db migrate" (role names come from the config).
REVOKE ALL ON FUNCTION kb_answer_question(bigint, bigint[], boolean) FROM PUBLIC;
