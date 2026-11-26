# 2. Achats

## Bon de commande

1. **Achats → Bons de commande → Nouveau.** Choisissez le fournisseur, puis ajoutez des lignes avec un article, l'entrepôt où il va, la quantité et le prix. Si vous laissez le prix vide, il est rempli à partir de la liste de prix du fournisseur ou, à défaut, du dernier prix payé.
2. **Approuver** (`purchase_order.approve`). La commande reçoit son numéro et est envoyée au fournisseur avec *Envoyer par courriel* (`document.send`), ou imprimée.

## Réception des marchandises

- Sur la commande ouverte, **Recevoir** (`purchase_order.receive`) crée une réception en brouillon avec tout ce qui reste à recevoir. Ajustez les quantités à ce qui est arrivé et indiquez les numéros de lot ou de série des articles suivis.
- **Comptabiliser** la réception (`purchase_receipt.post`). Le stock augmente au prix de la commande, et la valeur reste dans les *marchandises reçues non facturées* jusqu'à l'arrivée de la facture.

## La facture du fournisseur

- **Facture** (`purchase_order.invoice`) crée la facture fournisseur en brouillon à partir de ce qui a été reçu. Inscrivez le *Numéro de la facture du fournisseur*, vérifiez les prix et les codes de taxe, et ajoutez le transport sur une ligne de frais.
- **Comptabiliser** la facture (`vendor_invoice.post`). Le montant dû devient un poste ouvert dans les comptes fournisseurs, payable selon les conditions de paiement.
- Une facture électronique (XRechnung, ZUGFeRD) peut être lue directement avec *Importer une facture électronique* (`einvoice.import`).
- Une facture sans commande (loyer, service) est une facture fournisseur avec des lignes de frais ou de compte.

## Paiement

- **Lot de paiement** (`payment_run.propose`, puis `approve` et `post`). Il propose tout ce qui est échu à une date donnée et prend les escomptes encore ouverts, puis produit un fichier SEPA ou NACHA pour votre banque.
- Ou payez une seule facture : **Paiements → Nouveau** avec les postes ouverts du fournisseur (`payment.post`).

Les marchandises retournées repartent avec **Retour** sur la commande, et la note de crédit du fournisseur s'inscrit avec **Crédit** sur la facture.
