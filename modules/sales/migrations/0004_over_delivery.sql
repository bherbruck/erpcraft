-- Whether more may be delivered than was ordered is now a policy of the
-- company (the policy point sales_delivery.over_delivery), decided when a
-- delivery is posted. The default still refuses it, but the database no
-- longer does: an order line can be delivered beyond its quantity where
-- the company allows it. Never more is invoiced than was delivered; that
-- constraint stays.

DROP TRIGGER sales_order_progress_check ON sales_order_progress;
DROP FUNCTION sales_order_progress_check();
