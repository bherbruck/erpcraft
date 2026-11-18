// What a run of the e2e or screenshot script makes and normally undoes at its end. A run that
// dies partway (a timeout, Ctrl+C, a crash) leaves it behind, and the next run then trips over
// it: a provider slug that already exists, a fiscal period that overlaps, a policy rule that
// holds an order for approval. Both scripts sweep it away first, so each run starts clean.
//
// Only what the scripts name as theirs is touched: memos and names starting with "E2E",
// "e2e-" slugs, the screenshots' example provider and their "(demo ...)" journals.

import { asArray, asObject, asString, type Json, type JsonObject, type Session } from "./lib";

const isE2E = (value: Json | undefined): boolean => typeof value === "string" && /^e2e[ -]/i.test(value);

async function list(session: Session, command: string, input: JsonObject): Promise<JsonObject[]> {
  try {
    return asArray(asObject(await session.run(command, { ...input, limit: 500 })).items).map(asObject);
  } catch {
    return [];
  }
}

/** Runs a step and reports a failure without stopping the sweep: a leftover may already be half gone. */
async function attempt(what: string, step: () => Promise<unknown>): Promise<boolean> {
  try {
    await step();
    return true;
  } catch (cause) {
    console.log(`leftover not removed: ${what}: ${cause instanceof Error ? cause.message : String(cause)}`);
    return false;
  }
}

export async function sweepLeftovers(session: Session): Promise<void> {
  let removed = 0;
  const count = async (what: string, step: () => Promise<unknown>): Promise<void> => {
    if (await attempt(what, step)) removed += 1;
  };

  for (const provider of await list(session, "entity.list", { entity: "sso_provider" })) {
    const slug = provider.slug;
    if (slug === "microsoft-example" || (typeof slug === "string" && slug.startsWith("e2e-"))) {
      await count(`sso_provider ${String(slug)}`, () =>
        session.run("sso_provider.delete", { id: asString(provider.id) }),
      );
    }
  }

  for (const rule of await list(session, "entity.list", { entity: "policy_rule" })) {
    const fromPolicyChecks =
      rule.point === "purchase_receipt.over_receipt" &&
      rule.effect === "allow" &&
      rule.condition === "facts.over_percent <= 5";
    if (isE2E(rule.message) || fromPolicyChecks) {
      await count(`policy_rule ${asString(rule.id)}`, () =>
        session.run("entity.delete", { entity: "policy_rule", id: asString(rule.id) }),
      );
    }
  }

  for (const period of await list(session, "entity.list", { entity: "fiscal_period" })) {
    if (!isE2E(period.name)) continue;
    const id = asString(period.id);
    await session.run("fiscal_period.reopen", { id }).catch(() => null);
    await count(`fiscal_period ${String(period.name)}`, () =>
      session.run("entity.delete", { entity: "fiscal_period", id }),
    );
  }

  for (const asset of await list(session, "entity.list", { entity: "asset" })) {
    if (isE2E(asset.code) && asset.status === "planned") {
      await count(`asset ${String(asset.code)}`, () =>
        session.run("entity.delete", { entity: "asset", id: asString(asset.id) }),
      );
    }
  }

  // Drafts the scripts made; posted documents are reversed by the runs that post them and stay.
  const demoJournal = /\(demo [A-Z0-9]+\)$|^Rent, August$/;
  for (const docType of ["journal_entry", "purchase_order", "vendor_invoice", "payment"]) {
    for (const doc of await list(session, "document.list", { doc_type: docType, status: "draft" })) {
      const memo = doc.memo;
      if (isE2E(memo) || (docType === "journal_entry" && typeof memo === "string" && demoJournal.test(memo))) {
        await count(`${docType} draft ${String(memo)}`, () =>
          session.run("document.delete", { doc_type: docType, id: asString(doc.id) }),
        );
      }
    }
  }
  // An approved order of a run that died is cancelled, so that no open order of it is waiting.
  for (const doc of await list(session, "document.list", { doc_type: "purchase_order", status: "open" })) {
    if (isE2E(doc.memo)) {
      await count(`purchase_order ${String(doc.memo)}`, () =>
        session.run("purchase_order.cancel", { id: asString(doc.id) }),
      );
    }
  }

  // Contacts the party checks add to the seed's customer.
  const customer = (await list(session, "entity.list", { entity: "customer", filters: { code: "C001" } }))[0];
  if (customer) {
    const id = asString(customer.id);
    const party = asObject(await session.run("party.get", { party_type: "customer", party_id: id }));
    const contacts = asArray(party.contacts).map(asObject);
    const kept = contacts.filter((contact) => !isE2E(contact.name));
    if (kept.length < contacts.length) {
      await count("E2E contacts of C001", () =>
        session.run("party.set", { party_type: "customer", party_id: id, contacts: kept }),
      );
    }
  }

  // Vendors the record checks create ("E2E vendor", a quick-created "e2e supplier").
  for (const vendor of await list(session, "entity.list", { entity: "vendor" })) {
    if (isE2E(vendor.name)) {
      await count(`vendor ${String(vendor.name)}`, () =>
        session.run("entity.delete", { entity: "vendor", id: asString(vendor.id) }),
      );
    }
  }

  if (removed > 0) console.log(`removed ${removed} leftovers of an earlier run`);
}

/**
 * Runs `undo` when the process is stopped (Ctrl+C, a timeout's SIGTERM) before its own
 * `finally` could, then exits with the signal's code.
 */
export function undoOnSignal(undo: () => Promise<void>): void {
  let stopping = false;
  for (const signal of ["SIGINT", "SIGTERM"] as const) {
    process.on(signal, () => {
      if (stopping) return;
      stopping = true;
      console.log(`\n${signal}: undoing what this run made`);
      void undo().finally(() => process.exit(signal === "SIGINT" ? 130 : 143));
    });
  }
}
