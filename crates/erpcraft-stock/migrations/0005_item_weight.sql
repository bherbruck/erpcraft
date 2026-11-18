-- What one unit of an item weighs, in the company's unit of weight
-- (kilograms, say): what a landed cost such as freight is shared out by
-- when it is allocated by weight. Optional.
ALTER TABLE item ADD COLUMN weight numeric(20, 6) CHECK (weight > 0);
