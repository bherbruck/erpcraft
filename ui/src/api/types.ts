// The shapes the UI works with.
//
// Almost all of them are the server's own: they are generated from the output
// schemas of the commands (see generated.ts) and only given shorter names here.
// What is still written by hand is at the bottom, each with the reason.

import type * as G from "./generated";
import type { Json } from "./generated";

export type { Json };
export type JsonObject = { [key: string]: Json };

// ------------------------------------------------------------- the registry

export type Meta = G.MetaDescribeOutput;
export type FieldMeta = G.FieldMeta;
export type FieldType = G.FieldType;
export type DecimalMeta = G.DecimalMeta;
export type FactMeta = G.FactMeta;
export type ValueSource = G.ValueSource;
export type EntityMeta = G.EntityMeta;
export type DocTypeMeta = G.DocTypeMeta;
export type CollectionMeta = G.CollectionMeta;
export type StateMeta = G.StateMeta;
export type CommandMeta = G.CommandMeta;
export type EntityAccess = G.EntityAccess;
export type Reach = G.Reach;
export type PolicyMeta = G.PolicyPoint;
export type LookupType = Extract<FieldType, { kind: "lookup" }>;

// ---------------------------------------------------------------- documents

export type Posting = G.PostingView;
export type DocumentLink = G.LinkedDocument;
export type DocumentSummary = G.Document;
export type HistoryEntry = G.AuditEntry;
export type TrialBalance = G.GlTrialBalanceOutput;
export type StockOnHand = G.StockOnHandOutput;
export type BalanceSheet = G.GlBalanceSheetOutput;
export type IncomeStatement = G.GlIncomeStatementOutput;
export type StatementSection = G.Section;
export type AccountLedger = G.GlAccountLedgerOutput;
export type OpenItem = G.ListedItem;
export type SearchGroup = G.SearchGroup;

/**
 * A document as `document.get` returns it. The schema leaves `body` and
 * `summary` open because every document type defines its own; both are always
 * objects, which is all this narrows.
 */
export type DocumentRecord = Omit<G.DocumentGetOutput, "body" | "summary"> & { body: JsonObject; summary: JsonObject };

export interface Page<T> {
  items: T[];
  total: number;
}

// ------------------------------------------------- still written by hand
//
// 1. Records of entities. `entity.*` return "any value" in their schemas,
//    because the fields depend on the entity; the UI reads them through the
//    registry's field list.
// 2. The answers of the two sign-in routes, which are not commands.

/** A record of an entity. `_version` is sent back when the record is changed. */
export type EntityRecord = JsonObject & { id: string; _version?: string };

export interface Me {
  user_id: string;
  name: string;
  email: string;
  is_admin: boolean;
}

export interface LoginResult {
  token: string;
  expires_at: string;
}

export interface SsoProviders {
  providers: { slug: string; name: string }[];
}

type Narrowed = {
  "entity.list": Page<EntityRecord>;
  "entity.get": EntityRecord;
  "entity.create": EntityRecord;
  "entity.update": EntityRecord;
  "document.get": DocumentRecord;
  "document.create": DocumentRecord;
  "document.update": DocumentRecord;
};

/** The output of every command: the server's schema, narrowed where it is open. */
export type CommandOutputs = Omit<G.CommandOutputs, keyof Narrowed> & Narrowed;
