# 2. Einkauf

## Bestellung

1. **Einkauf → Bestellungen → Neu.** Wählen Sie den Kreditor und erfassen Sie Zeilen mit Artikel, Lager, Menge und Preis. Lassen Sie den Preis leer, schlägt ihn die Preisliste des Kreditors vor, sonst der zuletzt bezahlte Preis.
2. **Freigeben** (`purchase_order.approve`). Die Bestellung erhält ihre Nummer und geht per *Per E-Mail senden* (`document.send`) an den Kreditor, oder Sie drucken sie.

## Wareneingang

- Auf der offenen Bestellung legt **Wareneingang buchen** (`purchase_order.receive`) einen Wareneingang mit allem Ausstehenden an. Passen Sie die Mengen an das Angekommene an und geben Sie bei chargengeführten Artikeln Chargen oder Seriennummern an.
- **Buchen** (`purchase_receipt.post`). Der Bestand steigt zum Bestellpreis, und der Wert steht bis zur Rechnung auf *Wareneingang ohne Rechnung*.

## Eingangsrechnung

- **Rechnung** (`purchase_order.invoice`) legt die Eingangsrechnung aus dem Eingegangenen an. Tragen Sie die Rechnungsnummer des Lieferanten ein, prüfen Sie Preise und Steuerschlüssel und erfassen Sie Fracht als Nebenkostenzeile.
- **Buchen** (`vendor_invoice.post`). Der geschuldete Betrag wird ein offener Posten auf den Verbindlichkeiten, fällig nach den Zahlungsbedingungen.
- Eine E-Rechnung (XRechnung, ZUGFeRD) lesen Sie direkt mit *E-Rechnung importieren* (`einvoice.import`) ein.
- Eine Rechnung ohne Bestellung (Miete, eine Dienstleistung) ist eine Eingangsrechnung mit Nebenkosten- oder Kontozeilen.

## Bezahlen

- **Zahlungslauf** (`payment_run.propose`, dann `approve` und `post`). Er schlägt alles bis zu einem Datum Fällige vor und zieht noch offenes Skonto. Danach schreibt er eine SEPA-Datei für Ihre Bank.
- Oder einzeln: **Zahlungen → Neu** mit den offenen Posten des Kreditors (`payment.post`).

Rücksendungen gehen mit **Rücksendung** an der Bestellung zurück, die Gutschrift des Lieferanten mit **Gutschrift** an der Rechnung.
