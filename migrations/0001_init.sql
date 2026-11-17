-- tui-kanban schema, version 1. See docs/data-model.md.

CREATE TABLE workspace (
    id          bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    key         text        NOT NULL UNIQUE,
    name        text        NOT NULL,
    source      text        NOT NULL CHECK (source IN ('herdr', 'dir', 'manual')),
    created_at  timestamptz NOT NULL DEFAULT now(),
    updated_at  timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE board (
    id              bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    workspace_id    bigint      NOT NULL REFERENCES workspace (id),
    session_key     text        NOT NULL UNIQUE,
    title           text        NOT NULL DEFAULT '',
    goal            text        NOT NULL DEFAULT '',
    status          text        NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'archived')),
    outcome         text        CHECK (outcome IN ('completed', 'incomplete', 'abandoned')),
    summary         text,
    herdr_space     text,
    herdr_tab       text,
    herdr_terminal  text,
    cwd             text,
    prev_board_id   bigint      REFERENCES board (id),
    next_card_seq   integer     NOT NULL DEFAULT 1,
    rev             bigint      NOT NULL DEFAULT 0,
    created_at      timestamptz NOT NULL DEFAULT now(),
    updated_at      timestamptz NOT NULL DEFAULT now(),
    archived_at     timestamptz
);

CREATE INDEX board_workspace_idx ON board (workspace_id, updated_at DESC);

CREATE TABLE lane (
    id          bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    board_id    bigint  NOT NULL REFERENCES board (id),
    name        text    NOT NULL,
    position    integer NOT NULL CHECK (position >= 1),
    kind        text    NOT NULL CHECK (kind IN ('todo', 'doing', 'done', 'dropped')),
    wip_limit   integer CHECK (wip_limit > 0),
    CONSTRAINT lane_name_uniq UNIQUE (board_id, name),
    -- Deferred so that reordering lanes can pass through temporary duplicates.
    CONSTRAINT lane_position_uniq UNIQUE (board_id, position) DEFERRABLE INITIALLY DEFERRED,
    -- Target of card's composite foreign key (a card's lane must be on the card's board).
    CONSTRAINT lane_id_board_uniq UNIQUE (id, board_id)
);

CREATE TABLE card (
    id              bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    board_id        bigint      NOT NULL REFERENCES board (id),
    seq             integer     NOT NULL,
    lane_id         bigint      NOT NULL,
    title           text        NOT NULL,
    body            text        NOT NULL DEFAULT '',
    priority        smallint    NOT NULL DEFAULT 2 CHECK (priority BETWEEN 0 AND 3),
    position        integer     NOT NULL,
    owner           text        NOT NULL DEFAULT 'agent',
    blocked_reason  text,
    result          text,
    created_at      timestamptz NOT NULL DEFAULT now(),
    updated_at      timestamptz NOT NULL DEFAULT now(),
    closed_at       timestamptz,
    CONSTRAINT card_seq_uniq UNIQUE (board_id, seq),
    CONSTRAINT card_lane_fk FOREIGN KEY (lane_id, board_id) REFERENCES lane (id, board_id)
);

CREATE INDEX card_lane_order_idx ON card (lane_id, priority, position);

CREATE TABLE card_note (
    id          bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    card_id     bigint      NOT NULL REFERENCES card (id),
    kind        text        NOT NULL CHECK (kind IN ('progress', 'decision', 'blocker', 'result', 'reply')),
    body        text        NOT NULL,
    author      text        NOT NULL DEFAULT 'agent',
    created_at  timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX card_note_card_idx ON card_note (card_id, id);

CREATE TABLE link (
    id            bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    board_id      bigint      NOT NULL REFERENCES board (id),
    from_card_id  bigint      NOT NULL REFERENCES card (id),
    kind          text        NOT NULL CHECK (kind IN ('spawned', 'blocks', 'relates', 'continues', 'ref')),
    to_card_id    bigint      REFERENCES card (id),
    target        text,
    label         text,
    created_at    timestamptz NOT NULL DEFAULT now(),
    -- Exactly one of to_card_id / target; target iff kind = 'ref'.
    CONSTRAINT link_target_chk CHECK ((to_card_id IS NULL) <> (target IS NULL)),
    CONSTRAINT link_ref_chk CHECK ((kind = 'ref') = (target IS NOT NULL)),
    CONSTRAINT link_self_chk CHECK (to_card_id IS NULL OR to_card_id <> from_card_id),
    CONSTRAINT link_uniq UNIQUE NULLS NOT DISTINCT (from_card_id, kind, to_card_id, target)
);

CREATE INDEX link_from_idx ON link (from_card_id);
CREATE INDEX link_to_idx ON link (to_card_id);

CREATE TABLE activity (
    id        bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    board_id  bigint      NOT NULL REFERENCES board (id),
    card_id   bigint      REFERENCES card (id),
    at        timestamptz NOT NULL DEFAULT now(),
    actor     text        NOT NULL DEFAULT 'agent',
    verb      text        NOT NULL,
    detail    jsonb       NOT NULL DEFAULT '{}'::jsonb,
    turn_key  text
);

CREATE INDEX activity_board_idx ON activity (board_id, id DESC);

-- Change notification: {"workspace": <id>, "board": <id>, "rev": <n>} on channel kb_events.
--
-- The trigger is a deferred constraint trigger, so it runs at commit time and reads the
-- board's final state. Every event of one transaction therefore produces the same payload,
-- and PostgreSQL folds identical payloads of one transaction into a single notification:
-- one write command = one notification carrying the committed rev.
CREATE FUNCTION kb_board_notify() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
    b record;
BEGIN
    SELECT id, workspace_id, rev INTO b FROM board WHERE id = NEW.id;
    IF FOUND THEN
        PERFORM pg_notify(
            'kb_events',
            json_build_object('workspace', b.workspace_id, 'board', b.id, 'rev', b.rev)::text
        );
    END IF;
    RETURN NULL;
END;
$$;

CREATE CONSTRAINT TRIGGER board_notify_insert
    AFTER INSERT ON board
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW EXECUTE FUNCTION kb_board_notify();

CREATE CONSTRAINT TRIGGER board_notify_update
    AFTER UPDATE ON board
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW
    WHEN (OLD.rev IS DISTINCT FROM NEW.rev OR OLD.status IS DISTINCT FROM NEW.status)
    EXECUTE FUNCTION kb_board_notify();
