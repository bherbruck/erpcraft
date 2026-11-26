# 1. Configurer une entreprise

## L'entreprise en une seule étape

Ouvrez **Configuration → Configurer l'entreprise** (`company.setup`) et choisissez un modèle :

| Modèle | Pour |
|---|---|
| `ca_small_business`, `ca_manufacturer` | plan comptable canadien, CAD, codes TPS/TVH, TVQ et TVP |
| `us_small_business`, `us_manufacturer` | plan comptable américain, USD |
| `de_skr03`, `de_skr03_manufacturer` | DATEV SKR 03, EUR, TVA à 19 % et à 7 % |

Donnez à l'entreprise un code, un nom et son premier exercice. L'entreprise est configurée avec tout ce qui suit :
- son plan comptable;
- les périodes financières, une par mois (choisissez `yearly` pour une par année);
- un entrepôt, ainsi que les paramètres des achats, des ventes et des taxes;
- les codes de taxe et les conditions de paiement.

Rien n'est fait à moitié : si quoi que ce soit est refusé, rien n'est créé.

## Ce qu'il faut remplir ensuite

1. **Renseignements sur l'entreprise.** Dans les *Coordonnées du partenaire* de l'entreprise, inscrivez :
   - la dénomination sociale et l'adresse;
   - au Canada, le numéro d'entreprise avec le compte TPS/TVH (`123456789RT0001`) et, au Québec, le numéro d'inscription à la TVQ (`1234567890TQ0001`);
   - en France, le numéro SIREN, le numéro de TVA intracommunautaire et l'immatriculation au RCS;
   - la langue (fr).

   Ils s'impriment sur chaque document.
2. **Compte bancaire.** Le modèle en crée un (rôle `bank`). Inscrivez son numéro de compte (ou son IBAN) sous *Numéros de compte bancaire* et désignez-le comme le compte où les clients paient.
3. **Logo.** *Impression → Définir le logo* (`company_logo.set`) : un fichier PNG ou JPEG d'au plus 512 Kio.
4. **Courriel.** *Paramètres de courriel* (`smtp_settings.set`) : votre serveur SMTP, pour que les documents puissent être envoyés.
5. **Utilisateurs et rôles.** *Utilisateurs* : invitez vos collègues et attribuez-leur des rôles pour cette entreprise.

## Clients, fournisseurs et articles

- **Articles** : un code, un nom et l'unité dans laquelle le stock est tenu.
- **Clients** et **fournisseurs** : un code et un nom. Ajoutez pour chacun les adresses, les numéros de taxes, le courriel de la personne-ressource et la langue dans les *Coordonnées du partenaire*; les factures sont envoyées à ce courriel.
- **Conditions de paiement** : un nombre de jours nets et, au besoin, un escompte pour paiement anticipé (« 2 % 10 jours, net 30 jours »). Attribuez à chaque client et à chaque fournisseur ses conditions habituelles.
- **Soldes d'ouverture** : comptabilisez une écriture de journal (`journal_entry.post`) datée de la veille de votre démarrage, qui porte le solde d'ouverture de chaque compte du bilan en contrepartie d'un compte de capitaux propres d'ouverture.
