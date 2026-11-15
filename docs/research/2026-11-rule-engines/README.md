# Rule and script engines for erpcraft

Date: 2026-11-15. Status: research only. **No engine is chosen.** This records what was tested so the decision can be made later without repeating the work.

## The question

erpcraft will need two things it does not have yet:

- **Rules.** One expression each: a row permission ("only your warehouse"), a list filter or saved view, a validation, an approval threshold, a workflow condition, a computed value.
- **Scripts.** Custom logic that runs without rebuilding the server.

A rule that filters a list must become a SQL `WHERE` clause. Filtering in Rust after the query breaks paging, is slow, and leaks counts. So a rule engine must expose its syntax tree, and it must not allow loops. Scripts have neither requirement, but must be sandboxed. The two decisions are separate and may use different engines.

## What was tested

Four engines, the same rule in each:

> A user may read stock entries in their own warehouses, and only those under 10,000 unless they are a manager.

Each had to:

1. evaluate the rule against one record (a matching one and a non-matching one);
2. compile the rule to a SQL `WHERE` clause with bind parameters, given the user;
3. refuse input that a rule must never be (a loop, a multi-statement program).

The code is in [`spike/`](spike/). It is standalone, not part of the workspace. To rerun it:

```sh
cd spike
cargo run --release --features cel   --bin s_cel
cargo run --release --features rhai  --bin s_rhai
cargo run --release --features cedar --bin s_cedar
cargo run --release --features monty --bin s_monty   # needs Rust 1.98+
```

## Results

| | CEL | Rhai | Cedar | Monty |
|---|---|---|---|---|
| Crate and version | `cel` 0.16.0 | `rhai` 1.27.0 | `cedar-policy` 4.14.1 | `monty` 1.6.0 |
| What it is | Expression language | Scripting language with an expression-only mode | Authorization policy language | Sandboxed Python interpreter |
| License | MIT | MIT or Apache-2.0 | Apache-2.0 | MIT |
| Evaluates the rule | Yes | Yes | Yes | Yes |
| Compiles to SQL | Yes, about 45 lines of our code | Yes, about 45 lines of our code | Yes, about 30 lines, after its own partial evaluation | No: no syntax tree is exposed |
| Refuses loops and programs | Yes, the language has none | Yes in expression mode | Yes, the language has none | No |
| Size added to a stripped release binary | 5.7 MB | 3.3 MB | 8.3 MB | 11.3 MB |
| Crates in its dependency tree | 54 | 32 | 88 | 197 |
| Minimum Rust | 1.86 | 1.66 | 1.89 | 1.98 |
| Releases on crates.io | 19 | 106 | 82 | 20 |
| Total downloads | 6.2 million | 14.9 million | 12.1 million | 0.2 million |
| GitHub stars | 719 | 5,815 | 1,877 | 8,943 |
| Repository first created | 2023 | 2016 | 2023 | 2023 |

All four repositories had commits on the day of the test. Sizes were measured with Rust 1.100, default crate features, `strip = true`, against a 0.4 MB baseline binary.

### SQL each one produced

For a user with warehouses `W1`, `W2` who is not a manager:

| Engine | `WHERE` clause | Bind values |
|---|---|---|
| CEL | `((t.warehouse = ANY($1)) AND ((t.amount < $2) OR $3))` | `["W1","W2"]`, `10000`, `false` |
| Rhai | `((t.warehouse = ANY($1)) AND ((t.amount < $2) OR $3))` | `["W1","W2"]`, `10000`, `false` |
| Cedar | `((t.warehouse = ANY($1)) AND (t.amount < $2))` | `["W1","W2"]`, `10000` |

Cedar's is shorter because its partial evaluation already knew the user was not a manager and removed that branch.

## Findings per engine

### CEL

- Worked with no surprises. The syntax tree is public API (`Program::expression()`), and each operator arrives as a named function call (`_&&_`, `@in`), which made the SQL compiler a single `match`.
- Cannot loop by language design. Comprehensions such as `list.all(x, ...)` exist but are bounded by the list, and our compiler refused them for SQL, as intended.
- CEL also has JavaScript implementations, so a browser UI could check the same filter or validation rule. Not tested here.
- Version 0.16: expect API changes before 1.0.

### Rhai

- `Engine::compile_expression` rejected a statement block with a loop and a closure containing a loop. It accepted `record.items.all(|i| i.qty > 0)`: closures whose body is an expression are allowed.
- Its expression tree compiled to SQL as easily as CEL's. `x in list` arrives as `contains(list, x)`, with the arguments swapped.
- Reading the tree requires the `internals` feature, which Rhai documents as unstable API. That is the main risk of using Rhai for rules.
- Smallest footprint and the only 1.x-stable crate that can do both rules and scripts.
- Termination in script mode depends on engine settings (operation limits), not on the language.

### Cedar

- The best result for row permissions. Type-aware partial evaluation took the policy plus the known user and returned a residual condition about the record alone, already simplified.
- That feature (`tpe`) carries an experimental warning in the crate.
- It needs its own schema and entity model, which would duplicate the erpcraft registry unless generated from it. The spike had to declare `User`, `StockEntry` and the `read` action by hand.
- It decides permit or forbid and nothing else. It cannot express a validation, a computed value or a user's saved filter.
- Worth copying regardless of engine: explicit `forbid` rules that always override `permit`.
- Agent platforms are adopting Cedar for "what may this agent do", so exporting erpcraft roles as Cedar policies may be useful later. Not tested.

### Monty

- Ran the rule correctly as a Python expression.
- Exposes a runner, not a syntax tree, so there is nothing to compile to SQL. It also accepted a multi-statement program with a `while` loop as a "rule".
- Its README states that a crash inside the interpreter takes the host process down, and recommends the `monty-pool` crate, which runs it in worker subprocesses, for untrusted code.
- Requires Rust 1.98, released weeks before this test. It did not build on the Rust 1.95 this project was developed with. Its minimum Rust version has risen three times in two months.
- A candidate for scripts, especially ones written by agents. Not a candidate for rules.

## Reading of the results

- For **rules**, the candidates are CEL and Rhai. CEL is safer by design and portable to the browser. Rhai is smaller, stable, and would also cover scripts, at the cost of depending on an unstable API for SQL.
- For **permissions only**, Cedar's partial evaluation is the strongest, but it cannot serve the other rule types.
- For **scripts**, the candidates are Rhai, Monty, or WASM. Nothing here tested WASM.
- Whatever is chosen should sit behind its own crate (check, evaluate, compile to SQL), so the kernel never depends on an engine and a later swap stays small.

## Limits of this study

- One rule. No nested data, no dates, no decimals, no null handling.
- Default features only; sizes could be reduced by turning features off.
- No speed measurements.
- Error messages, type checking against the registry, and hardening against hostile input were not examined.
- WASM, GoRules ZEN and the smaller CEL-based policy crates were not tested.
