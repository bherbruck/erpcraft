# 3. Ventes

## Soumission et commande

1. Une **soumission** (`sales_quote`) est facultative. Émettez-la (`sales_quote.issue`) et envoyez-la. Lorsque le client l'accepte, **Accepter** (`sales_quote.accept`) la transforme en commande client; **Perdre** conserve la raison de la perte.
2. Une **commande client** : le client et des lignes avec l'article, l'entrepôt et la quantité. Les prix laissés vides proviennent des listes de prix. **Approuver** (`sales_order.approve`) : la commande reçoit son numéro et le stock est réservé.

## Livraison

- **Livrer** (`sales_order.deliver`) crée le bon de livraison en brouillon avec ce qui reste à expédier. Indiquez les numéros de lot ou de série des articles suivis.
- **Comptabiliser** le bon de livraison (`sales_delivery.post`) : le stock sort à son coût, et le coût des marchandises vendues est inscrit.

## Facture

- **Facture** (`sales_order.invoice`) crée la facture en brouillon pour ce qui a été livré et pas encore facturé. Le code de taxe provient des règles de taxe (le client, l'article, la destination des marchandises) et peut être modifié.
- **Comptabiliser** la facture (`sales_invoice.post`). Les produits et les taxes sont inscrits, et le client doit un poste ouvert.
- **Envoyer par courriel** (`document.send`) l'envoie en PDF, ou en facture électronique si le client en accepte.

La facture d'un client francophone s'imprime en français, avec les nombres (`1 234,56`) et les dates (`15 mars 2026`) à la française.
- Pour une entreprise au Canada, elle porte ce que le Québec exige : le numéro de facture et sa date, la date de livraison, vos numéros d'inscription à la TPS/TVH et à la TVQ, et les taxes par taux (TPS 5 %, TVQ 9,975 % calculée sur le prix sans la TPS).
- Pour une entreprise en France, elle porte ce que la loi française exige : le numéro SIREN, le numéro de TVA intracommunautaire des deux parties, la TVA par taux, la date d'échéance, les pénalités de retard et l'indemnité forfaitaire pour frais de recouvrement de 40 € (art. L. 441-10 du Code de commerce).

## Encaissement

- **Paiements → Nouveau**, en entrée, avec les postes ouverts du client (`payment.post`), ou rapprochez la ligne d'un relevé bancaire importé (*Banque → Importer un relevé*, puis *Rapprocher la ligne*).
- L'escompte pour paiement anticipé est pris automatiquement si le paiement est daté à l'intérieur des jours d'escompte.

## Quand les clients paient en retard

- Les **relevés de compte client** (`customer_statement.issue_all`) en fin de mois montrent à chaque client ce qui est ouvert.
- Les **traitements de relance** (`dunning_run.propose`, révision, `post`, `send`) envoient des relances par niveau, avec les frais et les intérêts que prévoient vos niveaux de relance.
