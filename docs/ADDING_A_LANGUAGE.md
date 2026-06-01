# Adding support for a new language

`cccc` keeps the **scoring engine** (`cccc-core`) and the **CLI** (`cccc-cli`)
completely language-agnostic. Supporting a new language means writing a thin
**adapter** that lowers that language's AST into the shared IR
([`cccc_core::ir::Node`]) — you never touch the metrics or the CLI.

This guide walks through it end to end, using a hypothetical Python front-end
(`cccc-py` / `cccc-py-cli`) as the running example. The existing ECMaScript
front-end (`cccc-es` + `cccc-es-cli`) is the reference implementation — read it
alongside this guide.

- [The big picture](#the-big-picture)
- [Step 1 — create the adapter crate](#step-1--create-the-adapter-crate)
- [Step 2 — lower the AST to IR](#step-2--lower-the-ast-to-ir)
- [Step 3 — create the binary crate](#step-3--create-the-binary-crate)
- [Step 4 — register both crates in the workspace](#step-4--register-both-crates-in-the-workspace)
- [Step 5 — test it](#step-5--test-it)
- [The IR contract (reference)](#the-ir-contract-reference)
- [Checklist](#checklist)

## The big picture

```
your-parser ──▶ cccc-<lang>  ──(Vec<ir::Node>)──▶ cccc-core::engine ──▶ FileReport
 (AST)          (adapter lib)                      (scoring, shared)      (JSON/table)
                     ▲                                                        ▲
                     └──────────── cccc-<lang>-cli (binary) ─────────────────┘
                                   wires adapter + cccc-cli::run
```

Two new crates per language:

| Crate | Kind | Depends on | Responsibility |
|-------|------|-----------|----------------|
| `cccc-<lang>` | library | `cccc-core` + your parser | Parse source, lower AST → `Vec<ir::Node>`. **No scoring, no CLI deps.** |
| `cccc-<lang>-cli` | binary (`cccc-<lang>`) | `cccc-cli` + `cccc-<lang>` | A few lines of `main` wiring the adapter into the shared runner. |

Keeping the adapter and binary in **separate crates** is deliberate: a library
consumer who only wants the metrics depends on `cccc-<lang>` (+ `cccc-core` +
your parser) and never pulls in `clap` / `ignore` / `rayon`.

## Step 1 — create the adapter crate

```
crates/cccc-py/
├── Cargo.toml
└── src/
    └── lib.rs
```

`crates/cccc-py/Cargo.toml`:

```toml
[package]
name = "cccc-py"
version.workspace = true
edition.workspace = true
license.workspace = true
description = "Python adapter that lowers source into the cccc-core complexity IR"

[dependencies]
cccc-core = { workspace = true }
# your Python parser crate(s) here, e.g.:
rustpython-parser = "0.4"
```

The adapter must expose exactly two public items, because that is the contract
the binary (Step 3) and `cccc_cli::run` rely on:

```rust
use std::path::Path;
use cccc_core::report::FileReport;

/// File extensions analyzed by default (when `--ext` is not given).
pub const DEFAULT_EXTS: &[&str] = &["py", "pyi"];

/// Parse `source` (named by `path`) and produce its scored [`FileReport`].
pub fn analyze_source(path: &Path, source: &str) -> FileReport {
    let (nodes, parse_errors) = to_ir(path, source);
    cccc_core::engine::analyze(&path.display().to_string(), &nodes, parse_errors)
}
```

> `analyze_source`'s signature **must** be `fn(&Path, &str) -> FileReport` — that
> is `cccc_cli::AnalyzeFn`. `analyze` itself is generic over languages; you only
> supply the `Vec<ir::Node>`.

Optionally also expose `to_ir(path, source) -> (Vec<Node>, Vec<String>)` (the IR
plus parser-error strings) for embedders who want the raw IR — `cccc-es` does.

## Step 2 — lower the AST to IR

This is the only real work. You walk your parser's AST and emit
[`cccc_core::ir::Node`] values for the constructs that affect complexity;
everything else becomes a [`Node::Group`] (a transparent container the engine
just recurses into) or is dropped.

See [The IR contract](#the-ir-contract-reference) below for what each node means
and which scoring it triggers. The mapping for a typical language:

| Source construct | IR node |
|------------------|---------|
| function / method / lambda / accessor | `Node::Function { name, kind, line, body }` |
| `if` / `elif` / `else` | `Node::Branch { test, then, alternate }` (chain `elif` as a nested `Branch` in `alternate`) |
| ternary / conditional expression | `Node::Conditional { test, then, alternate }` |
| `for` / `while` / comprehension-with-condition | `Node::Loop { body }` |
| `switch` / `match` | `Node::Switch { cases }` (one `SwitchCase` per arm; set `is_default` for the catch-all) |
| `catch` / `except` | `Node::Catch { body }` |
| labelled `break` / `continue` | `Node::Jump { labeled: true }` (unlabelled → `false`, or just omit) |
| `&&` / `||` / `??` chains | `Node::Logical { op, operands }` — **fold like operators** (see below) |
| function call | `Node::Call { callee: Some(name) }` (for recursion detection) |
| anything else with children | `Node::Group(children)` |

### Two subtleties to get right

**1. Logical-operator folding (the only lowering logic the adapter owns).**
A run of *like* operators is **one** `Logical` node: `a && b && c` →
`Logical { op: And, operands: [a, b, c] }` (one cognitive point). A *different*
operator nested inside starts a fresh `Logical`: `a && (b || c)` → an outer
`And` whose second operand is an inner `Logical { op: Or, .. }` (two points).
The engine counts one cognitive point per `Logical` node and one cyclomatic
point per *extra* operand, so the folding is what makes the SonarSource
like-operator rule come out right.

**2. Use your parser's full-traversal visitor, not a hand-written recursion.**
If your parser offers a visitor with default "walk every child" methods (oxc's
`Visit`, `syn::visit`, tree-sitter cursors, …), **use it** and override only the
nodes that produce IR. Hand-rolling a recursive `match` that enumerates node
kinds *will* miss constructs in positions you didn't think of (a lambda inside a
default-argument, an operator inside an index expression) and silently
undercount. `cccc-es` learned this the hard way; it now drives lowering from
oxc's `Visit` and assembles the IR with a stack of "collector" vectors:

```rust
// Sketch of the collector pattern (see crates/cccc-es/src/lib.rs for the real thing):
struct Builder { stack: Vec<Vec<Node>>, /* line table, pending name/kind … */ }

impl Builder {
    fn emit(&mut self, node: Node) { self.stack.last_mut().unwrap().push(node); }

    /// Run `f` against a fresh child collector and return what it gathered.
    fn collect(&mut self, f: impl FnOnce(&mut Self)) -> Vec<Node> {
        self.stack.push(Vec::new());
        f(self);
        self.stack.pop().unwrap()
    }
}

// In the visitor: a loop emits a Loop whose body is whatever the sub-walk gathered.
fn visit_while(&mut self, node: &WhileStmt) {
    let body = self.collect(|b| walk_while(b, node)); // default walk reaches everything
    self.emit(Node::Loop { body });
}
```

## Step 3 — create the binary crate

```
crates/cccc-py-cli/
├── Cargo.toml
└── src/
    └── main.rs
```

`crates/cccc-py-cli/Cargo.toml` — note `[[bin]] name` is the user-facing command:

```toml
[package]
name = "cccc-py-cli"
version.workspace = true
edition.workspace = true
license.workspace = true
description = "The cccc-py binary: Python complexity CLI"

[[bin]]
name = "cccc-py"
path = "src/main.rs"

[dependencies]
cccc-cli = { workspace = true }
cccc-py = { workspace = true }

[dev-dependencies]
assert_cmd = "2"
predicates = "3"
serde_json = "1"
```

`crates/cccc-py-cli/src/main.rs` — the whole binary:

```rust
fn main() {
    std::process::exit(cccc_cli::run(
        env!("CARGO_BIN_NAME"),        // → program name in --help / --version
        env!("CARGO_PKG_VERSION"),     // → version string
        cccc_py::analyze_source,       // the AnalyzeFn from Step 1
        cccc_py::DEFAULT_EXTS,
    ));
}
```

`cccc_cli::run` provides, for free and identically across languages: argument
parsing (`--table`, `--ext`, `--max-*`, `--min`, `--top-*`, `--no-ignore`,
`-j/--jobs`), `.gitignore`-aware file discovery, parallel analysis, summary /
ranking, JSON & table rendering, and the exit-code convention
(`0` ok / `1` threshold exceeded / `2` cannot proceed).

## Step 4 — register both crates in the workspace

Add the two crates to the root `Cargo.toml`:

```toml
[workspace]
members = [
    "crates/cccc-core",
    "crates/cccc-cli",
    "crates/cccc-es",
    "crates/cccc-es-cli",
    "crates/cccc-py",       # new
    "crates/cccc-py-cli",   # new
]

[workspace.dependencies]
cccc-core = { path = "crates/cccc-core" }
cccc-cli  = { path = "crates/cccc-cli" }
cccc-es   = { path = "crates/cccc-es" }
cccc-py   = { path = "crates/cccc-py" }   # new — lets cccc-py-cli use `workspace = true`
```

## Step 5 — test it

Test the two concerns **separately** — this is the payoff of the IR seam:

1. **Engine rules** are already covered by `cccc-core`'s own tests (built from IR
   directly, no parser). You don't re-test the metrics.

2. **Your adapter's lowering** — write end-to-end tests that feed real source to
   `analyze_source` and assert the scores, mirroring
   `crates/cccc-es/src/lib.rs`'s `#[cfg(test)]` module. Anchor them on the
   SonarSource white-paper examples so cross-language results stay comparable:

   ```rust
   #[test]
   fn sonar_sum_of_primes_is_7() {
       let src = "def sum_of_primes(max):\n  ...";   // your language
       let r = analyze_source(Path::new("t.py"), src);
       assert_eq!(find(&r.functions, "sum_of_primes").unwrap().cognitive, 7);
   }
   ```

3. **CLI behaviour** is already covered generically; copy
   `crates/cccc-es-cli/tests/cli.rs` and adjust `cargo_bin("cccc-py")` plus a
   fixture if you want a smoke test of the wired binary.

Then:

```sh
cargo build
cargo clippy --all-targets   # expect zero warnings
cargo test                   # core + your adapter + cli tests
cargo run -p cccc-py-cli -- --table path/to/some/code
```

## The IR contract (reference)

Defined in [`crates/cccc-core/src/ir.rs`](../crates/cccc-core/src/ir.rs). The
top level you return is a `Vec<Node>` representing **module-level code**; the
engine scores it under an implicit "module" frame (so top-level statements count
toward the file totals but are not reported as a function).

| `Node` variant | Meaning | Cognitive | Cyclomatic |
|----------------|---------|-----------|------------|
| `Function { name, kind, line, body }` | A function-like unit. Scored independently — **nesting resets to 0** inside it — and reported as a child of the enclosing unit. `name`/`kind` are opaque labels you choose (`"function"`, `"method"`, `"arrow"`, `"getter"`, …); the engine only compares `name` for recursion. | — (body scored in its own frame) | base **1** |
| `Branch { test, then, alternate }` | `if`. `then` scored with nesting bonus; `alternate` (nested `Branch` = `else if`, else any node = `else`) scored **flat**. | `then`: +1 +nesting · `alternate`: +1 flat | +1 per branch + per `else if` |
| `Conditional { test, then, alternate }` | Ternary `?:`. | +1 +nesting | +1 |
| `Loop { body }` | Any loop. | +1 +nesting | +1 |
| `Switch { cases }` | `switch`/`match`. | +1 +nesting (the switch) | +1 per non-`default` case |
| `Catch { body }` | Exception handler. | +1 +nesting | +1 |
| `Jump { labeled }` | `break`/`continue`. | +1 if `labeled`, else 0 | — |
| `Logical { op, operands }` | One folded run of like operators. | +1 per node | +1 per *extra* operand (`operands.len() - 1`) |
| `Call { callee }` | A call. | +1 if `callee` == nearest enclosing function's `name` (recursion) | — |
| `Group(children)` | Transparent container; recurse only. | 0 | 0 |

Rules to respect when lowering:

- **Nesting** increases inside the bodies of `Branch`/`Conditional`/`Loop`/
  `Switch`/`Catch` and inside nested `Function` bodies. You express this purely
  by **structure** — put nested constructs inside the parent's `body`/`then`/
  `operands`; the engine tracks the depth. You never compute nesting yourself.
- **`else if` is a nested `Branch`** in the outer branch's `alternate`; a plain
  `else` is any other node (commonly a `Group`). This is what makes `else if`
  score flat (no nesting bonus) while a nested `if` inside `then` does get the
  bonus.
- **Recursion** is detected by name equality against the nearest enclosing
  `Function`. Populate `Call.callee` with the called function's simple name
  (e.g. both `foo()` and `obj.foo()` → `Some("foo")` in `cccc-es`). If you can't
  resolve a name, use `None`.
- **Module code is not a `Function`.** Don't wrap the file in a synthetic
  `Function`; just return its statements as the top-level `Vec<Node>`.

## Checklist

- [ ] `crates/cccc-<lang>/` adapter lib: `analyze_source` (`fn(&Path,&str)->FileReport`) + `DEFAULT_EXTS`; depends only on `cccc-core` + your parser.
- [ ] AST → IR lowering driven by a full-traversal visitor; like-logical-operators folded into single `Logical` nodes.
- [ ] `crates/cccc-<lang>-cli/` binary: `[[bin]] name = "cccc-<lang>"`, `main` calls `cccc_cli::run(env!("CARGO_BIN_NAME"), env!("CARGO_PKG_VERSION"), analyze_source, DEFAULT_EXTS)`.
- [ ] Both crates added to root `[workspace] members`; adapter added to `[workspace.dependencies]`.
- [ ] Adapter tests assert scores on real source (anchor on SonarSource examples).
- [ ] `cargo build` / `cargo clippy --all-targets` / `cargo test` all green.

[`cccc_core::ir::Node`]: ../crates/cccc-core/src/ir.rs
[`Node::Group`]: ../crates/cccc-core/src/ir.rs
