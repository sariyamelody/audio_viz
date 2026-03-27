---
name: review-visualizer
description: Pre-commit quality check for a visualizer file. Verifies structural correctness, convention compliance, config consistency, and that the file builds for both native and WASM targets.
argument-hint: <path/to/visualizer.rs>
---

Review the visualizer at `$ARGUMENTS`. Work through each item in order. Report pass/fail for each, fix any failures, then give a final summary.

## Checklist

### 1. Index comment is present and accurate
- The file has a `// ── Index:` line immediately before the first `use` statement.
- Spot-check three entries: look up their line numbers in the file and verify they match.
- If the comment is missing or wrong, invoke `/update-viz-index $ARGUMENTS` to fix it.

### 2. No local copies of shared utilities
Search the file for these patterns — none should exist as local definitions:

| Pattern | Shared location |
|---------|----------------|
| `fn rms(` | `visualizer_utils::rms` |
| `fn palette_lookup(` | `visualizer_utils::palette_lookup` |
| `fn smooth` or `smooth!` macro | `visualizer_utils::smooth_asymmetric` |
| `fn freq_to_bin(` | `visualizer_utils::freq_to_bin` |
| `fn mag_to_frac(` | `visualizer_utils::mag_to_frac` |
| Local `const PALETTE_` arrays | `visualizer_utils::PALETTE_*` constants |

Any found → replace with the shared version and add the import.

### 3. Config fields are consistent across all four sites
For every field in the struct's `// config` section, verify it appears in all four places:

- [ ] Struct field declaration
- [ ] `new()` initializer default
- [ ] `get_default_config()` JSON entry (with correct type, min/max or variants)
- [ ] `set_config()` match arm

A field missing from any site is either a silent bug or a compile error.

### 4. `render()` is structurally correct
- Signature is `fn render(&self, size: TermSize, fps: f32) -> Vec<String>` — `&self` not `&mut self`.
- Returns `Vec<String>` with exactly `size.rows` entries (enforced by `pad_frame`).
- Visual content loop is over `0..vis` where `vis = rows.saturating_sub(1)` (optionally `.max(1)`).
- Last two statements are:
  ```rust
  lines.push(status_bar(cols, fps, self.name(), &self.source, ""));
  pad_frame(lines, rows, cols)
  ```
- No `return` before `pad_frame` that would skip it.

### 5. `register()` is correct
```rust
pub fn register() -> Vec<Box<dyn Visualizer>> {
    vec![Box::new(<StructName>Viz::new(""))]
}
```
- Uses the correct struct name.
- Passes `""` as source (runtime replaces it).
- Returns a `Vec` not a single boxed value.

### 6. Builds for both targets
Run these in order; fix errors before moving to the next:

```bash
cargo check
```
```bash
cd web && wasm-pack build --target web --out-dir pkg --release 2>&1 | tail -20
```

WASM failures are often caused by:
- Using `std::time`, threads, or `rand` without WASM-compatible feature flags
- Importing a `terminal`-feature-gated symbol outside a `#[cfg(feature = "terminal")]` guard
- Any `eprintln!` / `println!` (use `web_sys::console::log_1` or remove)

If a symbol is terminal-only, it must be gated. Check `Cargo.toml` for the `terminal` feature definition if unsure what's gated.

---

## Output format

Report each item as `✓ passed` or `✗ failed — <what was wrong and what was fixed>`. End with either:

> All checks passed. Ready to commit.

or a summary of remaining issues if any require user input.
