-- tui-kanban schema, version 3: epics and card origin (docs/board-v2.md section 3).
-- Purely additive: a new table and two new card columns with safe defaults. Binaries built
-- for versions 1 and 2 keep working: an INSERT into card that does not name the new columns
-- gets epic_id = NULL and origin = 'derived'.

-- --- Epics ---------------------------------------------------------------------------------

CREATE TABLE epic (
    id          bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    board_id    bigint      NOT NULL REFERENCES board (id),
    name        text        NOT NULL,
    position    integer     NOT NULL CHECK (position >= 1),
    summary     text,
    created_at  timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT epic_name_uniq UNIQUE (board_id, name),
    -- Deferred so that reordering epics can pass through temporary duplicates.
    CONSTRAINT epic_position_uniq UNIQUE (board_id, position) DEFERRABLE INITIALLY DEFERRED,
    -- Target of card's composite foreign key (a card's epic must be on the card's board).
    CONSTRAINT epic_id_board_uniq UNIQUE (id, board_id)
);

-- --- Card epic and origin ------------------------------------------------------------------

-- The column is added with default 'agreed' so every existing card is backfilled with it;
-- the default then becomes 'derived' for cards inserted from now on.
ALTER TABLE card
    ADD COLUMN epic_id bigint,
    ADD COLUMN origin  text NOT NULL DEFAULT 'agreed'
        CONSTRAINT card_origin_chk CHECK (origin IN ('discussing', 'agreed', 'derived')),
    ADD CONSTRAINT card_epic_fk FOREIGN KEY (epic_id, board_id) REFERENCES epic (id, board_id);

ALTER TABLE card ALTER COLUMN origin SET DEFAULT 'derived';

CREATE INDEX card_epic_idx ON card (epic_id);
