# erpcraft – Benutzerhandbuch

Das Handbuch geht einen Monat eines kleinen Handelsunternehmens durch:

1. [Einen Mandanten einrichten](01-einrichtung.md)
2. [Einkauf](02-einkauf.md)
3. [Verkauf](03-verkauf.md)
4. [Monatsabschluss](04-monatsabschluss.md)

Alles, was Sie in der Oberfläche tun, ist ein **Befehl**. Derselbe Befehl lässt sich über die API (`POST /api/commands/{name}`) oder von einem KI-Agenten über MCP ausführen, mit denselben Rechten. Das Handbuch nennt jeden Befehl in Klammern, etwa (`sales_invoice.post`), damit Sie ihn an beiden Stellen finden.

**So funktionieren Belege.**
- Aufträge, Lieferscheine, Rechnungen und Zahlungen sind *Belege*. Ein Beleg beginnt als *Entwurf*, den Sie beliebig oft ändern können.
- Das Buchen (bei Aufträgen das Freigeben) sperrt ihn, vergibt die Nummer und schreibt ihn in die Bücher.
- Ein gebuchter Beleg wird nie geändert: er wird *storniert*, und ein korrigierter wird gebucht.
- Auf einem Entwurf steht die Schaltfläche für den nächsten Schritt (Freigeben, Buchen, Ausstellen) an erster Stelle.

**Sprache.** Die Oberfläche folgt der Sprache Ihres Browsers. Bei Deutsch sind Bezeichnungen, Meldungen und die für deutsche Kunden gedruckten Belege deutsch. Die Sprache eines Debitors oder Kreditors stellen Sie in seinen *Partnerdaten* ein (Profil → Sprache).
