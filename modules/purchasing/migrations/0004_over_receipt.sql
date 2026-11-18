-- Whether more may be received than was ordered is now a policy of the
-- company (the policy point purchase_receipt.over_receipt), decided when a
-- receipt is posted. The default still refuses it, but the database no
-- longer does: an order line can be received beyond its quantity where the
-- company allows it.
--
-- The other constraints on purchase_order_progress stay, and they are what
-- keeps the books right: never more invoiced than received, and nothing
-- left in goods received not invoiced once everything received is invoiced.

DROP TRIGGER purchase_order_progress_check ON purchase_order_progress;
DROP FUNCTION purchase_order_progress_check();
