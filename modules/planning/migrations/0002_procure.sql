-- Whether an item is bought or made is a choice on the reorder rule:
-- `buy` or `make`. Left empty, planning decides: bought when the rule names
-- a vendor, made when the item has an active bill of materials and the
-- rule names no vendor, bought otherwise. What the rule says always wins.
ALTER TABLE reorder_rule
    ADD COLUMN procure text CHECK (procure IN ('buy', 'make'));
