---
name: add-config-field
description: Add a new config field to an existing visualizer. Handles all four required edit sites — struct field, new() default, get_default_config() JSON, and set_config() match arm — so none are missed.
argument-hint: <path/to/visualizer.rs> <field-name> <type> <default-value>
---

Add a config field to the visualizer at `$ARGUMENTS[0]`.

- Field name: `$ARGUMENTS[1]`
- Type: `$ARGUMENTS[2]`  (float | int | bool | enum)
- Default: `$ARGUMENTS[3]`

If any argument is missing, infer it from context or ask before proceeding.

## The four required edit sites

A config field touches exactly four places. Make all four edits, then verify none were missed.

### 1 — Struct field

Add a Rust field to the visualizer struct. Use the correct Rust type:

| Config type | Rust type |
|-------------|-----------|
| `float`     | `f32`     |
| `int`       | `usize` or `i32` depending on semantics |
| `bool`      | `bool`    |
| `enum`      | `String`  |

Find the struct using the `// ── Index:` line at the top of the file. Add the new field in the `// config` section of the struct (after any existing config fields, before the closing `}`).

### 2 — `new()` default

In the `Self { … }` initializer inside `new()`, add:
```rust
<field_name>: <default_value>,
```
Match the type: string defaults need `.to_string()`, numeric defaults are bare literals.

### 3 — `get_default_config()` JSON entry

Add a new object to the `"config"` array inside the `serde_json::json!({…})` macro. Use the correct schema shape for the type:

**float:**
```json
{
    "name": "<field_name>",
    "display_name": "<Human Label>",
    "type": "float",
    "value": <default>,
    "min": <min>,
    "max": <max>
}
```

**int:**
```json
{
    "name": "<field_name>",
    "display_name": "<Human Label>",
    "type": "int",
    "value": <default>,
    "min": <min>,
    "max": <max>
}
```

**bool:**
```json
{
    "name": "<field_name>",
    "display_name": "<Human Label>",
    "type": "bool",
    "value": <true|false>
}
```

**enum:**
```json
{
    "name": "<field_name>",
    "display_name": "<Human Label>",
    "type": "enum",
    "value": "<default_variant>",
    "variants": ["<v1>", "<v2>", …]
}
```

### 4 — `set_config()` match arm

Inside the `for entry in config { match entry["name"].as_str().unwrap_or("") { … } }` block in `set_config()`, add a new arm:

**float / int:**
```rust
"<field_name>" => {
    self.<field_name> = entry["value"].as_f64().unwrap_or(<default>) as f32;
}
```
*(use `as_i64()` and cast to the int type if the field is `int`)*

**bool:**
```rust
"<field_name>" => {
    self.<field_name> = entry["value"].as_bool().unwrap_or(<default>);
}
```

**enum:**
```rust
"<field_name>" => {
    if let Some(s) = entry["value"].as_str() {
        self.<field_name> = s.to_string();
    }
}
```

---

## After all four edits

1. **Run `cargo check`** — fix any type or borrow errors before finishing.

2. **Check if lines shifted** — if the total line count changed (it will), invoke `/update-viz-index $ARGUMENTS[0]` to keep the index comment accurate.

3. **Remind the user** to wire the new field into `tick()` or `render()` if it's not already used — an unused struct field will generate a compiler warning.
