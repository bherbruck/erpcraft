// The one API client. Nothing else in the UI calls `fetch`.
//
// Apart from signing in and out, everything goes through `run`, which executes
// a registered command: the same commands an agent reaches over MCP.

import { language } from "@/i18n/language";
import { t } from "@/i18n/t";
import type { CommandInputs, CommandName } from "./generated";
import type { CommandOutputs, Json, JsonObject, LoginResult, Me, Meta, SsoProviders } from "./types";

/** Where in a record or document the server says an error is. */
export interface ErrorPlace {
  /** The field the error is about. With `line`, a field of that line. */
  field?: string | null;
  /** The collection of a document the error is in, such as `lines`. */
  collection?: string | null;
  /** The line of that collection, counted from 1. */
  line?: number | null;
  /** On a 409, why: `stale`, `state`, `duplicate` or `in_use`. */
  reason?: string | null;
}

export class ApiError extends Error {
  readonly field: string | null;
  readonly collection: string | null;
  readonly line: number | null;
  readonly reason: string | null;

  constructor(
    readonly status: number,
    readonly code: string,
    message: string,
    place: ErrorPlace = {},
  ) {
    super(message);
    this.name = "ApiError";
    this.field = place.field ?? null;
    this.collection = place.collection ?? null;
    this.line = place.line ?? null;
    this.reason = place.reason ?? null;
  }

  /** Someone else changed the record after it was read. */
  get isStale(): boolean {
    return this.status === 409 && this.reason === "stale";
  }
}

const STORAGE_KEY = "erpcraft.token";

let token: string | null = null;
let onUnauthorized: () => void = () => {};

function storage(): Storage | null {
  try {
    return window.sessionStorage;
  } catch {
    return null;
  }
}

export const session = {
  /** The token from this tab's session, if there is one. */
  restore(): string | null {
    token = storage()?.getItem(STORAGE_KEY) ?? null;
    return token;
  },
  set(value: string): void {
    token = value;
    storage()?.setItem(STORAGE_KEY, value);
  },
  clear(): void {
    token = null;
    storage()?.removeItem(STORAGE_KEY);
  },
  has(): boolean {
    return token !== null;
  },
  /** Called whenever the server answers 401, after the token is dropped. */
  onUnauthorized(handler: () => void): void {
    onUnauthorized = handler;
  },
};

interface RequestOptions {
  body?: Json;
  headers?: Record<string, string>;
  signal?: AbortSignal;
  /** A 401 from signing in is a wrong password, not an expired session. */
  anonymous?: boolean;
}

async function request<T>(method: string, path: string, options: RequestOptions = {}): Promise<T> {
  const headers: Record<string, string> = { ...options.headers };
  if (token && !options.anonymous) headers.authorization = `Bearer ${token}`;
  if (options.body !== undefined) headers["content-type"] = "application/json";
  // The server names things (fields, states, actions, reports) in the language the UI is in.
  headers["accept-language"] = language();

  let response: Response;
  try {
    response = await fetch(path, {
      method,
      headers,
      body: options.body === undefined ? undefined : JSON.stringify(options.body),
      signal: options.signal,
      // The single sign-on exchange relies on a cookie set by this same origin.
      credentials: "same-origin",
    });
  } catch (cause) {
    if (cause instanceof DOMException && cause.name === "AbortError") throw cause;
    throw new ApiError(0, "network", t("error.network"));
  }

  const text = await response.text();
  let parsed: unknown = null;
  if (text) {
    try {
      parsed = JSON.parse(text);
    } catch {
      parsed = null;
    }
  }

  if (!response.ok) {
    const error = readError(parsed);
    if (response.status === 401 && !options.anonymous) {
      session.clear();
      onUnauthorized();
    }
    throw new ApiError(
      response.status,
      error?.code ?? "error",
      error?.message ?? t("error.status", { status: response.status }),
      error ?? {},
    );
  }
  return parsed as T;
}

function readError(parsed: unknown): ({ code?: string; message?: string } & ErrorPlace) | null {
  if (typeof parsed !== "object" || parsed === null || !("error" in parsed)) return null;
  const error = parsed.error;
  if (typeof error !== "object" || error === null) return null;
  const text = (name: string): string | undefined => {
    const value = (error as Record<string, unknown>)[name];
    return typeof value === "string" ? value : undefined;
  };
  const line = (error as Record<string, unknown>).line;
  return {
    code: text("code"),
    message: text("message"),
    field: text("field"),
    collection: text("collection"),
    reason: text("reason"),
    line: typeof line === "number" ? line : null,
  };
}

export interface RunOptions {
  /** Makes a repeat of the same request return the first result. */
  idempotencyKey?: string;
  signal?: AbortSignal;
}

export type CommandOutput<N extends string> = N extends keyof CommandOutputs ? CommandOutputs[N] : Json;

function commandPath(name: string, preview: boolean): string {
  return `/api/commands/${encodeURIComponent(name)}${preview ? "?preview=true" : ""}`;
}

function commandOptions(input: Json, options: RunOptions): RequestOptions {
  return {
    body: input,
    signal: options.signal,
    headers: options.idempotencyKey ? { "idempotency-key": options.idempotencyKey } : undefined,
  };
}

export const api = {
  async login(email: string, password: string): Promise<LoginResult> {
    const result = await request<LoginResult>("POST", "/api/auth/login", {
      body: { email, password },
      anonymous: true,
    });
    session.set(result.token);
    return result;
  },

  /** The single sign-on providers the sign-in page offers. Needs no session. */
  ssoProviders(signal?: AbortSignal): Promise<SsoProviders> {
    return request<SsoProviders>("GET", "/api/auth/sso", { anonymous: true, signal });
  },

  /** Turns the one-time code a provider's sign-in ended with into a session. */
  async ssoExchange(code: string): Promise<LoginResult> {
    const result = await request<LoginResult>("POST", "/api/auth/sso/exchange", { body: { code }, anonymous: true });
    session.set(result.token);
    return result;
  },

  async logout(): Promise<void> {
    try {
      await request<Json>("POST", "/api/auth/logout");
    } catch {
      // Signing out locally must work even if the server cannot be reached.
    }
    session.clear();
  },

  me(signal?: AbortSignal): Promise<Me> {
    return request<Me>("GET", "/api/auth/me", { signal });
  },

  /** The registry, its labels and help in the UI's language (`meta.describe` answers in English). */
  meta(signal?: AbortSignal): Promise<Meta> {
    return request<Meta>("GET", `/api/meta?${new URLSearchParams({ locale: language() })}`, { signal });
  },

  /** Run a command known when the UI was built. Input and output are typed. */
  run<N extends CommandName>(name: N, input: CommandInputs[N], options: RunOptions = {}): Promise<CommandOutput<N>> {
    return request<CommandOutput<N>>("POST", commandPath(name, false), commandOptions(input, options));
  },

  /**
   * Run a command the UI only knows from the registry at run time, such as the
   * workflow actions of a document type a module added.
   */
  runNamed(name: string, input: JsonObject, options: RunOptions = {}): Promise<Json> {
    return request<Json>("POST", commandPath(name, false), commandOptions(input, options));
  },

  /** What a command would do, without doing it. Nothing is kept. */
  async preview(name: string, input: JsonObject, options: RunOptions = {}): Promise<Json> {
    const answer = await request<{ preview: boolean; result: Json }>(
      "POST",
      commandPath(name, true),
      commandOptions(input, { signal: options.signal }),
    );
    return answer.result;
  },
};
