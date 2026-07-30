//! Model-facing failure text.
//!
//! Every message produced here is read by a language model — often a weak 7B
//! one — and is the *only* thing it has to work from when a call fails. The
//! audit measured this directly: two 7-8B models given the same bad `Read`
//! call recovered 6/12 times from Cersei's old `Invalid input: {serde error}`
//! and 12/12 times from a message that named the tool, echoed the arguments,
//! and included the schema. So these strings are load-bearing, not cosmetic.
//!
//! Three rules, in order:
//!
//! 1. **Name the tool.** With parallel tool calls in flight, a nameless
//!    `Invalid input: ...` cannot be attributed to a call, and the model
//!    retries the wrong one.
//! 2. **Echo what the model actually sent**, elided so a failed `Write` does
//!    not re-inject a whole file into the conversation.
//! 3. **Say what to send instead**, concretely — the parameter list, and where
//!    possible a corrected copy of the call itself.
//!
//! Size discipline is a correctness constraint here, not taste:
//! `runner.rs` skips `cap_tool_result` on the `is_error` branch, so nothing
//! downstream trims this text and it lands in history on every retry. This
//! module is the only limiter; see [`MAX_MESSAGE_CHARS`].

use crate::{Tool, ToolResult};
use serde_json::Value;
use std::fmt::Display;

// ─── Budgets ─────────────────────────────────────────────────────────────────

/// Hard ceiling on any message this module produces. Error results bypass
/// `cap_tool_result` in the runner, so this is the only cap that applies.
pub const MAX_MESSAGE_CHARS: usize = 1200;
/// Cap on the echoed raw text from a wire-level JSON failure.
const MAX_RAW_ECHO_CHARS: usize = 400;
/// Cap on any single string value inside the echo of what the model sent.
const MAX_STRING_VALUE_CHARS: usize = 200;
/// Cap on the serialized echo as a whole.
const MAX_ECHO_CHARS: usize = 420;
/// Include the verbatim JSON schema only when it is at most this long;
/// otherwise the Required/Optional table carries the information alone.
const MAX_SCHEMA_JSON_CHARS: usize = 600;
/// Cap on the underlying deserializer message.
const MAX_ERR_CHARS: usize = 240;
/// Cap on a rendered parameter table.
const MAX_PARAM_TABLE_CHARS: usize = 300;
/// Cap on an enumerated "available values" list.
const MAX_AVAILABLE_LIST_CHARS: usize = 600;
/// Strings longer than this are not reproduced in a suggested example call —
/// a truncated body copied back verbatim would corrupt the caller's data.
const MAX_EXAMPLE_STRING_CHARS: usize = 80;

// ─── Phase 1 wire markers ────────────────────────────────────────────────────

/// Key written by `cersei-provider`'s stream accumulator when the model's
/// tool-call arguments failed to parse as JSON on the wire. Its presence means
/// a serde "expected struct Input" message is meaningless — the JSON never
/// parsed at all — so we report the raw text instead.
pub const PARSE_ERROR_KEY: &str = "__parse_error";
/// Key carrying the original argument text that failed to parse.
pub const RAW_KEY: &str = "__raw";

/// True when `input` is the marker object the provider emits for arguments
/// that were malformed on the wire.
pub fn is_wire_parse_failure(input: &Value) -> bool {
    input.get(PARSE_ERROR_KEY).and_then(Value::as_str).is_some()
        && input.get(RAW_KEY).and_then(Value::as_str).is_some()
}

// ─── Public API ──────────────────────────────────────────────────────────────

/// Deserialize a tool's `Input` struct, or produce a model-actionable failure.
///
/// Takes the input **by reference** on purpose: `serde_json::from_value`
/// consumes the `Value`, which is exactly why no existing call site could echo
/// what the model sent. `T::deserialize(&Value)` leaves it alive for the echo.
///
/// ```ignore
/// let input: Input = match tool_feedback::parse_input(self, &input) {
///     Ok(i) => i,
///     Err(e) => return e,
/// };
/// ```
pub fn parse_input<T, S>(tool: &S, input: &Value) -> Result<T, ToolResult>
where
    T: serde::de::DeserializeOwned,
    S: Tool + ?Sized,
{
    match T::deserialize(input) {
        Ok(v) => Ok(v),
        Err(e) => Err(invalid_input(tool, input, e)),
    }
}

/// Build the "your arguments were rejected" message for a tool that validated
/// its input by hand rather than through serde.
pub fn invalid_input<S: Tool + ?Sized>(tool: &S, sent: &Value, err: impl Display) -> ToolResult {
    ToolResult::error(invalid_input_message(
        tool.name(),
        &tool.input_schema(),
        sent,
        err,
    ))
}

/// [`invalid_input`] without the [`Tool`] indirection — for tests and for
/// callers that hold a name and schema but not the tool itself.
pub fn invalid_input_message(
    name: &str,
    schema: &Value,
    sent: &Value,
    err: impl Display,
) -> String {
    let (req, opt) = schema_params(schema);
    let known: Vec<String> = req
        .iter()
        .chain(opt.iter())
        .map(|(n, _)| n.clone())
        .collect();

    let mut head = String::new();
    let wire_failure = is_wire_parse_failure(sent);
    let mut renames: Vec<(String, String)> = Vec::new();

    if wire_failure {
        // BRANCH A — the model's JSON never parsed. A serde type error about a
        // Rust struct it has never seen would be actively misleading here.
        let parse_err = sent
            .get(PARSE_ERROR_KEY)
            .and_then(Value::as_str)
            .unwrap_or("unknown parse error");
        let raw = sent.get(RAW_KEY).and_then(Value::as_str).unwrap_or("");
        head.push_str(&format!(
            "Tool '{name}' did not run: the arguments you sent were not valid JSON, so they could not be read at all.\n\n"
        ));
        head.push_str(&format!(
            "JSON parse error: {}\n",
            truncate(parse_err, MAX_ERR_CHARS)
        ));
        head.push_str(&format!(
            "This is the exact text you sent:\n{}\n\n",
            truncate(raw, MAX_RAW_ECHO_CHARS)
        ));
    } else {
        // BRANCH B — well-formed JSON, wrong shape.
        head.push_str(&format!(
            "Tool '{name}' rejected your arguments: {}\n\n",
            truncate(&err.to_string(), MAX_ERR_CHARS)
        ));
        head.push_str(&format!("You sent: {}\n", echo(sent)));

        let misses = near_misses(sent, &known);
        for (sent_key, matched) in &misses {
            match matched {
                Some(param) => {
                    head.push_str(&format!(
                        "You used '{sent_key}', but this tool's parameter is named '{param}'. Rename it.\n"
                    ));
                    renames.push((sent_key.clone(), param.clone()));
                }
                None => head.push_str(&format!(
                    "'{sent_key}' is not a parameter of '{name}'. Remove it.\n"
                )),
            }
        }
        head.push('\n');
    }

    // Parameter table — the part a weak model actually acts on.
    if req.is_empty() && opt.is_empty() {
        head.push_str(&format!(
            "'{name}' takes no parameters: send an empty JSON object.\n"
        ));
    } else {
        head.push_str(&format!(
            "Required parameters: {}\n",
            if req.is_empty() {
                "(none)".to_string()
            } else {
                truncate(&param_list(&req), MAX_PARAM_TABLE_CHARS)
            }
        ));
        if !opt.is_empty() {
            head.push_str(&format!(
                "Optional parameters: {}\n",
                truncate(&param_list(&opt), MAX_PARAM_TABLE_CHARS)
            ));
        }
    }

    let schema_json = serde_json::to_string(schema).unwrap_or_default();
    let with_schema = if schema_json.chars().count() <= MAX_SCHEMA_JSON_CHARS {
        format!("Full schema for '{name}': {schema_json}\n")
    } else {
        String::new()
    };

    // Tail: the imperative. Never truncated — it is the whole point.
    let mut tail = String::new();
    let example = example_call(sent, &req, &opt, &renames, wire_failure);
    tail.push_str(&format!(
        "\nRetry '{name}' now, sending exactly this JSON object (fill in any placeholder):\n{example}\n"
    ));
    if wire_failure {
        tail.push_str(
            "Use double quotes around every key and every string value. No markdown fences, no trailing commas, no single quotes.\n",
        );
    }

    assemble(head, with_schema, tail)
}

/// Reject any key the tool does not declare, phrased exactly as serde phrases it.
///
/// `Edit` and `MultiEdit` parse their arguments by hand — they coerce numbers
/// and stringified booleans that a derived `Deserialize` would refuse — so they
/// cannot inherit `#[serde(deny_unknown_fields)]`. Reusing serde's exact wording
/// keeps one policy readable as one policy, at the wire and in tests, rather
/// than as two rules that happen to agree today.
pub fn reject_unknown_keys(input: &Value, known: &[&str]) -> std::result::Result<(), String> {
    let Some(obj) = input.as_object() else {
        return Ok(()); // shape errors are the caller's to report, with better context
    };
    for key in obj.keys() {
        if known.contains(&key.as_str()) {
            continue;
        }
        return Err(format!("unknown field `{key}`, {}", expected_one_of(known)));
    }
    Ok(())
}

/// serde's `OneOf` phrasing, which branches on arity: one field renders as
/// "expected `a`", two as "expected `a` or `b`", and only three or more as
/// "expected one of `a`, `b`, `c`". Reproducing the branching matters because
/// `MultiEdit` declares exactly two top-level parameters — emitting the plural
/// form there would make the hand-rolled path diverge from every two-field
/// serde tool (`Write`, `Bash`, `CronCreate`, …) for no reason a reader could
/// see.
fn expected_one_of(known: &[&str]) -> String {
    let q: Vec<String> = known.iter().map(|k| format!("`{k}`")).collect();
    match q.len() {
        0 => "there are no fields".to_string(),
        1 => format!("expected {}", q[0]),
        2 => format!("expected {} or {}", q[0], q[1]),
        _ => format!("expected one of {}", q.join(", ")),
    }
}

/// Build a "not found, and here is what does exist" message.
///
/// This is the pattern `skill_tool.rs` already got right; every other
/// not-found site in the tree now routes through here so they cannot drift.
/// `kind` is a lowercase singular noun (`"tool"`, `"task"`, `"cron job"`).
pub fn not_found(kind: &str, id: &str, available: &[String], how_to_list: &str) -> ToolResult {
    ToolResult::error(not_found_message(kind, id, available, how_to_list))
}

/// [`not_found`] as a plain string.
pub fn not_found_message(kind: &str, id: &str, available: &[String], how_to_list: &str) -> String {
    // The literal substring "not found" is depended upon by callers and tests.
    let mut msg = format!("{} '{}' not found.", capitalize(kind), id);

    let candidates: Vec<&str> = available.iter().map(String::as_str).collect();
    if let Some(best) = closest(id, &candidates) {
        msg.push_str(&format!("\n\nDid you mean: {best}?"));
    }

    if available.is_empty() {
        msg.push_str(&format!("\n\nThere are no {kind}s available right now."));
    } else {
        let (list, shown) = join_capped(&candidates, MAX_AVAILABLE_LIST_CHARS);
        if shown < available.len() {
            msg.push_str(&format!(
                "\n\nAvailable {kind}s ({} total): {list}, … and {} more.",
                available.len(),
                available.len() - shown
            ));
        } else {
            msg.push_str(&format!(
                "\n\nAvailable {kind}s ({}): {list}",
                available.len()
            ));
        }
    }

    if !how_to_list.is_empty() {
        msg.push_str(&format!("\n\n{how_to_list}"));
    }
    truncate(&msg, MAX_MESSAGE_CHARS)
}

/// Describe a windowed result so the model can tell truncation from EOF.
///
/// Returns `None` only when the window genuinely covered everything from the
/// start, in which case silence is correct. `unit` is a plural noun
/// (`"lines"`). `how_to_continue` is an imperative telling the model how to
/// fetch the rest.
pub fn window_notice(
    offset: usize,
    returned: usize,
    total: usize,
    unit: &str,
    how_to_continue: &str,
) -> Option<String> {
    if returned == 0 {
        if total == 0 {
            return Some(format!("[This file is empty — 0 {unit}.]"));
        }
        // Offset past the end currently yields a blank success: the model sees
        // nothing at all and concludes the file is empty.
        return Some(format!(
            "[No {unit} returned. You asked to start at offset {offset}, but there are only {total} {unit} (valid offsets are 0 to {}). Retry with a smaller offset.]",
            total.saturating_sub(1)
        ));
    }

    let first = offset + 1;
    let last = offset + returned;

    if last < total {
        let remaining = total - last;
        return Some(format!(
            "[Showing {unit} {first}-{last} of {total}. This is NOT the end of the file — {remaining} more {unit} were not shown. Do not assume content ends here. {how_to_continue}]"
        ));
    }

    if offset > 0 {
        return Some(format!(
            "[Showing {unit} {first}-{last} of {total}. This is the end of the file.]"
        ));
    }

    None
}

/// Best fuzzy match for `target` among `candidates`, or `None`.
///
/// Case-insensitive exact match, then separator-insensitive match
/// (`filePath` → `file_path`), then containment (the heuristic
/// `skill_tool.rs` already used), then edit distance ≤ 2.
pub fn closest<'a>(target: &str, candidates: &[&'a str]) -> Option<&'a str> {
    if target.is_empty() {
        return None;
    }
    let t = target.to_lowercase();

    if let Some(c) = candidates.iter().find(|c| c.to_lowercase() == t) {
        return Some(c);
    }

    let tn = normalize(target);
    if !tn.is_empty() {
        if let Some(c) = candidates.iter().find(|c| normalize(c) == tn) {
            return Some(c);
        }
    }

    if let Some(c) = candidates.iter().find(|c| {
        let cl = c.to_lowercase();
        !cl.is_empty() && (cl.contains(&t) || t.contains(&cl))
    }) {
        return Some(c);
    }

    let mut best: Option<(usize, &'a str)> = None;
    for c in candidates {
        let d = levenshtein(&t, &c.to_lowercase());
        if d <= 2 && best.map(|(bd, _)| d < bd).unwrap_or(true) {
            best = Some((d, c));
        }
    }
    best.map(|(_, c)| c)
}

// ─── Internals ───────────────────────────────────────────────────────────────

/// Fit head + optional schema + tail inside [`MAX_MESSAGE_CHARS`], dropping
/// the verbatim schema first and truncating the head only as a last resort.
/// The tail is never cut: it holds the instruction.
fn assemble(head: String, schema_section: String, tail: String) -> String {
    let full_len = head.chars().count() + schema_section.chars().count() + tail.chars().count();
    if full_len <= MAX_MESSAGE_CHARS {
        return format!("{head}{schema_section}{tail}");
    }
    // Drop the verbatim schema: the Required/Optional table is what a weak
    // model needs, and it is already in `head`.
    let without = head.chars().count() + tail.chars().count();
    if without <= MAX_MESSAGE_CHARS {
        return format!("{head}{tail}");
    }
    let room = MAX_MESSAGE_CHARS.saturating_sub(tail.chars().count());
    format!("{}{tail}", truncate(&head, room))
}

/// Split a flat JSON-Schema object into (required, optional) `(name, type)`
/// pairs. Every shipped schema is a hand-written flat `json!` object, so no
/// `$ref` resolution is needed.
fn schema_params(schema: &Value) -> (Vec<(String, String)>, Vec<(String, String)>) {
    let required: Vec<&str> = schema
        .get("required")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();

    let Some(props) = schema.get("properties").and_then(Value::as_object) else {
        return (
            required
                .iter()
                .map(|n| ((*n).to_string(), "any".to_string()))
                .collect(),
            Vec::new(),
        );
    };

    // Required first, in the schema's own `required` order.
    let mut req = Vec::new();
    for name in &required {
        let ty = props.get(*name).map(type_of).unwrap_or_else(|| "any".into());
        req.push(((*name).to_string(), ty));
    }
    let mut opt = Vec::new();
    for (name, p) in props {
        if !required.contains(&name.as_str()) {
            opt.push((name.clone(), type_of(p)));
        }
    }
    (req, opt)
}

fn type_of(p: &Value) -> String {
    if let Some(e) = p.get("enum").and_then(Value::as_array) {
        let vals: Vec<String> = e
            .iter()
            .take(8)
            .map(|v| match v {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            })
            .collect();
        return format!("one of: {}", vals.join(" | "));
    }
    match p.get("type") {
        Some(Value::String(s)) => {
            if s == "array" {
                if let Some(it) = p
                    .get("items")
                    .and_then(|i| i.get("type"))
                    .and_then(Value::as_str)
                {
                    return format!("array of {it}");
                }
            }
            s.clone()
        }
        Some(Value::Array(a)) => {
            let joined: Vec<&str> = a.iter().filter_map(Value::as_str).collect();
            if joined.is_empty() {
                "any".to_string()
            } else {
                joined.join("|")
            }
        }
        _ => "any".to_string(),
    }
}

fn param_list(params: &[(String, String)]) -> String {
    params
        .iter()
        .map(|(n, t)| format!("{n} ({t})"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Keys the model sent that are not parameters of this tool, each paired with
/// the closest unused real parameter (if any).
fn near_misses(sent: &Value, known: &[String]) -> Vec<(String, Option<String>)> {
    let Some(obj) = sent.as_object() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for key in obj.keys() {
        if key.starts_with("__") || known.iter().any(|k| k == key) {
            continue;
        }
        // Only suggest a rename onto a parameter the model has not already set.
        let unused: Vec<&str> = known
            .iter()
            .filter(|k| !obj.contains_key(k.as_str()))
            .map(String::as_str)
            .collect();
        out.push((key.clone(), closest(key, &unused).map(str::to_string)));
        if out.len() >= 3 {
            break;
        }
    }
    out
}

/// The corrected call to show the model: the arguments it actually sent, with
/// detected renames applied and any still-missing required parameter filled
/// with an obvious placeholder.
fn example_call(
    sent: &Value,
    req: &[(String, String)],
    opt: &[(String, String)],
    renames: &[(String, String)],
    wire_failure: bool,
) -> String {
    let mut m = serde_json::Map::new();
    let known: Vec<&str> = req
        .iter()
        .chain(opt.iter())
        .map(|(n, _)| n.as_str())
        .collect();

    if !wire_failure {
        if let Some(obj) = sent.as_object() {
            for (k, v) in obj {
                if k.starts_with("__") {
                    continue;
                }
                let target = if known.contains(&k.as_str()) {
                    Some(k.clone())
                } else {
                    renames
                        .iter()
                        .find(|(from, _)| from == k)
                        .map(|(_, to)| to.clone())
                };
                if let Some(t) = target {
                    m.insert(t, example_value(v));
                }
            }
        }
    }
    for (n, t) in req {
        if !m.contains_key(n) {
            m.insert(n.clone(), placeholder(t));
        }
    }
    let rendered = Value::Object(m).to_string();
    truncate(&rendered, MAX_ECHO_CHARS)
}

/// Values reproduced inside a suggested example. A long string is replaced
/// wholesale rather than truncated: a model that copies a truncated file body
/// back into a Write would destroy the file.
fn example_value(v: &Value) -> Value {
    match v {
        Value::String(s) if s.chars().count() > MAX_EXAMPLE_STRING_CHARS => {
            Value::String("<keep the same value you sent>".to_string())
        }
        Value::Array(_) | Value::Object(_) => {
            let rendered = v.to_string();
            if rendered.chars().count() > MAX_EXAMPLE_STRING_CHARS {
                Value::String("<keep the same value you sent>".to_string())
            } else {
                v.clone()
            }
        }
        other => other.clone(),
    }
}

fn placeholder(ty: &str) -> Value {
    if let Some(rest) = ty.strip_prefix("one of: ") {
        return Value::String(rest.split(" | ").next().unwrap_or("").to_string());
    }
    match ty {
        "integer" | "number" => Value::Number(0.into()),
        "boolean" => Value::Bool(false),
        "object" => Value::Object(serde_json::Map::new()),
        t if t.starts_with("array") => Value::Array(Vec::new()),
        _ => Value::String("<your value here>".to_string()),
    }
}

/// Compact rendering of what the model sent, with oversized string values
/// elided so a failed Write does not duplicate a whole file into history.
fn echo(sent: &Value) -> String {
    truncate(&elide(sent).to_string(), MAX_ECHO_CHARS)
}

fn elide(v: &Value) -> Value {
    match v {
        Value::String(s) => Value::String(truncate(s, MAX_STRING_VALUE_CHARS)),
        Value::Array(a) => {
            let mut out: Vec<Value> = a.iter().take(10).map(elide).collect();
            if a.len() > 10 {
                out.push(Value::String(format!("<… {} more items …>", a.len() - 10)));
            }
            Value::Array(out)
        }
        Value::Object(o) => Value::Object(o.iter().map(|(k, v)| (k.clone(), elide(v))).collect()),
        other => other.clone(),
    }
}

fn truncate(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max {
        return s.to_string();
    }
    let head: String = s.chars().take(max.saturating_sub(24)).collect();
    format!("{head}… <truncated, {n} chars total>")
}

fn join_capped(items: &[&str], max: usize) -> (String, usize) {
    let mut out = String::new();
    let mut shown = 0usize;
    for it in items {
        let add = if out.is_empty() {
            it.len()
        } else {
            it.len() + 2
        };
        if !out.is_empty() && out.len() + add > max {
            break;
        }
        if !out.is_empty() {
            out.push_str(", ");
        }
        out.push_str(it);
        shown += 1;
    }
    if shown == 0 && !items.is_empty() {
        out = truncate(items[0], max);
        shown = 1;
    }
    (out, shown)
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

fn normalize(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.is_empty() {
        return b.len();
    }
    if b.is_empty() {
        return a.len();
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        cur[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            cur[j + 1] = (prev[j + 1] + 1).min(cur[j] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn read_schema() -> Value {
        json!({
            "type": "object",
            "properties": {
                "file_path": { "type": "string", "description": "Absolute path to the file" },
                "offset": { "type": "integer", "description": "Line number to start reading from" },
                "limit": { "type": "integer", "description": "Number of lines to read" }
            },
            "required": ["file_path"]
        })
    }

    /// The measured Exp-2 case: `{"path": …}` instead of `{"file_path": …}`.
    #[test]
    fn wrong_param_name_names_the_real_param() {
        let sent = json!({ "path": "/x.rs" });
        let msg = invalid_input_message(
            "Read",
            &read_schema(),
            &sent,
            "missing field `file_path`",
        );
        assert!(msg.contains("'Read'"), "must name the tool: {msg}");
        assert!(msg.contains("file_path"), "must name the real param: {msg}");
        assert!(msg.contains("/x.rs"), "must echo what was sent: {msg}");
        assert!(
            msg.contains("You used 'path'"),
            "must call out the near miss: {msg}"
        );
        assert!(
            msg.contains(r#"{"file_path":"/x.rs"}"#),
            "must show the corrected call: {msg}"
        );
        assert!(msg.chars().count() <= MAX_MESSAGE_CHARS, "over budget: {msg}");
    }

    #[test]
    fn wire_parse_failure_reports_raw_text_not_a_rust_type() {
        let sent = json!({
            PARSE_ERROR_KEY: "EOF while parsing a string at line 1 column 22",
            RAW_KEY: "{\"file_path\": \"/x/y.rs",
        });
        let msg = invalid_input_message(
            "Read",
            &read_schema(),
            &sent,
            "invalid type: map, expected struct Input",
        );
        assert!(msg.contains("not valid JSON"), "{msg}");
        assert!(msg.contains("EOF while parsing"), "{msg}");
        assert!(msg.contains("{\"file_path\": \"/x/y.rs"), "{msg}");
        assert!(
            !msg.contains("struct Input"),
            "must not leak a Rust type name here: {msg}"
        );
        assert!(msg.contains("double quotes"), "{msg}");
        assert!(msg.chars().count() <= MAX_MESSAGE_CHARS, "over budget");
    }

    #[test]
    fn no_arg_call_to_a_tool_with_required_params() {
        let sent = json!({});
        let msg =
            invalid_input_message("Read", &read_schema(), &sent, "missing field `file_path`");
        assert!(msg.contains("Required parameters: file_path (string)"), "{msg}");
        assert!(msg.contains("Optional parameters:"), "{msg}");
        assert!(msg.contains("<your value here>"), "{msg}");
    }

    #[test]
    fn huge_payloads_do_not_blow_the_budget() {
        let big = "x".repeat(200_000);
        let schema = json!({
            "type": "object",
            "properties": {
                "file_path": {"type": "string"},
                "content": {"type": "string"}
            },
            "required": ["file_path", "content"]
        });
        let sent = json!({ "filePath": "/a.rs", "content": big });
        let msg = invalid_input_message("Write", &schema, &sent, "missing field `file_path`");
        assert!(
            msg.chars().count() <= MAX_MESSAGE_CHARS,
            "budget blown: {} chars",
            msg.chars().count()
        );
        // The file body must not be reproduced.
        assert!(!msg.contains(&"x".repeat(300)));
        assert!(msg.contains("keep the same value you sent"), "{msg}");
    }

    #[test]
    fn oversized_schema_is_dropped_but_the_table_survives() {
        let mut props = serde_json::Map::new();
        for i in 0..40 {
            props.insert(
                format!("param_{i}"),
                json!({"type": "string", "description": "a".repeat(60)}),
            );
        }
        let schema = json!({"type":"object","properties":props,"required":["param_0"]});
        let msg = invalid_input_message("Big", &schema, &json!({}), "missing field `param_0`");
        assert!(msg.chars().count() <= MAX_MESSAGE_CHARS);
        assert!(msg.contains("Required parameters: param_0 (string)"), "{msg}");
        assert!(msg.contains("Retry 'Big' now"), "{msg}");
    }

    #[test]
    fn not_found_keeps_the_literal_substring_and_lists_values() {
        let avail = vec!["ab12".to_string(), "cd34".to_string()];
        let msg = not_found_message("task", "ab13", &avail, "Run TaskList to see all tasks.");
        assert!(msg.contains("not found"), "{msg}");
        assert!(msg.starts_with("Task 'ab13' not found."), "{msg}");
        assert!(msg.contains("Did you mean: ab12?"), "{msg}");
        assert!(msg.contains("Available tasks (2): ab12, cd34"), "{msg}");
        assert!(msg.contains("Run TaskList"), "{msg}");
    }

    #[test]
    fn not_found_with_nothing_available() {
        let msg = not_found_message("cron job", "x", &[], "Run CronList to see all jobs.");
        assert!(msg.contains("Cron job 'x' not found."), "{msg}");
        assert!(msg.contains("no cron jobs available"), "{msg}");
    }

    #[test]
    fn not_found_caps_a_huge_list() {
        let avail: Vec<String> = (0..500).map(|i| format!("tool_number_{i}")).collect();
        let msg = not_found_message("tool", "zzz", &avail, "Call ToolSearch.");
        assert!(msg.chars().count() <= MAX_MESSAGE_CHARS, "{}", msg.len());
        assert!(msg.contains("500 total"), "{msg}");
        assert!(msg.contains("more"), "{msg}");
    }

    #[test]
    fn window_notice_flags_truncation() {
        let n = window_notice(0, 2000, 5000, "lines", "Call Read again with offset=2000.")
            .expect("truncation must be reported");
        assert!(n.contains("Showing lines 1-2000 of 5000"), "{n}");
        assert!(n.contains("NOT the end"), "{n}");
        assert!(n.contains("3000 more lines"), "{n}");
        assert!(n.contains("offset=2000"), "{n}");
    }

    #[test]
    fn window_notice_is_silent_on_a_complete_read() {
        assert!(window_notice(0, 12, 12, "lines", "x").is_none());
    }

    #[test]
    fn window_notice_reports_the_tail_and_bad_offsets() {
        let tail = window_notice(4000, 1000, 5000, "lines", "x").unwrap();
        assert!(tail.contains("end of the file"), "{tail}");

        let past = window_notice(9000, 0, 5000, "lines", "x").unwrap();
        assert!(past.contains("No lines returned"), "{past}");
        assert!(past.contains("0 to 4999"), "{past}");

        let empty = window_notice(0, 0, 0, "lines", "x").unwrap();
        assert!(empty.contains("empty"), "{empty}");
    }

    #[test]
    fn closest_handles_the_real_aliases() {
        let known = ["file_path", "offset", "limit"];
        assert_eq!(closest("path", &known), Some("file_path"));
        assert_eq!(closest("filePath", &known), Some("file_path"));
        assert_eq!(closest("ofset", &known), Some("offset"));
        assert_eq!(closest("completely_unrelated_xyz", &known), None);
    }

    #[test]
    fn parse_input_round_trips() {
        use crate::{PermissionLevel, ToolCategory};

        struct Fake;
        #[async_trait::async_trait]
        impl Tool for Fake {
            fn name(&self) -> &str {
                "Read"
            }
            fn description(&self) -> &str {
                "d"
            }
            fn input_schema(&self) -> Value {
                read_schema()
            }
            fn permission_level(&self) -> PermissionLevel {
                PermissionLevel::ReadOnly
            }
            fn category(&self) -> ToolCategory {
                ToolCategory::FileSystem
            }
            async fn execute(&self, _i: Value, _c: &crate::ToolContext) -> ToolResult {
                ToolResult::success("")
            }
        }

        #[derive(Debug, serde::Deserialize)]
        struct Input {
            file_path: String,
        }

        let ok: Input = parse_input(&Fake, &json!({"file_path": "/a"})).unwrap();
        assert_eq!(ok.file_path, "/a");

        let err: ToolResult = parse_input::<Input, _>(&Fake, &json!({"path": "/a"})).unwrap_err();
        assert!(err.is_error);
        assert!(err.content.contains("file_path"), "{}", err.content);
        assert!(err.content.contains("'Read'"), "{}", err.content);
    }
}
