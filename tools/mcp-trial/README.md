# MCP trial

Does erpcraft hold up for an AI agent that knows nothing of it beforehand and only uses `POST /mcp`? This trial checks with a scripted agent instead of a model. It does five everyday tasks the way a careful agent would, using only what the server tells it, and measures each one:

- how many requests the task took;
- how many tokens of tool descriptions the agent had to load, and how many of results;
- where it got stuck, meaning the server did not tell it what it needed next.

The numbers are in [RESULTS.md](RESULTS.md).

## Run

```sh
# Once: the month scenario as a template database (about a minute)
tools/scenario/month.sh erpcraft_mcptrial
psql postgres://erpcraft:erpcraft@127.0.0.1:54320/postgres -c \
  "CREATE DATABASE erpcraft_mcptrial_base TEMPLATE erpcraft_mcptrial"

# Each run, from the repository root, after cargo build -p erpcraft-server
tools/mcp-trial/run.sh after             # or: run.sh LABEL path/to/erpcraft
```

`run.sh` does the following:

1. Copies the template to a scratch database.
2. Starts the server on 127.0.0.1:8611.
3. Runs `setup.ts`, which makes an approved order for more widgets than are on hand (SO-2026-00004) and reopens March.
4. Runs `trial.ts`, which writes `results/LABEL.json` and prints a table.
5. Drops the scratch database.

It needs `bun` and has no dependencies.

## How it counts

- **Tokens** are characters / 4. The estimate is rough, but the before and after runs use the same one.
- **Description tokens** are what `tools/list`, `erpcraft_tools`, prompts and resources returned: what the agent reads to know what it can do. **Result tokens** are everything else the tools returned.
- **Stuck** means the agent needed a tool whose definition it did not have, a lookup the server did not offer, or a refusal that did not say how to go on. It then goes on the way a real agent would have to, usually with a larger read, so the cost shows in the tokens as well.

The agent starts from `erpcraft_modules` and loads tools with `erpcraft_tools`. It never reads the whole tool list.

The client is `mcp.ts`: revision 2026-07-28, stateless, with `_meta` on every request.
