# General Coding Conventions
These are some general guidelines & coding conventions to follow when contributing to zsql. They
aren't strict all rules or enforced by tooling, but are intended to keep the codebase consistent
and maintainable.

## Formatting & Linting
- **Clippy pedantic is on** via `[workspace.lints]` in the root `Cargo.toml`. If a pedantic lint
  is genuinely wrong for a case, `#[allow(...)]` it narrowly with a one-line justification - do
  not disable it workspace-wide.
- **rustfmt** is used for formatting - run `cargo fmt --all` to format the codebase.

## Crate Boundaries
There are a number of crates in the zsql workspace. Currently, the main ones are:
- `zsql`: The main binary crate - which holds the gpui application, and composes the other crates.
- `zsql-core`: The core library, which is UI- and driver-agnostic. It contains the main logic
  and data structures used by the other crates.
- `zsql-ui`: A UI/UX crate for shared components and utilities. For example buttons, modals, etc.
- `zsql-editor`: The editor crate, which provides the code editor component used in the UI.
- `zsql-[driver]`: Crates for specific database drivers, e.g. `zsql-postgres` for Postgres.

This is not intended to be an exhaustive list, and adding more crates is encouraged if the code
being added can live on it's own and is not tightly coupled to the other crates.

## Code Style
- No magic constants at call sites - limits, batch sizes, and theme values come from `Config`
- Instrument notable operations (connect / query / cancel / introspect) with `tracing` spans.
- Keep code comments self-contained: never reference external plans, milestones, or phases. State
  the actual context instead - e.g. `TODO: deferred until the connection pool exists`, not
  `deferred until M1`. A plain `TODO:` is perfectly fine.
- Prefer self-documenting code (expressive names, clear structure, useful `tracing`) over comments.
  Comment only non-obvious intent or edge cases, not what the code plainly does.
- Doc comments state the item's job directly - no introductory filler ("This function ...", "A
  helper that ..."). Never write changelog-style comments ("changed from", "previously"
  "now uses", "fixed X").
- Doc comments describe the item's contract for a caller: what it does, what its arguments
  mean, its errors, and any invariant the caller must uphold. Never document WHO calls it -
  no "called by X", "the sole caller is", "for `Foo` to hand to `Bar`". Callers churn, and
  grep answers who they are; if an item only makes sense for one caller, tighten its
  visibility instead of documenting the coupling. The same goes for cross-references: link
  another item only for a genuinely shared contract, never to narrate the call graph.
- Keep doc comments short - one to three lines covers almost everything. A doc comment that
  needs a paragraph to justify an invariant is a design smell: move the invariant into the
  type system (a newtype, visibility, ownership) and delete the paragraph.
- A doc comment is optional, and no comment beats a redundant one. If the comment would only
  restate the name and signature (`/// Closes the modal.` on `fn close_modal`), omit it -
  write one only when it adds something the name cannot carry: a contract, an error, an
  invariant, a non-obvious behavior.
- Source is ASCII-only (standard 7-bit): use `-` not an em-dash, straight quotes, `->` not an
  arrow. No non-ASCII characters in code, comments, or identifiers.
- Prefer `thiserror` for typed errors in libraries, `anyhow` at the app edges.
