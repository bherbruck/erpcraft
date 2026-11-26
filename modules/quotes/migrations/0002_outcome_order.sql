-- The draft order a won quote made can be discarded like any draft: the
-- quote stays won, and no longer names an order.
ALTER TABLE sales_quote_outcome DROP CONSTRAINT sales_quote_outcome_order_id_fkey;
ALTER TABLE sales_quote_outcome ADD CONSTRAINT sales_quote_outcome_order_id_fkey
    FOREIGN KEY (order_id) REFERENCES document (id) ON DELETE SET NULL;

-- An outcome is written once; only the order it names may go, with the
-- order.
DROP TRIGGER sales_quote_outcome_immutable ON sales_quote_outcome;
CREATE FUNCTION sales_quote_outcome_guard() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF (NEW.document_id, NEW.outcome, NEW.reason, NEW.created_at)
           IS DISTINCT FROM (OLD.document_id, OLD.outcome, OLD.reason, OLD.created_at)
       OR NEW.order_id IS NOT NULL AND NEW.order_id IS DISTINCT FROM OLD.order_id THEN
        RAISE EXCEPTION 'how a quote ended is written once'
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER sales_quote_outcome_guard BEFORE UPDATE ON sales_quote_outcome
    FOR EACH ROW EXECUTE FUNCTION sales_quote_outcome_guard();
