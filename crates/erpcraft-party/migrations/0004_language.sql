-- The language a party is written to in: its printed documents and the
-- letters it is sent. ISO 639-1, such as de or en. None on a customer or
-- vendor: the company's; none on the company: English.
ALTER TABLE party_profile ADD COLUMN language text CHECK (language ~ '^[a-z]{2}$');
