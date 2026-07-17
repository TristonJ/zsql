# Contributing to zsql

zsql is a lightweight Postgres-first SQL editor (Rust + gpui). This document
covers the local workflow and the quality gates every change must pass.

## Toolchain

- Latest stable Rust (edition 2024). Verify with `rustc --version` / `cargo --version`.
- `rustfmt` and `clippy` components: `rustup component add rustfmt clippy`.

## Quality gates (must be green before review)

These are the same gates the automated implementation loop enforces:

```sh
cargo fmt --all                          # formatting (config in rustfmt.toml)
cargo clippy --all-targets -- -D warnings  # clippy pedantic, warnings are errors
cargo test --all                         # all tests
```

- **Clippy pedantic is on** via `[workspace.lints]` in the root `Cargo.toml`.
  If a pedantic lint is genuinely wrong for a case, `#[allow(...)]` it narrowly
  with a one-line justification - do not disable it workspace-wide.
- Public functions returning `Result` carry a `# Errors` doc section (pedantic).

## Style & conventions

- Respect crate boundaries: `zsql-core` is UI- and driver-agnostic (no gpui, no
  sqlx); `zsql-postgres` is the sqlx driver impl; `zsql` is the gpui binary.
- No magic constants at call sites - limits, batch sizes, and theme values come
  from `Config` (`crates/zsql/src/config.rs`).
- Instrument notable operations (connect / query / cancel / introspect) with
  `tracing` spans.
- Keep code comments self-contained: never reference external plans, milestones,
  or phases. State the actual context instead - e.g. `TODO: deferred until the
  connection pool exists`, not `deferred until M1`. A plain `TODO:` is sometimes fine.
- Prefer self-documenting code (expressive names, clear structure, useful `tracing`)
  over comments. Comment only non-obvious intent or edge cases, not what the code
  plainly does.
- Doc comments state the item's job directly - no introductory filler ("This
  function ...", "A helper that ..."). Never write changelog-style comments
  ("changed from", "previously", "now uses", "fixed X"); code is the current state.
- Source is ASCII-only (standard 7-bit): use `-` not an em-dash, straight quotes,
  `->` not an arrow. No non-ASCII characters in code, comments, or identifiers.
- Prefer `thiserror` for typed errors in libraries, `anyhow` at the app edges.

## Local development database

No `just`/`make` - a plain script plus SQL snippets:

```sh
./scripts/pg-dev.sh up      # ephemeral Postgres in Docker + seed data
./scripts/pg-dev.sh down    # stop it
```

The script prints a `DATABASE_URL` you can export. Seed schema lives in
`dev/seed.sql` (edit it or apply snippets by hand).

## Tests

- `zsql-core`: pure unit tests.
- `zsql-postgres`: integration tests against a local Postgres (start it with the
  script above); these cover the type mapping and introspection.
- Keep view logic thin; push testable logic into core / session types.

## Development loop

Feature work runs through the deterministic review loop documented in
`../plans/agentic-implementation-loop.md` (implement -> gate -> fresh-context
review). Nothing is committed on your behalf.
