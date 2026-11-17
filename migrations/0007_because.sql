-- tui-kanban schema, version 7: what a decision follows from (docs/board-v2.md section 18).
-- Purely additive: one new table. Binaries built for versions 1 to 6 never read it and keep
-- working against this schema.
--
-- A decision is one write: kb card note, kb card drop or kb card done. Its record is the
-- activity row of that write (card.note, card.drop with its reason, card.done with its
-- result), because drop and done keep their text only in card.result, which the next drop or
-- done overwrites. A note decision also names its note.
--
-- Grants: "kb db migrate" grants SELECT on all tables to the reader role and read/write on
-- all tables to the writer role every time it runs, so this table needs no statement here
-- (role names come from the config, not from this file).

CREATE TABLE cause (
    id                 bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    board_id           bigint      NOT NULL REFERENCES board (id),
    -- The card that made the decision.
    card_id            bigint      NOT NULL REFERENCES card (id),
    -- The decision: its activity row, and its note for kb card note.
    activity_id        bigint      NOT NULL REFERENCES activity (id),
    note_id            bigint      REFERENCES card_note (id),
    -- What it follows from. 'user': the user's instruction in the conversation, no target.
    kind               text        NOT NULL CHECK (kind IN ('card', 'question', 'comment', 'user')),
    cause_card_id      bigint      REFERENCES card (id),
    cause_question_id  bigint      REFERENCES question (id),
    cause_note_id      bigint      REFERENCES card_note (id),
    created_at         timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT cause_target_chk CHECK (
        (kind = 'card') = (cause_card_id IS NOT NULL)
        AND (kind = 'question') = (cause_question_id IS NOT NULL)
        AND (kind = 'comment') = (cause_note_id IS NOT NULL)
    ),
    CONSTRAINT cause_uniq UNIQUE NULLS NOT DISTINCT
        (activity_id, kind, cause_card_id, cause_question_id, cause_note_id)
);

-- Forward: the causes of a card's decisions.
CREATE INDEX cause_card_idx ON cause (card_id);
-- Reverse: what a card, a question or a comment led to.
CREATE INDEX cause_cause_card_idx ON cause (cause_card_id) WHERE cause_card_id IS NOT NULL;
CREATE INDEX cause_cause_question_idx ON cause (cause_question_id) WHERE cause_question_id IS NOT NULL;
CREATE INDEX cause_cause_note_idx ON cause (cause_note_id) WHERE cause_note_id IS NOT NULL;
