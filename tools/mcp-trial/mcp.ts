// A minimal MCP client for erpcraft's POST /mcp (revision 2026-07-28,
// stateless: every request carries its revision in params._meta). It
// counts what an agent would pay for: every request, and the tokens of
// what came back, split into tool descriptions (what the agent must read
// to know what it can do) and results.

export const VERSION = "2026-07-28";

/// Tokens, estimated as characters / 4: the usual rule of thumb for
/// English and JSON with a BPE tokenizer. The same estimate is used before
/// and after, so the comparison holds even where the absolute is off.
export function tokens(value: unknown): number {
  const text = typeof value === "string" ? value : JSON.stringify(value);
  return Math.ceil(text.length / 4);
}

export interface Step {
  method: string;
  name?: string;
  arguments?: unknown;
  isError: boolean;
  descriptionTokens: number;
  resultTokens: number;
  note?: string;
}

export class Client {
  steps: Step[] = [];
  stuck: string[] = [];
  private id = 0;
  constructor(readonly base: string, readonly token: string) {}

  static async login(base: string, email: string, password: string): Promise<Client> {
    const r = await fetch(`${base}/api/auth/login`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ email, password }),
    });
    if (!r.ok) throw new Error(`login failed: ${r.status} ${await r.text()}`);
    const body = (await r.json()) as { token: string };
    return new Client(base, body.token);
  }

  async rpc(method: string, params: Record<string, unknown> = {}): Promise<any> {
    const named = (params.name ?? params.uri) as string | undefined;
    const headers: Record<string, string> = {
      authorization: `Bearer ${this.token}`,
      "content-type": "application/json",
      accept: "application/json",
      "mcp-protocol-version": VERSION,
      "mcp-method": method,
    };
    if (named) headers["mcp-name"] = encodeURIComponent(named);
    const body = {
      jsonrpc: "2.0",
      id: ++this.id,
      method,
      params: {
        ...params,
        _meta: {
          "io.modelcontextprotocol/protocolVersion": VERSION,
          "io.modelcontextprotocol/clientCapabilities": {},
          "io.modelcontextprotocol/clientInfo": { name: "erpcraft-mcp-trial", version: "1" },
        },
      },
    };
    const r = await fetch(`${this.base}/mcp`, { method: "POST", headers, body: JSON.stringify(body) });
    const text = await r.text();
    let answer: any;
    try {
      answer = JSON.parse(text);
    } catch {
      throw new Error(`${method}: ${r.status} ${text.slice(0, 300)}`);
    }
    if (answer.error) {
      this.steps.push({ method, name: named, isError: true, descriptionTokens: 0, resultTokens: tokens(answer.error) });
      return { protocolError: answer.error };
    }
    return answer.result;
  }

  /// Every page of tools/list.
  async listTools(): Promise<any[]> {
    const all: any[] = [];
    let cursor: string | undefined;
    do {
      const result = await this.rpc("tools/list", cursor ? { cursor } : {});
      all.push(...result.tools);
      this.steps.push({ method: "tools/list", isError: false, descriptionTokens: tokens(result.tools), resultTokens: 0 });
      cursor = result.nextCursor;
    } while (cursor);
    return all;
  }

  /// A tool call. Tool definitions that come back (from erpcraft_tools)
  /// count as descriptions, everything else as results.
  async call(name: string, args: Record<string, unknown> = {}): Promise<{ ok: boolean; value: any }> {
    const result = await this.rpc("tools/call", { name, arguments: args });
    if (result?.protocolError) return { ok: false, value: result.protocolError };
    const value = result.structuredContent ?? JSON.parse(result.content?.[0]?.text ?? "null");
    const isDescription = Array.isArray(value?.tools);
    this.steps.push({
      method: "tools/call",
      name,
      arguments: args,
      isError: !!result.isError,
      descriptionTokens: isDescription ? tokens(value.tools) : 0,
      resultTokens: isDescription ? tokens(value) - tokens(value.tools) : tokens(value),
    });
    return { ok: !result.isError, value };
  }

  async read(uri: string): Promise<string> {
    const result = await this.rpc("resources/read", { uri });
    const text = result?.contents?.[0]?.text ?? "";
    this.steps.push({ method: "resources/read", name: uri, isError: !!result?.protocolError, descriptionTokens: tokens(text), resultTokens: 0 });
    return text;
  }

  async prompt(name: string, args: Record<string, string>): Promise<string> {
    const result = await this.rpc("prompts/get", { name, arguments: args });
    const text = (result?.messages ?? []).map((m: any) => m.content?.text ?? "").join("\n");
    this.steps.push({ method: "prompts/get", name, isError: !!result?.protocolError, descriptionTokens: tokens(text), resultTokens: 0 });
    return text;
  }

  /// Where an agent would have been stuck: it needed something the server
  /// did not tell it.
  stuckOn(what: string) {
    this.stuck.push(what);
  }
}
