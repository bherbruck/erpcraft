# 4. Monatsabschluss

Gehen Sie diese Schritte der Reihe nach durch, wenn die Belege des Monats erfasst sind.

1. **Bank.** Importieren Sie die Kontoauszüge des Monats (`bank_statement.import`), ordnen Sie jede Zeile zu (`bank_statement.match`) und buchen Sie den Auszug (`bank_statement.post`). Nicht zugeordnetes Geld wird auf ein Konto gebucht oder geklärt.
2. **Offene Posten.** *Offene Posten → Altersstruktur* (`open_item.aging`) zeigt, was Kunden Ihnen und was Sie Lieferanten schulden, nach Tagen überfällig. Die Summen müssen den Forderungs- und Verbindlichkeitenkonten entsprechen; der Bericht sagt es.
3. **Wareneingang ohne Rechnung.** Wareneingänge ohne Rechnung bleiben auf diesem Konto: beim Lieferanten nachfassen oder abgrenzen.
4. **Fremdwährung.** *Fremdwährungsbewertung* (`fx_revaluation.post`) bewertet offene Posten in Fremdwährung zum Stichtagskurs.
5. **Abschreibungen.** *Anlagenbuchhaltung → Abschreibungslauf* (`depreciation_run.post`) zum letzten Tag des Monats.
6. **Bestand.** Zählen Sie, was zu zählen ist (`stock_count.start`, `review`, `post`); Differenzen werden als Bestandskorrektur gebucht.
7. **Steuer.** Die *Steuerauswertung* (`tax.report`) je Steuerschlüssel ist die Grundlage der Umsatzsteuer-Voranmeldung. Die *OSS-Meldung* (`tax.oss_report`) gilt für Fernverkäufe in die EU.
8. **Auswertungen.**
   - Die *Summen- und Saldenliste* (`gl.trial_balance`) muss ausgeglichen sein.
   - *Gewinn- und Verlustrechnung*, *Bilanz* und *Kapitalflussrechnung* (`gl.cash_flow`) zeigen das Ergebnis des Monats.
   - Der *Soll-Ist-Vergleich* (`budget.report`) zeigt die Abweichungen zum Budget.
9. **Periode abschließen.** *Buchungsperioden → Abschließen* (`fiscal_period.close`). Danach kann nichts mehr hineingebucht werden. Zum Korrigieren öffnen Sie sie wieder (`fiscal_period.reopen`).

## Zum Jahresende

Buchen Sie einen **Jahresabschluss** (`year_end_close.post`) zum letzten Tag des Jahres. Er überträgt das Jahresergebnis auf den Gewinnvortrag und schließt alle Monate des Jahres. Zum Korrigieren eines abgeschlossenen Jahres stornieren Sie ihn (`year_end_close.reverse`) und schließen danach erneut ab.
