// Shared by the scripts in this directory: reads the admin credentials from the
// repository's .env, signs in, and runs commands against the server.
// The app itself never uses this file; it has its own client in src/api/client.ts.

import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";

export type Json = null | boolean | number | string | Json[] | { [key: string]: Json };
export type JsonObject = { [key: string]: Json };

export const SERVER = process.env.ERPCRAFT_URL ?? "http://127.0.0.1:8600";

function readEnvFile(): Record<string, string> {
  const path = resolve(import.meta.dir, "../../.env");
  const out: Record<string, string> = {};
  let text = "";
  try {
    text = readFileSync(path, "utf8");
  } catch {
    return out;
  }
  for (const line of text.split("\n")) {
    const match = /^\s*([A-Z0-9_]+)\s*=\s*(.*)\s*$/.exec(line);
    if (!match) continue;
    const [, key, raw] = match;
    if (key === undefined || raw === undefined) continue;
    out[key] = raw.replace(/^(['"])(.*)\1$/, "$2");
  }
  return out;
}

export class ApiFailure extends Error {
  constructor(
    readonly status: number,
    readonly code: string,
    message: string,
  ) {
    super(message);
  }
}

async function request(method: string, path: string, token: string | null, body?: Json): Promise<Json> {
  const headers: Record<string, string> = {};
  if (token) headers.authorization = `Bearer ${token}`;
  if (body !== undefined) headers["content-type"] = "application/json";
  const response = await fetch(`${SERVER}${path}`, {
    method,
    headers,
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const text = await response.text();
  const parsed: Json = text ? (JSON.parse(text) as Json) : null;
  if (!response.ok) {
    const error = parsed && typeof parsed === "object" && !Array.isArray(parsed) ? parsed.error : null;
    const detail = error && typeof error === "object" && !Array.isArray(error) ? error : {};
    throw new ApiFailure(
      response.status,
      typeof detail.code === "string" ? detail.code : "error",
      typeof detail.message === "string" ? detail.message : `${method} ${path} failed with ${response.status}`,
    );
  }
  return parsed;
}

export interface Session {
  token: string;
  /** Run a registered command. */
  run(command: string, input: JsonObject): Promise<Json>;
  get(path: string): Promise<Json>;
}

function adminCredentials(): { email: string; password: string } {
  const file = readEnvFile();
  const email = process.env.ERPCRAFT_ADMIN_EMAIL ?? file.ERPCRAFT_ADMIN_EMAIL;
  const password = process.env.ERPCRAFT_ADMIN_PASSWORD ?? file.ERPCRAFT_ADMIN_PASSWORD;
  if (!email || !password) {
    throw new Error("ERPCRAFT_ADMIN_EMAIL and ERPCRAFT_ADMIN_PASSWORD must be set in ../.env");
  }
  return { email, password };
}

async function sessionFor(email: string, password: string): Promise<Session> {
  const result = await request("POST", "/api/auth/login", null, { email, password });
  const token = result && typeof result === "object" && !Array.isArray(result) ? result.token : null;
  if (typeof token !== "string") throw new Error("the server did not return a token");
  return {
    token,
    run: (command, input) => request("POST", `/api/commands/${command}`, token, input),
    get: (path) => request("GET", path, token),
  };
}

/** Sign in as the administrator named in ../.env (or in the environment). */
export async function signIn(): Promise<Session> {
  const { email, password } = adminCredentials();
  return sessionFor(email, password);
}

/** A user the end-to-end run signs in as to see what a restricted user sees. */
export const RESTRICTED = {
  email: "e2e-restricted-test-user@example.test",
  name: "E2E restricted test user (not a person)",
  role: "E2E restricted test role",
};

/**
 * Sign in as the restricted test user, creating it and its role (granted for
 * all companies) the first time. The user cannot be deleted through the API, so it is kept
 * and reused; its password is derived from the administrator's at run time
 * and never written anywhere.
 */
export async function signInRestricted(admin: Session, permissions: Json[]): Promise<Session> {
  const password = createHash("sha256").update(`${adminCredentials().password}:${RESTRICTED.email}`).digest("hex");
  const roles = asObject(await admin.run("entity.list", { entity: "role", filters: { name: RESTRICTED.role } }));
  const existing = asArray(roles.items)[0];
  let roleId: string;
  if (existing) {
    roleId = asString(asObject(existing).id);
    await admin.run("entity.update", { entity: "role", id: roleId, data: { permissions } });
  } else {
    roleId = asString(
      asObject(await admin.run("entity.create", { entity: "role", data: { name: RESTRICTED.role, permissions } })).id,
    );
  }
  try {
    return await sessionFor(RESTRICTED.email, password);
  } catch {
    const user = asObject(await admin.run("user.create", { email: RESTRICTED.email, name: RESTRICTED.name, password }));
    await admin.run("user.assign_role", {
      user_id: asString(user.user_id ?? user.id),
      role_id: roleId,
    });
    return sessionFor(RESTRICTED.email, password);
  }
}

export function asObject(value: Json | undefined): JsonObject {
  if (value && typeof value === "object" && !Array.isArray(value)) return value;
  throw new Error(`expected an object, got ${JSON.stringify(value)}`);
}

export function asArray(value: Json | undefined): Json[] {
  if (Array.isArray(value)) return value;
  throw new Error(`expected an array, got ${JSON.stringify(value)}`);
}

export function asString(value: Json | undefined): string {
  if (typeof value === "string") return value;
  throw new Error(`expected a string, got ${JSON.stringify(value)}`);
}
