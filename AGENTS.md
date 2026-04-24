# Agent Instructions

## Version Control

Use **jujutsu (`jj`)** for all VCS operations. Do not use `git` commands directly.

Common equivalents:
- `git status` → `jj status`
- `git log` → `jj log`
- `git diff` → `jj diff`
- `git add` + `git commit` → `jj describe -m "message"` (changes are tracked automatically)
- `git checkout -b branch` → `jj branch create branch-name`

## Project Structure

`gph` is a zero-dependency Rust CLI that compiles a Lisp-like DSL to directed graph output formats.

**Pipeline:** Lexer (`src/lexer.rs`) → Parser (`src/parser.rs`) → AST (`src/ast.rs`) → Codegen

**Output modes:**
- Default: Mermaid flowchart text via `src/codegen.rs` (`gph file.gph`)
- SVG file: `src/layout.rs` (Sugiyama layout) + `src/svg.rs` (`gph --render -o out.svg file.gph`)
- Kitty terminal inline: `src/kitty.rs` with pixel rasterizer + bitmap font (`gph --render file.gph`)

**Key constraints:**
- Zero external crate dependencies — never add entries to `[dependencies]` in `Cargo.toml`
- No `unsafe` code
- All new output modes must parse through the same lexer/parser pipeline

## Layout Algorithm

`src/layout.rs` implements the Sugiyama framework:
1. Cycle removal (DFS feedback arc set) — reversed edges have `reversed=true`
2. Longest-path layer assignment
3. Dummy node insertion for multi-layer-spanning edges (dummy IDs start with `__d`)
4. Barycenter crossing minimization (4 passes)
5. Coordinate assignment — LR coords are canonical; direction transform applied at output time

Dummy nodes are internal and must never appear in rendered output. `insert_dummies` returns a `Vec<Vec<usize>>` (one chain per original edge) to avoid the cross-edge chain-following bug.

## Testing

Integration tests live in `tests/integration.rs`. Use `check(input, expected)` for Mermaid output and `check_svg(input, fragment)` for SVG assertions. Run with `cargo test`.

## Definition of Done

Before considering any Rust change complete:
1. `cargo clippy` — must produce zero warnings
2. `cargo fmt` — must be run to normalize formatting
3. `cargo test` — all tests must pass
