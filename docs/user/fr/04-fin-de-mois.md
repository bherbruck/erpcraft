# 4. Fin de mois

Une fois les documents du mois saisis, suivez ces étapes dans l'ordre.

1. **Banque.** Importez les relevés bancaires du mois (`bank_statement.import`), rapprochez chaque ligne (`bank_statement.match`) et comptabilisez le relevé (`bank_statement.post`). Les sommes non rapprochées sont comptabilisées dans un compte ou expliquées.
2. **Postes ouverts.** *Postes ouverts → Classement chronologique* (`open_item.aging`) présente ce que les clients vous doivent et ce que vous devez, par nombre de jours de retard. Les totaux doivent être égaux aux comptes clients et aux comptes fournisseurs; le rapport l'indique.
3. **Marchandises reçues non facturées.** Les réceptions sans facture restent dans ce compte : relancez les fournisseurs, ou constatez une charge à payer.
4. **Devises.** *Réévaluation de change* (`fx_revaluation.post`) évalue les postes ouverts en devises au cours de clôture du mois.
5. **Amortissement.** *Immobilisations → Calcul d'amortissement* (`depreciation_run.post`), daté du dernier jour du mois.
6. **Stock.** Comptez ce qui doit l'être (`stock_count.start`, `review`, `post`); les écarts sont inscrits comme ajustements.
7. **Taxes.** Le *Rapport de taxes* (`tax.report`) par code, ou par lieu pour les déclarations canadiennes (TPS/TVH, TVQ) et américaines, sert de base à vos déclarations; la *Déclaration OSS* (`tax.oss_report`) vise les ventes à distance dans l'UE (TVA).
8. **Rapports.**
   - la *Balance de vérification* (`gl.trial_balance`) doit être équilibrée;
   - l'*État des résultats*, le *Bilan* et le *Tableau des flux de trésorerie* (`gl.cash_flow`) présentent les résultats du mois;
   - *Budget par rapport au réel* (`budget.report`) montre les écarts.
9. **Fermer le mois.** *Périodes financières → Fermer* (`fiscal_period.close`). Plus rien ne peut y être comptabilisé; rouvrez-la (`fiscal_period.reopen`) si vous devez corriger quelque chose.

## À la fin de l'exercice

Comptabilisez une **clôture d'exercice** (`year_end_close.post`) datée du dernier jour de l'exercice. Elle reporte le résultat de l'exercice aux bénéfices non répartis et ferme tous les mois de l'exercice. Contrepassez-la (`year_end_close.reverse`) pour corriger un exercice clos, puis clôturez de nouveau.
