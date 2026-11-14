-- Protocol 1.1.6: the stage (and mode) draft of a match (JSON), kept with the match for its history.
ALTER TABLE matches ADD COLUMN draft TEXT;
