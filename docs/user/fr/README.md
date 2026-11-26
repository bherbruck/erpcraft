# erpcraft : guide de l'utilisateur

Le guide parcourt un mois d'activité d'une petite entreprise commerciale :

1. [Configurer une entreprise](01-configuration.md)
2. [Achats](02-achats.md)
3. [Ventes](03-ventes.md)
4. [Fin de mois](04-fin-de-mois.md)

Tout ce que vous faites dans l'interface d'administration est une **commande**. La même commande peut être exécutée par l'API (`POST /api/commands/{name}`) ou par un agent d'IA au moyen de MCP, avec les mêmes droits. Le guide nomme chaque commande entre parenthèses, par exemple (`sales_invoice.post`), pour que vous la retrouviez à l'un ou l'autre endroit.

**Fonctionnement des documents.**
- Les commandes, les livraisons, les factures et les paiements sont des *documents*. Un document commence comme *Brouillon*, que vous pouvez modifier autant de fois que vous le voulez.
- Le comptabiliser (ou approuver une commande) le verrouille, lui attribue son numéro et l'inscrit dans les livres.
- Un document comptabilisé n'est jamais modifié : on le *contrepasse*, puis on comptabilise un document corrigé.
- Sur un brouillon, le premier bouton est l'étape suivante (Approuver, Comptabiliser, Émettre).

**Langue.** L'interface suit la langue de votre navigateur. Lorsque le français est sélectionné, les libellés, les messages et les documents que vous imprimez pour un client francophone sont en français. La langue d'un client ou d'un fournisseur se définit dans ses *Coordonnées du partenaire* (Profil → Langue).
