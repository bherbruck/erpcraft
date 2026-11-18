-- Ending a delegation keeps its record: when it was ended is noted, and a
-- delegation ended counts no more, whatever its dates. Until now ending
-- one that had not yet begun, or began that day, deleted it, and with it
-- the record of who had been allowed to decide in whose place.
ALTER TABLE approval_delegation ADD COLUMN ended_at timestamptz;
