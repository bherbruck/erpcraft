# 3. Verkauf

## Angebot und Auftrag

1. Ein **Angebot** (`sales_quote`) ist freiwillig. Stellen Sie es aus (`sales_quote.issue`) und senden Sie es. Nimmt der Kunde an, macht **Annehmen** (`sales_quote.accept`) einen Kundenauftrag daraus. **Verloren geben** hält den Grund fest.
2. Ein **Kundenauftrag**: Debitor und Zeilen mit Artikel, Lager und Menge. Leere Preise kommen aus den Preislisten. **Freigeben** (`sales_order.approve`): der Auftrag erhält seine Nummer, der Bestand wird reserviert.

## Lieferung

- **Liefern** (`sales_order.deliver`) legt den Lieferschein mit allem noch zu Liefernden an. Nennen Sie bei chargengeführten Artikeln Chargen oder Seriennummern.
- **Buchen** (`sales_delivery.post`): der Bestand geht zu seinen Kosten ab, der Wareneinsatz wird gebucht.

## Rechnung

- **Rechnung** (`sales_order.invoice`) legt die Ausgangsrechnung für Geliefertes und noch nicht Berechnetes an. Der Steuerschlüssel kommt aus den Steuerregeln (Debitor, Artikel, Lieferort) und lässt sich ändern.
- **Buchen** (`sales_invoice.post`). Erlös und Umsatzsteuer werden gebucht, der Kunde schuldet einen offenen Posten.
- **Per E-Mail senden** (`document.send`) verschickt sie als PDF, oder als E-Rechnung, wenn der Kunde eine annimmt.

Die Rechnung an einen deutschen Kunden wird deutsch gedruckt, mit den Angaben nach § 14 UStG:
- Rechnungsnummer und Rechnungsdatum;
- Leistungsdatum;
- Ihre Steuernummer oder USt-IdNr.;
- die Umsatzsteuer nach Steuersätzen;
- gegebenenfalls der Hinweis auf eine Steuerbefreiung oder die Steuerschuldnerschaft des Leistungsempfängers;
- das vereinbarte Skonto.

## Zahlungseingang

- **Zahlungen → Neu**, Eingang, mit den offenen Posten des Kunden (`payment.post`). Oder Sie ordnen die Zeile eines importierten Kontoauszugs zu (*Bank → Kontoauszug importieren*, dann *Zeile zuordnen*).
- Skonto wird automatisch abgezogen, wenn die Zahlung innerhalb der Skontofrist liegt.

## Wenn Kunden spät zahlen

- **Kontoauszüge** (`customer_statement.issue_all`) zum Monatsende zeigen jedem Kunden, was offen ist.
- **Mahnläufe** (`dunning_run.propose`, prüfen, `post`, `send`) versenden Zahlungserinnerungen und Mahnungen nach Mahnstufen, mit Mahngebühren und Verzugszinsen, wie Ihre Mahnstufen es festlegen.
