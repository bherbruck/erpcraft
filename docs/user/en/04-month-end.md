# 4. Month-end

Work through these in order once the month's documents are in.

1. **Bank.** Import the month's bank statements (`bank_statement.import`), match every line (`bank_statement.match`) and post the statement (`bank_statement.post`). Unmatched money is posted to an account or explained.
2. **Open items.** *Open items → Aging* (`open_item.aging`) lists what customers owe and what you owe, by days overdue. The totals must equal the receivables and payables accounts; the report says so.
3. **Goods received not invoiced.** Receipts without their invoice stay on that account: chase the vendors, or accrue.
4. **Foreign currency.** *Exchange revaluation* (`fx_revaluation.post`) values open items in other currencies at the month's closing rate.
5. **Depreciation.** *Fixed assets → Depreciation run* (`depreciation_run.post`), dated the month's last day.
6. **Stock.** Count what needs counting (`stock_count.start`, `review`, `post`); differences are booked as adjustments.
7. **Tax.** *Tax report* (`tax.report`) per code, or per place for US and Canadian returns; *OSS return* (`tax.oss_report`) for EU distance sales.
8. **Reports.**
   - the *Trial balance* (`gl.trial_balance`) must balance;
   - the *Income statement*, *Balance sheet* and *Cash flow statement* (`gl.cash_flow`) are the month's results;
   - *Budget against actual* (`budget.report`) shows variances.
9. **Close the month.** *Fiscal periods → Close* (`fiscal_period.close`). Nothing more can be posted into it; reopen it (`fiscal_period.reopen`) if you must correct something.

## At the year's end

Post a **year-end close** (`year_end_close.post`) dated the last day of the year. It moves the year's result to retained earnings and closes every month of the year. Reverse it (`year_end_close.reverse`) to correct a closed year, then close again.
