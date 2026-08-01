//! B1 — the tool-serialization seam (TOOL-CALLING-RELIABILITY.md §6 Option B).
//!
//! There are exactly three places where a `ToolDefinition` becomes provider
//! JSON: `build_anthropic_body` (which the Vertex provider reuses),
//! `openai.rs::complete`, and `gemini.rs::complete`. Each calls [`adapt_tools`]
//! with its dialect instead of serializing `input_schema` verbatim.
//!
//! The transforms are the ones MEASURED in §7.0 Exp 3, not assumed. Gemini
//! rejects `$schema`, `$ref`, `definitions`, and `additionalProperties` — and
//! the rejection kills the entire request, every tool in the turn, not just
//! the offending one. OpenAI strict mode REQUIRES `additionalProperties:
//! false`. Those two constraints are irreconcilable, which is why this is a
//! per-dialect enum and cannot be a single normalization pass.
//!
//! `OpenAiStrict` has no wire site yet: the OpenAI provider stays on
//! `OpenAiLoose` until B2's `ProviderQuirks` selects dialects per provider.
//! It exists (and is tested) here because the strict transform is the
//! prerequisite for ever sending `strict: true`.

use cersei_types::ToolDefinition;
use serde_json::{json, Map, Value};
use std::collections::HashSet;

/// Which provider schema dialect to serialize tools into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchemaDialect {
    /// `{name, description, input_schema}`. Permissive; only the common
    /// cleanup (inline `$ref`, strip `$schema`/`definitions`) applies.
    AnthropicNative,
    /// OpenAI `{type:"function", function:{..., strict:true}}`. Strict mode
    /// requires `additionalProperties:false` and a full `required` list on
    /// every object node.
    OpenAiStrict,
    /// OpenAI `{type:"function", function:{...}}` without `strict`. The
    /// schema passes through apart from the common cleanup — in particular
    /// `additionalProperties` is preserved exactly as written.
    OpenAiLoose,
    /// Gemini `functionDeclarations` entry. Strips the four keys Gemini
    /// rejects; leaves `oneOf`/`anyOf`/`enum`/`format`/`default`/nesting
    /// alone — Exp 3 measured that Gemini accepts all of them.
    GeminiSubset,
}

/// Normalize once, at the only three places schemas cross the provider
/// boundary. Returns the full provider-shaped tool JSON for `dialect`.
///
/// For every dialect: tool names are sanitized to `^[a-zA-Z0-9_-]{1,64}$`
/// and collisions deduplicated with a numeric suffix. All 34 shipped tools
/// already have valid names, so today this is the identity on names; it
/// guards the MCP/custom-tool path (F-A11). If a rename ever fires for a
/// dispatchable tool, dispatch needs a reverse map — that wiring lands with
/// MCP itself, which is currently dead code (§9).
pub fn adapt_tools(tools: &[ToolDefinition], dialect: SchemaDialect) -> Vec<Value> {
    let mut used_names: HashSet<String> = HashSet::new();
    tools
        .iter()
        .map(|t| {
            let name = unique_name(sanitize_name(&t.name), &mut used_names);
            let schema = adapt_schema(&t.input_schema, dialect);
            match dialect {
                SchemaDialect::AnthropicNative => json!({
                    "name": name,
                    "description": t.description,
                    "input_schema": schema,
                }),
                SchemaDialect::GeminiSubset => json!({
                    "name": name,
                    "description": t.description,
                    "parameters": schema,
                }),
                SchemaDialect::OpenAiLoose => json!({
                    "type": "function",
                    "function": {
                        "name": name,
                        "description": t.description,
                        "parameters": schema,
                    }
                }),
                SchemaDialect::OpenAiStrict => json!({
                    "type": "function",
                    "function": {
                        "name": name,
                        "description": t.description,
                        "parameters": schema,
                        "strict": true,
                    }
                }),
            }
        })
        .collect()
}

/// Map every character outside `[a-zA-Z0-9_-]` to `_`, cap at 64, and never
/// return the empty string (an empty name is rejected by every provider).
fn sanitize_name(raw: &str) -> String {
    let mut s: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    s.truncate(64);
    if s.is_empty() {
        s.push_str("tool");
    }
    s
}

/// Deduplicate post-sanitization collisions with `_2`, `_3`, … while keeping
/// the result within the 64-char cap.
fn unique_name(base: String, used: &mut HashSet<String>) -> String {
    if used.insert(base.clone()) {
        return base;
    }
    for n in 2u32.. {
        let suffix = format!("_{n}");
        let mut candidate = base.clone();
        candidate.truncate(64 - suffix.len());
        candidate.push_str(&suffix);
        if used.insert(candidate.clone()) {
            return candidate;
        }
    }
    unreachable!("u32 suffixes exhausted")
}

/// Adapt one `input_schema` for `dialect`. Non-object schemas (`true`,
/// `false`, or malformed) pass through untouched — there is nothing to strip
/// and inventing structure here would be worse than sending them as-is.
fn adapt_schema(schema: &Value, dialect: SchemaDialect) -> Value {
    let definitions = collect_definitions(schema);
    let mut stack: Vec<String> = Vec::new();
    rewrite(schema, dialect, &definitions, &mut stack)
}

/// The `$ref` targets reachable from the schema root: `definitions` (draft-07,
/// what schemars 0.8 emits) and `$defs` (2019-09+).
fn collect_definitions(schema: &Value) -> Map<String, Value> {
    let mut defs = Map::new();
    for key in ["definitions", "$defs"] {
        if let Some(Value::Object(m)) = schema.get(key) {
            for (k, v) in m {
                defs.insert(k.clone(), v.clone());
            }
        }
    }
    defs
}

fn rewrite(
    node: &Value,
    dialect: SchemaDialect,
    defs: &Map<String, Value>,
    stack: &mut Vec<String>,
) -> Value {
    match node {
        Value::Array(items) => Value::Array(
            items.iter().map(|v| rewrite(v, dialect, defs, stack)).collect(),
        ),
        Value::Object(map) => {
            // `$ref` first: draft-07 semantics replace the whole node with the
            // resolved target (siblings are ignored). A cyclic or unresolvable
            // ref degrades to the node minus its `$ref` key — the key itself
            // must go (Gemini rejects it and kills the request), and an empty
            // `{}` is a permissive schema, which the tool's own deserializer
            // still backstops.
            if let Some(Value::String(r)) = map.get("$ref") {
                if let Some(def_name) = local_def_name(r) {
                    if !stack.iter().any(|s| s == def_name) {
                        if let Some(target) = defs.get(def_name) {
                            stack.push(def_name.to_string());
                            let resolved = rewrite(target, dialect, defs, stack);
                            stack.pop();
                            return resolved;
                        }
                    }
                }
            }

            let mut out = Map::new();
            for (k, v) in map {
                // Stripped in every dialect: `$ref` survives only via the
                // resolution above; `$schema` is noise everywhere and rejected
                // by Gemini; `definitions`/`$defs` are dead once refs are
                // inlined and rejected by Gemini.
                if k == "$ref" || k == "$schema" || k == "definitions" || k == "$defs" {
                    continue;
                }
                if dialect == SchemaDialect::GeminiSubset && k == "additionalProperties" {
                    continue;
                }
                out.insert(k.clone(), rewrite(v, dialect, defs, stack));
            }

            if dialect == SchemaDialect::OpenAiStrict {
                let is_object_node = out.contains_key("properties")
                    || out.get("type").and_then(Value::as_str) == Some("object");
                if is_object_node {
                    out.insert("additionalProperties".to_string(), Value::Bool(false));
                    let all_props: Vec<Value> = out
                        .get("properties")
                        .and_then(Value::as_object)
                        .map(|p| p.keys().cloned().map(Value::String).collect())
                        .unwrap_or_default();
                    out.insert("required".to_string(), Value::Array(all_props));
                }
            }

            Value::Object(out)
        }
        other => other.clone(),
    }
}

/// `#/definitions/Name` or `#/$defs/Name` → `Name`. Anything else (external
/// URLs, JSON-pointer paths into the schema body) is not resolvable here.
fn local_def_name(r: &str) -> Option<&str> {
    r.strip_prefix("#/definitions/")
        .or_else(|| r.strip_prefix("#/$defs/"))
        .filter(|name| !name.is_empty() && !name.contains('/'))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shape schemars 0.8's `schema_for!` emits — the exact shape Exp 1
    /// measured Gemini rejecting: `$schema` at the root, a `$ref` inside
    /// `properties`, the target under `definitions`, plus the constructs
    /// Exp 3 measured Gemini *accepting*, so the tests can pin that they
    /// survive.
    fn schemars_like_tool() -> ToolDefinition {
        ToolDefinition {
            name: "Read".to_string(),
            description: "Reads a file".to_string(),
            input_schema: json!({
                "$schema": "http://json-schema.org/draft-07/schema#",
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "file_path": { "type": "string", "format": "path", "default": "/" },
                    "mode": { "enum": ["full", "head"] },
                    "range": { "$ref": "#/definitions/Range" },
                    "variant": { "oneOf": [ { "type": "string" }, { "type": "integer" } ] },
                    "alt": { "anyOf": [ { "type": "string" }, { "type": "null" } ] },
                },
                "required": ["file_path"],
                "definitions": {
                    "Range": {
                        "type": "object",
                        "properties": {
                            "start": { "type": "integer", "minimum": 0 },
                            "end": { "type": "integer" },
                        },
                        "required": ["start"],
                    }
                }
            }),
        }
    }

    fn adapt_one(dialect: SchemaDialect) -> Value {
        adapt_tools(&[schemars_like_tool()], dialect)
            .pop()
            .expect("one tool in, one tool out")
    }

    fn schema_of(tool: &Value, dialect: SchemaDialect) -> &Value {
        match dialect {
            SchemaDialect::AnthropicNative => &tool["input_schema"],
            SchemaDialect::GeminiSubset => &tool["parameters"],
            SchemaDialect::OpenAiLoose | SchemaDialect::OpenAiStrict => {
                &tool["function"]["parameters"]
            }
        }
    }

    /// True if `key` appears as an object key anywhere in the tree.
    fn contains_key(v: &Value, key: &str) -> bool {
        match v {
            Value::Object(m) => {
                m.contains_key(key) || m.values().any(|v| contains_key(v, key))
            }
            Value::Array(items) => items.iter().any(|v| contains_key(v, key)),
            _ => false,
        }
    }

    // ─── GeminiSubset: the four measured-rejected keys ───────────────────────

    #[test]
    fn gemini_strips_all_four_rejected_keys_at_every_depth() {
        let tool = adapt_one(SchemaDialect::GeminiSubset);
        let schema = schema_of(&tool, SchemaDialect::GeminiSubset);
        for key in ["$schema", "$ref", "definitions", "additionalProperties"] {
            assert!(
                !contains_key(schema, key),
                "Gemini rejects `{key}` and the rejection kills the whole \
                 request (Exp 1/3); it must not survive adaptation: {schema:#}"
            );
        }
    }

    #[test]
    fn gemini_preserves_the_eight_measured_accepted_constructs() {
        let tool = adapt_one(SchemaDialect::GeminiSubset);
        let schema = schema_of(&tool, SchemaDialect::GeminiSubset);
        // Exp 3: nesting, enum, oneOf, anyOf, format, default, minimum all
        // accepted — stripping them would shrink the schema for no reason.
        assert!(contains_key(schema, "oneOf"), "{schema:#}");
        assert!(contains_key(schema, "anyOf"), "{schema:#}");
        assert!(contains_key(schema, "enum"), "{schema:#}");
        assert!(contains_key(schema, "format"), "{schema:#}");
        assert!(contains_key(schema, "default"), "{schema:#}");
        assert!(contains_key(schema, "minimum"), "{schema:#}");
        // The $ref was inlined, not dropped: Range's properties are in place.
        assert_eq!(schema["properties"]["range"]["properties"]["start"]["type"], "integer");
    }

    // ─── OpenAiStrict ────────────────────────────────────────────────────────

    #[test]
    fn strict_forces_additional_properties_false_and_full_required_recursively() {
        let tool = adapt_one(SchemaDialect::OpenAiStrict);
        let schema = schema_of(&tool, SchemaDialect::OpenAiStrict);
        assert_eq!(schema["additionalProperties"], json!(false));
        let mut required: Vec<&str> = schema["required"]
            .as_array()
            .expect("strict requires a full required list")
            .iter()
            .filter_map(Value::as_str)
            .collect();
        required.sort_unstable();
        assert_eq!(required, ["alt", "file_path", "mode", "range", "variant"]);
        // The inlined nested object gets the same treatment.
        let nested = &schema["properties"]["range"];
        assert_eq!(nested["additionalProperties"], json!(false));
        let mut nested_req: Vec<&str> = nested["required"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .collect();
        nested_req.sort_unstable();
        assert_eq!(nested_req, ["end", "start"]);
        assert_eq!(tool["function"]["strict"], json!(true));
    }

    /// The load-bearing measured fact from Exp 3: `additionalProperties:
    /// false` is REQUIRED by OpenAI strict and REJECTED by Gemini. If one
    /// normalization pass could serve both, this test could not pass.
    #[test]
    fn strict_and_gemini_are_irreconcilable_on_additional_properties() {
        let strict = adapt_one(SchemaDialect::OpenAiStrict);
        let gemini = adapt_one(SchemaDialect::GeminiSubset);
        assert!(contains_key(
            schema_of(&strict, SchemaDialect::OpenAiStrict),
            "additionalProperties"
        ));
        assert!(!contains_key(
            schema_of(&gemini, SchemaDialect::GeminiSubset),
            "additionalProperties"
        ));
    }

    // ─── OpenAiLoose / AnthropicNative: common cleanup only ──────────────────

    #[test]
    fn loose_and_native_inline_refs_strip_schema_and_touch_nothing_else() {
        for dialect in [SchemaDialect::OpenAiLoose, SchemaDialect::AnthropicNative] {
            let tool = adapt_one(dialect);
            let schema = schema_of(&tool, dialect);
            assert!(!contains_key(schema, "$schema"), "{dialect:?}: {schema:#}");
            assert!(!contains_key(schema, "$ref"), "{dialect:?}: {schema:#}");
            assert!(!contains_key(schema, "definitions"), "{dialect:?}: {schema:#}");
            // Preserved exactly as written — loose must NOT invent strict
            // constraints, and the author's `required` list survives.
            assert_eq!(schema["additionalProperties"], json!(false), "{dialect:?}");
            assert_eq!(schema["required"], json!(["file_path"]), "{dialect:?}");
            assert_eq!(
                schema["properties"]["range"]["properties"]["end"]["type"], "integer",
                "{dialect:?}: the ref target must be inlined in place"
            );
        }
    }

    #[test]
    fn wrappers_match_each_provider_wire_shape() {
        let native = adapt_one(SchemaDialect::AnthropicNative);
        assert!(native.get("input_schema").is_some());
        assert!(native.get("type").is_none());

        let gemini = adapt_one(SchemaDialect::GeminiSubset);
        assert!(gemini.get("parameters").is_some());
        assert!(gemini.get("input_schema").is_none());

        let loose = adapt_one(SchemaDialect::OpenAiLoose);
        assert_eq!(loose["type"], "function");
        assert_eq!(loose["function"]["name"], "Read");
        assert!(loose["function"].get("strict").is_none());
    }

    // ─── Names ───────────────────────────────────────────────────────────────

    #[test]
    fn invalid_names_are_sanitized_and_collisions_deduped() {
        let tool = |name: &str| ToolDefinition {
            name: name.to_string(),
            description: String::new(),
            input_schema: json!({"type": "object"}),
        };
        let tools = [tool("my.tool"), tool("my tool"), tool("my_tool"), tool("")];
        let out = adapt_tools(&tools, SchemaDialect::GeminiSubset);
        let names: Vec<&str> = out.iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert_eq!(names, ["my_tool", "my_tool_2", "my_tool_3", "tool"]);
        let re_valid = |n: &str| {
            !n.is_empty()
                && n.len() <= 64
                && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        };
        assert!(names.iter().all(|n| re_valid(n)), "{names:?}");
    }

    #[test]
    fn valid_names_pass_through_untouched_and_long_names_are_capped() {
        assert_eq!(sanitize_name("Read"), "Read");
        assert_eq!(sanitize_name("mcp__server-1_list"), "mcp__server-1_list");
        let long = "x".repeat(80);
        assert_eq!(sanitize_name(&long).len(), 64);
    }

    // ─── Degenerate schemas ──────────────────────────────────────────────────

    #[test]
    fn ref_cycle_terminates_and_the_ref_key_still_dies() {
        let tool = ToolDefinition {
            name: "cyclic".to_string(),
            description: String::new(),
            input_schema: json!({
                "type": "object",
                "properties": { "node": { "$ref": "#/definitions/Node" } },
                "definitions": {
                    "Node": {
                        "type": "object",
                        "properties": { "next": { "$ref": "#/definitions/Node" } }
                    }
                }
            }),
        };
        let out = adapt_tools(&[tool], SchemaDialect::GeminiSubset);
        let schema = &out[0]["parameters"];
        // One level inlined; the cyclic re-entry degraded to a permissive
        // node; and no `$ref` key survived anywhere.
        assert_eq!(schema["properties"]["node"]["type"], "object");
        assert!(!contains_key(schema, "$ref"), "{schema:#}");
        assert!(!contains_key(schema, "definitions"), "{schema:#}");
    }

    #[test]
    fn unresolvable_ref_is_dropped_not_kept() {
        let tool = ToolDefinition {
            name: "ext".to_string(),
            description: String::new(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "x": { "$ref": "https://example.com/schema.json", "description": "kept" }
                }
            }),
        };
        let out = adapt_tools(&[tool], SchemaDialect::GeminiSubset);
        let schema = &out[0]["parameters"];
        assert!(!contains_key(schema, "$ref"), "{schema:#}");
        assert_eq!(schema["properties"]["x"]["description"], "kept");
    }

    #[test]
    fn non_object_schema_passes_through() {
        let tool = ToolDefinition {
            name: "odd".to_string(),
            description: String::new(),
            input_schema: json!(true),
        };
        let out = adapt_tools(&[tool], SchemaDialect::AnthropicNative);
        assert_eq!(out[0]["input_schema"], json!(true));
    }
}
