# 1. Einen Mandanten einrichten

## Der Mandant in einem Schritt

Öffnen Sie **Einrichtung → Mandant einrichten** (`company.setup`) und wählen Sie eine Vorlage:

| Vorlage | Für |
|---|---|
| `de_skr03`, `de_skr03_manufacturer` | DATEV SKR 03, EUR, 19 % und 7 % Umsatzsteuer |
| `us_small_business`, `us_manufacturer` | US-Kontenrahmen, USD |
| `ca_small_business`, `ca_manufacturer` | kanadischer Kontenrahmen, CAD, GST/HST/PST |

Geben Sie Code, Namen und das erste Geschäftsjahr an. Der Mandant wird mit all dem eingerichtet:
- sein Kontenrahmen;
- die Buchungsperioden, eine je Monat (`yearly` für eine je Jahr);
- ein Lager sowie die Einstellungen für Einkauf, Verkauf und Steuer;
- Steuerschlüssel und Zahlungsbedingungen.

Nichts bleibt halb fertig: wird etwas abgelehnt, entsteht nichts.

## Was danach einzutragen ist

1. **Firmendaten.** In den *Partnerdaten* des Mandanten tragen Sie ein:
   - den Firmennamen und die Anschrift;
   - die Steuernummer oder USt-IdNr.;
   - den Handelsregistereintrag und die Sprache (de).

   Sie erscheinen auf jedem Beleg, wie § 14 UStG es verlangt.
2. **Bankkonto.** Die Vorlage legt eines an (Rolle `bank`). Tragen Sie unter *Bankverbindungen* die IBAN und BIC ein und wählen Sie es als Konto, auf das Kunden zahlen.
3. **Logo.** *Druck → Logo setzen* (`company_logo.set`): ein PNG oder JPEG bis 512 KiB.
4. **E-Mail.** *E-Mail-Einstellungen* (`smtp_settings.set`): Ihr SMTP-Server, damit Belege versendet werden können.
5. **Benutzer und Rollen.** Unter *Benutzer* laden Sie Kollegen ein und geben ihnen Rollen für diesen Mandanten.

## Debitoren, Kreditoren und Artikel

- **Artikel**: Artikelnummer, Bezeichnung und die Einheit, in der der Bestand geführt wird.
- **Debitoren** und **Kreditoren**: Nummer und Name. Anschriften, USt-IdNr., E-Mail des Ansprechpartners und Sprache tragen Sie in den *Partnerdaten* ein; an diese E-Mail gehen die Rechnungen.
- **Zahlungsbedingungen**: Nettotage und optional Skonto („2 % Skonto innerhalb 10 Tagen, 30 Tage netto“). Geben Sie jedem Debitor und Kreditor seine üblichen Bedingungen.
- **Eröffnungsbilanz**: Buchen Sie einen Buchungssatz (`journal_entry.post`) zum Vortag Ihres Starts, mit den Anfangsbeständen aller Bestandskonten gegen das Saldenvortragskonto.
