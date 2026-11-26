# 1. Setting up a company

## The company in one step

Open **Setup → Set up company** (`company.setup`) and choose a template:

| Template | For |
|---|---|
| `us_small_business`, `us_manufacturer` | US chart of accounts, USD |
| `ca_small_business`, `ca_manufacturer` | Canadian chart, CAD, GST/HST/PST codes |
| `de_skr03`, `de_skr03_manufacturer` | DATEV SKR 03, EUR, 19 % and 7 % VAT |

Give the company a code and a name, and the first fiscal year. The company is set up with all of these:
- its chart of accounts;
- the fiscal periods, one per month (choose `yearly` for one per year);
- a warehouse, and the purchasing, sales and tax settings;
- tax codes and payment terms.

Nothing is half done: if anything is refused, nothing is created.

## What to fill in next

1. **Company details.** In *Party details* for the company, enter the legal name, the address, the VAT ID or tax number (Steuernummer), the commercial register entry and the language. They print on every document.
2. **Bank account.** The template creates one (`bank` role). Enter its IBAN or account number under *Bank account numbers*, and set it as the account customers pay into.
3. **Logo.** *Printing → Set logo* (`company_logo.set`): a PNG or JPEG up to 512 KiB.
4. **Email.** *Email settings* (`smtp_settings.set`): your SMTP server, so documents can be sent.
5. **Users and roles.** *Users*: invite colleagues and give them roles for this company.

## Customers, vendors and items

- **Items**: a code, a name, the unit stock is kept in.
- **Customers** and **vendors**: a code and a name. Add each one's addresses, VAT ID, contact email and language in *Party details*; invoices go to that email.
- **Payment terms**: net days and an optional early-payment discount ("2 % within 10 days, net 30"). Give a customer or vendor its usual terms.
- **Opening balances**: post a journal entry (`journal_entry.post`) dated the day before you start, with every balance sheet account's opening balance against an opening-balance equity account.
