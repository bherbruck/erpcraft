# Verfahrensdokumentation nach GoBD

*Grundsätze zur ordnungsmäßigen Führung und Aufbewahrung von Büchern, Aufzeichnungen und Unterlagen in elektronischer Form sowie zum Datenzugriff (BMF-Schreiben vom 28. November 2019, zuletzt geändert am 11. März 2024).*

This document is in German because it is written for a German tax auditor (Betriebsprüfer) and a German tax adviser. It describes what erpcraft itself guarantees. A company that keeps its books in erpcraft completes it with how it works: who enters what, where the vouchers are kept, how backups are run. The parts it must add are marked **(vom Unternehmen zu ergänzen)**.

Die GoBD verlangen keine bestimmte Software, sondern dass die Bücher nachvollziehbar, vollständig, richtig, zeitgerecht, geordnet und unveränderbar geführt und aufbewahrt werden, und dass das Verfahren beschrieben ist (Rz. 151 ff.). Diese Beschreibung gliedert sich wie dort empfohlen: allgemeine Beschreibung, Anwenderdokumentation, technische Systemdokumentation und Betriebsdokumentation.

## 1. Allgemeine Beschreibung

erpcraft ist ein quelloffenes ERP-System (Apache-2.0) mit Finanzbuchhaltung, Einkauf, Verkauf, Lager und Zahlungsverkehr. Alle Daten liegen in einer PostgreSQL-Datenbank; das Programm selbst speichert nichts. Jede Veränderung geschieht durch einen **Befehl** (Command), den ein angemeldeter Benutzer über die Oberfläche, die HTTP-Schnittstelle oder MCP aufruft. Ein Befehl läuft in genau einer Datenbanktransaktion: Er wird ganz ausgeführt oder gar nicht.

Buchungen entstehen aus **Belegen** (Dokumenten): Rechnungen, Gutschriften, Lieferscheine, Wareneingänge, Zahlungen, Buchungssätze. Ein Beleg wird als Entwurf erfasst, geprüft und dann gebucht. Erst die Buchung (Posting) schreibt in die Nebenbücher: das Hauptbuch (`gl_entry`), das Lager (`stock_entry`), die offenen Posten (`open_item`, `settlement`) und die Steuer (`tax_entry`).

**(vom Unternehmen zu ergänzen):** Unternehmen, eingesetzte Module, Zahl der Mandanten, Kontenrahmen (z. B. SKR 03), wer die Buchhaltung führt und wer sie an den Steuerberater übergibt.

## 2. Anwenderdokumentation

Die Bedienung ist im Benutzerhandbuch beschrieben (`docs/user/de/`): Einrichtung eines Mandanten, Einkauf, Verkauf, Monatsabschluss. Für jeden Befehl beschreibt das System selbst, was er tut, welche Eingaben er nimmt und welche Rechte er verlangt (`meta.describe`, in der Oberfläche als Hilfe).

**Belegfunktion.** Jede Buchung gehört zu einem Beleg und trägt dessen Art, Nummer, Datum und Buchungstext. Die Belegnummer wird beim Buchen aus einem Nummernkreis vergeben, lückenlos und ohne Doppel (ein Entwurf hat noch keine Nummer). Eingehende elektronische Rechnungen (XRechnung, ZUGFeRD) und EDI-Nachrichten werden im Original gespeichert; zu jedem Beleg lassen sich Druckstücke (HTML, PDF/A-3) erzeugen.

**Korrekturen.** Ein gebuchter Beleg wird nicht geändert und nicht gelöscht. Er wird storniert: Die Stornobuchung kehrt jede Zeile in jedem Nebenbuch um, an ihrem eigenen Datum, das nicht vor der stornierten Buchung liegen darf. Original und Storno bleiben beide sichtbar und verweisen aufeinander. Gelöscht werden können nur Entwürfe, die noch nichts gebucht haben.

**(vom Unternehmen zu ergänzen):** Wer erfasst, wer prüft und wer bucht (Rollen und Rechte, siehe 3.4); wie Papierbelege abgelegt oder ersetzend gescannt werden; wie eingehende E-Mails mit Rechnungen aufbewahrt werden.

## 3. Technische Systemdokumentation

### 3.1 Unveränderbarkeit (Rz. 58 ff.)

Die Unveränderbarkeit wird nicht durch das Programm allein, sondern in der Datenbank erzwungen, also auch gegen direkten Datenbankzugriff mit den Rechten der Anwendung:

- Buchungen (`posting`), Hauptbuchzeilen (`gl_entry`), Lagerbewegungen (`stock_entry`), Ausgleiche offener Posten (`settlement`), Steuerzeilen (`tax_entry`), das Protokoll (`audit_log`) und das Exportprotokoll (`export_log`) haben Trigger, die jede Änderung und jedes Löschen ablehnen (`erp_immutable`), und Trigger gegen `TRUNCATE`.
- Ein gebuchter Beleg ist gesperrt (`document.locked`): Kopf und Inhalt sind unveränderlich, nur sein Status geht über die definierten Übergänge weiter (z. B. gebucht → storniert).
- Eine Buchung in eine abgeschlossene Periode wird abgelehnt (`fiscal_period`, Trigger der Hauptbuchzeilen). Eine Periode wird erst nach dem Abschluss geschlossen; das Wiedereröffnen ist ein eigener, protokollierter Befehl mit eigenem Recht.
- Jede Buchung ist ausgeglichen (Soll = Haben je Buchung, in der Hauptwährung), geprüft beim Abschluss der Transaktion.

### 3.2 Protokollierung (Nachvollziehbarkeit, Rz. 59, 107 f.)

Jeder Befehl wird protokolliert (`audit_log`): wer, wann, welcher Befehl mit welcher Eingabe, und welche Datensätze er berührt hat; auch abgelehnte und fehlgeschlagene Versuche mit ihrem Grund. Damit hat jeder Datensatz, auch jeder Stammdatensatz (Konto, Kunde, Lieferant, Steuerschlüssel), eine Historie (`audit.history`). Das Protokoll wird nie gelöscht. Nur bei Befehlen, die Geheimnisse verarbeiten (Passwörter, Schlüssel), wird die Eingabe nicht gespeichert.

Jede Buchung trägt das Buchungsdatum (Belegdatum), den Zeitpunkt ihrer Erfassung (`created_at`, UTC) und den Benutzer. So ist auch erkennbar, ob zeitgerecht gebucht wurde (Rz. 46 ff.).

### 3.3 Vollständigkeit und Richtigkeit (Rz. 36 ff.)

- Ein Beleg bucht nur, wenn alle Nebenbücher zustimmen; eine Buchung, die nicht aufgeht, wird ganz abgelehnt.
- Hauptbuch und Nebenbücher werden gegeneinander abgestimmt: `erpcraft verify` prüft, dass die Salden der Sammelkonten den offenen Posten, das Lagerkonto den Lagerwerten und die Steuerkonten den Steuerzeilen entsprechen. Die Prüfung läuft auch nach jeder Wiederherstellung einer Sicherung.
- Nummernkreise sind lückenlos; ein Beleg ohne Nummer ist ein Entwurf und hat nichts gebucht.

### 3.4 Zugriffsrechte (internes Kontrollsystem, Rz. 100 ff.)

Jeder Benutzer meldet sich an; jeder Befehl verlangt ein Recht, das ihm über eine Rolle je Mandant zugewiesen ist. Rechte können auf Datensätze und Felder eingeschränkt werden. Richtlinien (Policies) legen fest, was einer Freigabe bedarf (z. B. Gutschriften ohne Rechnung, Zahlungen über einem Betrag); niemand gibt seine eigene Anfrage frei. Benutzer werden deaktiviert, nicht gelöscht, damit ihre Einträge im Protokoll zuordenbar bleiben.

**(vom Unternehmen zu ergänzen):** die Rollen und wer sie hat; Freigaberegeln; das Vier-Augen-Prinzip bei Zahlungen.

### 3.5 Datenzugriff der Finanzverwaltung (Rz. 158 ff.)

- **Unmittelbarer Zugriff (Z1):** Ein Benutzer mit Leserechten (ohne Schreibrechte) kann angelegt werden; alle Berichte (Summen- und Saldenliste, Kontoblatt, GuV, Bilanz, offene Posten, Umsatzsteuer) sind lesend verfügbar.
- **Mittelbarer Zugriff (Z2):** Auswertungen nach Vorgabe des Prüfers über dieselben Berichte und Filter.
- **Datenträgerüberlassung (Z3):** `gl.export` gibt alle Hauptbuchzeilen eines Zeitraums als CSV aus (UTF-8, mit Beleg, Konto, Betrag, Währung, Gegenpartei und Dimensionen); `datev.export_postings` und `datev.export_parties` erzeugen den DATEV-Buchungsstapel und die Debitoren/Kreditoren. Jeder Export wird mit Zeitpunkt, Benutzer, Zeitraum, Zeilenzahl und SHA-256 der Datei im Exportprotokoll festgehalten. Eine Beschreibungsdatei nach dem Beschreibungsstandard für die Datenträgerüberlassung (`index.xml`) wird nicht erzeugt **(vom Unternehmen bzw. Steuerberater bei Bedarf zu ergänzen)**.

## 4. Betriebsdokumentation

### 4.1 Aufbewahrung (§ 147 AO, Rz. 113 ff.)

erpcraft löscht keine Buchhaltungsdaten: Belege, Buchungen, Nebenbücher, Protokoll und Exportprotokoll bleiben unbegrenzt in der Datenbank. Gelöscht werden nur technische Hilfsdaten: zugestellte Ereignisse der Ereigniswarteschlange nach 30 Tagen und Idempotenzschlüssel nach 24 Stunden. Sie sind keine Buchhaltungsdaten; was sie auslöste, steht im Protokoll.

Die Fristen (zehn Jahre für Bücher und Jahresabschlüsse; acht Jahre für Buchungsbelege nach dem Vierten Bürokratieentlastungsgesetz, für Belege, deren Frist am 1. Januar 2025 noch nicht abgelaufen war; sechs Jahre für Handels- und Geschäftsbriefe) werden damit vom System nicht verkürzt. Eine Löschung nach Ablauf der Frist ist nicht vorgesehen und müsste außerhalb von erpcraft geschehen.

### 4.2 Datensicherung

Die Datenbank wird mit `deploy/backup.sh` gesichert (siehe `docs/ops/backup.md`): eine in sich konsistente Sicherung, die vor dem Behalten auf Lesbarkeit geprüft wird. Eine Wiederherstellung ersetzt die Datenbank erst, nachdem die wiederhergestellten Bücher mit `erpcraft verify` abgestimmt sind; die ersetzte Datenbank bleibt erhalten.

**(vom Unternehmen zu ergänzen):** Sicherungsrhythmus, Aufbewahrung der Sicherungen außer Haus, wer die Wiederherstellung testet und wie oft.

### 4.3 Programmänderungen (Rz. 104 f.)

Jede Version von erpcraft ist im Quellcode versioniert. Datenbankänderungen erfolgen ausschließlich durch nummerierte Migrationen, die nie nachträglich geändert werden; die Datenbank weiß, welche Migrationen sie hat, und ein älteres Programm verweigert den Start gegen eine neuere Datenbank (`docs/ops/UPGRADING.md`). Geschäftsregeln, die ein Unternehmen selbst festlegt (Richtlinien, Rechte, Steuerregeln), sind Daten und werden wie jede Änderung protokolliert.

**(vom Unternehmen zu ergänzen):** wer Updates einspielt, wie sie vorher getestet werden, und die eingesetzte Version je Zeitraum.

## 5. Grenzen

- erpcraft ersetzt nicht die organisatorischen Teile der Verfahrensdokumentation (Belegfluss, ersetzendes Scannen, Aufbewahrung außerhalb des Systems, internes Kontrollsystem); sie sind oben markiert.
- Wer Administratorzugriff auf den Datenbankserver selbst hat (Superuser), kann Trigger abschalten. Die Unveränderbarkeit gilt gegenüber der Anwendung und ihren Benutzern; den Datenbankserver und seine Superuser zu beschränken und zu protokollieren ist Teil des Betriebs **(vom Unternehmen zu ergänzen)**.
- Diese Beschreibung ist keine Zertifizierung. Ob ein Verfahren den GoBD entspricht, beurteilt die Finanzverwaltung im Einzelfall.
