//! The shape a CLI model's reply must have, built from the tools of the call (ADR 0049).
//!
//! ARK's tools carry plain JSON Schema ([`ToolDef::parameters`]). A vendor CLI shapes its reply
//! with a schema of its own: Claude Code takes open objects, so a call's arguments are an
//! object. Codex takes only OpenAI's strict subset: every object closed, every key required, no
//! open object. There, each tool becomes one closed variant of the reply (its name as a
//! one-value enum, its arguments as its own schema made strict), so the arguments arrive as a
//! real object the grammar checked, never as JSON written inside a string.
//!
//! Making a schema strict changes it in ways that are undone exactly when the reply is read
//! ([`Decode`]):
//!
//! | Tool schema | In the reply schema | Read back |
//! |---|---|---|
//! | a key the tool does not require | required, and may be null | a null is dropped, unless the tool's schema accepts null |
//! | a part strict mode cannot express (an open object, `$ref`, a tuple, too deep) | a string holding JSON | parsed; not JSON is a malformed reply, never repaired |
//! | `oneOf`, `const`, constraint keywords, descriptions | `anyOf`, a one-value `enum`, left out | as written |
//!
//! A schema over OpenAI's size limits makes the whole reply use the older shape, with every
//! call's arguments as a JSON string ([`ReplyShape::legacy`]).

use crate::ToolDef;
use serde_json::{json, Map, Value};
use std::collections::{BTreeSet, HashMap, HashSet};

/// The older shape: arguments are a JSON string. Strict, for when a typed shape cannot be used.
const LEGACY: &str = crate::cli::REPLY_SCHEMA;

/// Claude Code's shape: arguments are an open object.
const OPEN: &str = crate::cli::CLAUDE_REPLY_SCHEMA;

/// Objects and arrays nested deeper than this fall back to a JSON string. OpenAI refuses an
/// 11th level of objects (measured 2026-10-07 through Codex 0.153); arrays are counted too,
/// which is stricter than measured.
const MAX_DEPTH: usize = 10;
/// The level of a call's `arguments` object: the reply, `tool_calls`, the call, the arguments.
const ARGUMENTS_DEPTH: usize = 4;
/// OpenAI's limits on one schema: properties, enum values, characters of property names and
/// enum values, and characters of one enum of more than 250 values.
const MAX_PROPERTIES: usize = 5000;
const MAX_ENUM_VALUES: usize = 1000;
const MAX_CHARS: usize = 120_000;
const MAX_LARGE_ENUM_CHARS: usize = 15_000;

/// What a part that cannot be an object in a strict schema says about itself.
const AS_TEXT: &str = "JSON, written as a string";

/// Keywords a strict schema cannot hold whose meaning cannot be dropped: a part using one
/// travels as a JSON string.
const UNSUPPORTED: &[&str] = &[
    "$ref",
    "$defs",
    "definitions",
    "not",
    "if",
    "then",
    "else",
    "patternProperties",
    "dependentSchemas",
    "dependentRequired",
    "dependencies",
    "prefixItems",
    "unevaluatedProperties",
    "unevaluatedItems",
    "propertyNames",
    "contains",
];

/// The reply schema of one call and how to read its tool calls' arguments back.
pub(crate) struct ReplyShape {
    pub(crate) schema: Value,
    /// Each call is a closed variant per tool ([`ReplyShape::typed`]).
    pub(crate) typed: bool,
    decode: HashMap<String, Decode>,
}

impl ReplyShape {
    /// Arguments as an open object (Claude Code).
    pub(crate) fn open() -> Self {
        ReplyShape {
            schema: serde_json::from_str(OPEN).expect("open reply schema"),
            typed: false,
            decode: HashMap::new(),
        }
    }

    /// Arguments as a JSON string, strict (Codex when a typed shape cannot be used).
    pub(crate) fn legacy() -> Self {
        ReplyShape {
            schema: serde_json::from_str(LEGACY).expect("legacy reply schema"),
            typed: false,
            decode: HashMap::new(),
        }
    }

    /// One closed variant per tool, strict. The legacy shape when there is no tool or the
    /// typed schema is over a limit.
    pub(crate) fn typed(tools: &[ToolDef]) -> Self {
        let mut seen = HashSet::new();
        let mut variants = Vec::new();
        let mut decode = HashMap::new();
        let mut size = Size::default();
        size.properties += 3;
        size.chars += "actiontexttool_calls".len() + "replycall_tools".len();
        size.values += 2;
        for t in tools.iter().filter(|t| seen.insert(t.name.as_str())) {
            let (arguments, d) = arguments_schema(&t.parameters, &mut size);
            size.properties += 2;
            size.chars += "namearguments".len() + t.name.len();
            size.values += 1;
            variants.push(json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "name": { "type": "string", "enum": [t.name] },
                    "arguments": arguments,
                },
                "required": ["name", "arguments"],
            }));
            decode.insert(t.name.clone(), d);
        }
        if variants.is_empty() {
            return ReplyShape::legacy();
        }
        if !size.within_limits() {
            tracing::warn!(
                tools = variants.len(),
                properties = size.properties,
                enum_values = size.values,
                chars = size.chars,
                "the typed reply schema is over a limit; tool arguments travel as JSON strings"
            );
            return ReplyShape::legacy();
        }
        let items = if variants.len() == 1 {
            variants.pop().expect("one variant")
        } else {
            json!({ "anyOf": variants })
        };
        ReplyShape {
            schema: json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "action": { "type": "string", "enum": ["reply", "call_tools"] },
                    "text": { "type": "string" },
                    "tool_calls": { "type": "array", "items": items },
                },
                "required": ["action", "text", "tool_calls"],
            }),
            typed: true,
            decode,
        }
    }

    /// The arguments of a call to `tool` as the tool's own schema has them. An object is used
    /// as it is, a string is parsed as JSON (empty: no arguments). A part that is not what the
    /// schema promised is an error: arguments are never guessed.
    pub(crate) fn arguments(&self, tool: &str, raw: &Value) -> Result<Value, String> {
        match self.decode.get(tool) {
            Some(d) if self.typed && *d != Decode::Json => d.apply(raw.clone(), ""),
            _ => match raw {
                Value::Object(_) => Ok(raw.clone()),
                Value::String(s) if s.trim().is_empty() => Ok(json!({})),
                Value::String(s) => {
                    serde_json::from_str(s.trim()).map_err(|e| format!("not JSON: {e}"))
                }
                _ => Ok(json!({})),
            },
        }
    }
}

/// How one part of a reply is read back into the tool's own shape.
#[derive(Clone, Debug, PartialEq)]
enum Decode {
    /// Already in the tool's shape.
    Same,
    /// A string holding JSON.
    Json,
    /// An object whose fields need reading back.
    Object(Vec<Field>),
    Array(Box<Decode>),
    /// One of several closed objects, told apart by their exact keys (all keys of a strict
    /// object are present, so a reply's keys name its variant).
    Variants(Vec<(BTreeSet<String>, Decode)>),
}

#[derive(Clone, Debug, PartialEq)]
struct Field {
    key: String,
    decode: Decode,
    /// The tool does not require the key and its schema does not accept null: a null can
    /// only mean the key was left out.
    drop_null: bool,
}

impl Decode {
    fn apply(&self, v: Value, at: &str) -> Result<Value, String> {
        match (self, v) {
            (Decode::Same, v) => Ok(v),
            (Decode::Json, Value::String(s)) => {
                serde_json::from_str(&s).map_err(|e| format!("{} is not JSON: {e}", place(at)))
            }
            (Decode::Json, other) => Err(format!(
                "{} should be JSON written as a string, got {other}",
                place(at)
            )),
            (Decode::Object(fields), Value::Object(mut m)) => {
                for f in fields {
                    match m.remove(&f.key) {
                        None => {}
                        Some(Value::Null) if f.drop_null => {}
                        Some(Value::Null) => {
                            m.insert(f.key.clone(), Value::Null);
                        }
                        Some(x) => {
                            let x = f.decode.apply(x, &format!("{at}.{}", f.key))?;
                            m.insert(f.key.clone(), x);
                        }
                    }
                }
                Ok(Value::Object(m))
            }
            (Decode::Array(d), Value::Array(items)) => items
                .into_iter()
                .enumerate()
                .map(|(i, x)| d.apply(x, &format!("{at}[{i}]")))
                .collect::<Result<Vec<_>, _>>()
                .map(Value::Array),
            (Decode::Variants(vs), Value::Object(m)) => {
                let keys: BTreeSet<String> = m.keys().cloned().collect();
                match vs.iter().find(|(k, _)| *k == keys) {
                    Some((_, d)) => d.apply(Value::Object(m), at),
                    None => Ok(Value::Object(m)),
                }
            }
            // A null, or a value of a branch that needs nothing read back.
            (_, v) => Ok(v),
        }
    }
}

fn place(at: &str) -> String {
    if at.is_empty() {
        "the arguments".into()
    } else {
        format!("`{}`", at.trim_start_matches('.'))
    }
}

/// What a schema adds toward OpenAI's limits.
#[derive(Default)]
struct Size {
    properties: usize,
    values: usize,
    chars: usize,
    large_enum_over: bool,
}

impl Size {
    fn within_limits(&self) -> bool {
        self.properties <= MAX_PROPERTIES
            && self.values <= MAX_ENUM_VALUES
            && self.chars <= MAX_CHARS
            && !self.large_enum_over
    }
}

/// A tool's arguments in the reply: its schema made strict, or a JSON string when the whole
/// object cannot be one.
fn arguments_schema(parameters: &Value, size: &mut Size) -> (Value, Decode) {
    // No parameters, or none declared: an empty object.
    let empty = parameters.is_null()
        || parameters.as_object().is_some_and(|m| m.is_empty())
        || (parameters["type"] == "object"
            && parameters.get("properties").is_none()
            && parameters["additionalProperties"] == false);
    if empty {
        return (
            json!({ "type": "object", "additionalProperties": false, "properties": {}, "required": [] }),
            Decode::Same,
        );
    }
    node(parameters, ARGUMENTS_DEPTH, size)
}

/// One part made strict, or a JSON string when strict mode cannot express it.
fn node(s: &Value, depth: usize, size: &mut Size) -> (Value, Decode) {
    strict(s, depth, size).unwrap_or_else(|| {
        (
            json!({ "type": "string", "description": AS_TEXT }),
            Decode::Json,
        )
    })
}

/// `s` in OpenAI's strict subset, rebuilt from the keywords it allows; `None` when a keyword
/// whose meaning would be lost cannot be expressed. `depth`: the level this part has if it is
/// an object or an array.
fn strict(s: &Value, depth: usize, size: &mut Size) -> Option<(Value, Decode)> {
    let m = s.as_object()?;
    if m.is_empty() || UNSUPPORTED.iter().any(|k| m.contains_key(*k)) {
        return None;
    }
    let (v, d) = strict_inner(m, depth, size)?;
    if m.get("nullable") == Some(&Value::Bool(true)) {
        return Some((or_null(v), d));
    }
    Some((v, d))
}

fn strict_inner(m: &Map<String, Value>, depth: usize, size: &mut Size) -> Option<(Value, Decode)> {
    if let Some(all) = m.get("allOf") {
        let [one] = all.as_array()?.as_slice() else {
            return None;
        };
        if structural(m) {
            return None;
        }
        return strict(one, depth, size);
    }
    if let Some(branches) = m.get("anyOf").or_else(|| m.get("oneOf")) {
        if structural(m) || m.contains_key("enum") {
            return None;
        }
        return any_of(branches.as_array()?, depth, size);
    }
    if let Some(values) = m
        .get("enum")
        .cloned()
        .or_else(|| m.get("const").map(|c| json!([c])))
    {
        return enumeration(values.as_array()?, size);
    }
    let kind = match m.get("type") {
        Some(Value::String(t)) => t.clone(),
        Some(Value::Array(ts)) => return type_list(m, ts, depth, size),
        Some(_) => return None,
        None if m.contains_key("properties") => "object".into(),
        None if m.contains_key("items") => "array".into(),
        None => return None,
    };
    match kind.as_str() {
        "object" => object(m, depth, size),
        "array" => array(m, depth, size),
        "string" | "number" | "integer" | "boolean" | "null" => {
            Some((json!({ "type": kind }), Decode::Same))
        }
        _ => None,
    }
}

/// Whether a part with `anyOf` or `allOf` also describes a shape of its own, which would have
/// to be merged into each branch.
fn structural(m: &Map<String, Value>) -> bool {
    ["type", "properties", "items", "additionalProperties"]
        .iter()
        .any(|k| m.contains_key(*k))
}

fn object(m: &Map<String, Value>, depth: usize, size: &mut Size) -> Option<(Value, Decode)> {
    if depth > MAX_DEPTH {
        return None;
    }
    // Extra keys of any shape cannot be expressed; a part without declared keys is open
    // unless it says it has none.
    match m.get("additionalProperties") {
        None | Some(Value::Bool(false)) => {}
        _ => return None,
    }
    let empty = Map::new();
    let props = match m.get("properties") {
        Some(Value::Object(p)) => p,
        Some(_) => return None,
        None if m.get("additionalProperties") == Some(&Value::Bool(false)) => &empty,
        None => return None,
    };
    let required: HashSet<&str> = m
        .get("required")
        .and_then(Value::as_array)
        .map(|r| r.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let mut out = Map::new();
    let mut fields = Vec::new();
    for (key, schema) in props {
        size.properties += 1;
        size.chars += key.len();
        let (v, d) = node(schema, depth + 1, size);
        let optional = !required.contains(key.as_str());
        let drop_null = optional && !accepts_null(schema);
        out.insert(key.clone(), if optional { or_null(v) } else { v });
        if d != Decode::Same || drop_null {
            fields.push(Field {
                key: key.clone(),
                decode: d,
                drop_null,
            });
        }
    }
    let keys: Vec<&String> = props.keys().collect();
    let schema = json!({
        "type": "object",
        "additionalProperties": false,
        "properties": out,
        "required": keys,
    });
    let decode = if fields.is_empty() {
        Decode::Same
    } else {
        Decode::Object(fields)
    };
    Some((schema, decode))
}

fn array(m: &Map<String, Value>, depth: usize, size: &mut Size) -> Option<(Value, Decode)> {
    if depth > MAX_DEPTH {
        return None;
    }
    let items = m.get("items").filter(|i| i.is_object())?;
    let (v, d) = node(items, depth + 1, size);
    let decode = match d {
        Decode::Same => Decode::Same,
        d => Decode::Array(Box::new(d)),
    };
    Some((json!({ "type": "array", "items": v }), decode))
}

/// `anyOf` (or `oneOf`, read as `anyOf`: a reply matching two branches is still one the tool
/// accepts under its own check). A branch that needs reading back must be a closed object
/// whose keys no other branch has, so the reply's keys name it.
fn any_of(branches: &[Value], depth: usize, size: &mut Size) -> Option<(Value, Decode)> {
    if branches.is_empty() {
        return None;
    }
    let mut out = Vec::new();
    // Each branch's keys when it is a closed object, and how it is read back.
    let mut parts: Vec<(Option<BTreeSet<String>>, Decode)> = Vec::new();
    for b in branches {
        let (v, d) = strict(b, depth, size)?;
        let keys = (v["type"] == "object").then(|| {
            v["properties"]
                .as_object()
                .map(|p| p.keys().cloned().collect())
                .unwrap_or_default()
        });
        out.push(v);
        parts.push((keys, d));
    }
    let mut variants = Vec::new();
    for (keys, d) in parts.iter().filter(|(_, d)| *d != Decode::Same) {
        // Only an object whose keys no other branch has can be named by its keys.
        let keys = keys.as_ref()?;
        if parts
            .iter()
            .filter(|(k, _)| k.as_ref() == Some(keys))
            .count()
            > 1
        {
            return None;
        }
        variants.push((keys.clone(), d.clone()));
    }
    let decode = if variants.is_empty() {
        Decode::Same
    } else {
        Decode::Variants(variants)
    };
    Some((json!({ "anyOf": out }), decode))
}

/// An `enum` of scalars, with its type next to it.
fn enumeration(values: &[Value], size: &mut Size) -> Option<(Value, Decode)> {
    if values.is_empty() || values.iter().any(|v| v.is_object() || v.is_array()) {
        return None;
    }
    let mut types: Vec<&str> = Vec::new();
    for v in values {
        let t = match v {
            Value::String(_) => "string",
            Value::Bool(_) => "boolean",
            Value::Null => "null",
            Value::Number(n) if n.is_f64() => "number",
            _ => "integer",
        };
        if !types.contains(&t) {
            types.push(t);
        }
    }
    if types.contains(&"number") {
        types.retain(|t| *t != "integer");
    }
    let chars: usize = values
        .iter()
        .map(|v| v.as_str().map_or_else(|| v.to_string().len(), str::len))
        .sum();
    size.values += values.len();
    size.chars += chars;
    if values.len() > 250 && chars > MAX_LARGE_ENUM_CHARS {
        size.large_enum_over = true;
    }
    let t = match types.as_slice() {
        [one] => json!(one),
        many => json!(many),
    };
    Some((json!({ "type": t, "enum": values }), Decode::Same))
}

/// `"type": [...]`: scalars stay a list; one object or array type with null is that part,
/// nullable.
fn type_list(
    m: &Map<String, Value>,
    ts: &[Value],
    depth: usize,
    size: &mut Size,
) -> Option<(Value, Decode)> {
    let ts: Vec<&str> = ts.iter().map(Value::as_str).collect::<Option<_>>()?;
    let scalar = |t: &&str| matches!(*t, "string" | "number" | "integer" | "boolean" | "null");
    if !ts.is_empty() && ts.iter().all(scalar) {
        return Some((json!({ "type": ts }), Decode::Same));
    }
    let shaped: Vec<&str> = ts.iter().copied().filter(|t| !scalar(t)).collect();
    let rest: Vec<&str> = ts.iter().copied().filter(|t| scalar(t)).collect();
    match (shaped.as_slice(), rest.as_slice()) {
        ([t], [] | ["null"]) => {
            let mut one = m.clone();
            one.insert("type".into(), json!(t));
            let (v, d) = strict_inner(&one, depth, size)?;
            Some((if rest.is_empty() { v } else { or_null(v) }, d))
        }
        _ => None,
    }
}

/// `v`, or null.
fn or_null(v: Value) -> Value {
    if allows_null(&v) {
        v
    } else {
        json!({ "anyOf": [v, { "type": "null" }] })
    }
}

/// Whether a strict part already accepts null.
fn allows_null(v: &Value) -> bool {
    match &v["type"] {
        Value::String(t) => t == "null",
        Value::Array(ts) => ts.iter().any(|t| t == "null"),
        _ => v["anyOf"]
            .as_array()
            .is_some_and(|bs| bs.iter().any(allows_null)),
    }
}

/// Whether a tool's own schema accepts null at this part.
fn accepts_null(s: &Value) -> bool {
    let Some(m) = s.as_object() else {
        return s == &Value::Bool(true);
    };
    m.is_empty()
        || m.get("nullable") == Some(&Value::Bool(true))
        || match m.get("type") {
            Some(Value::String(t)) => t == "null",
            Some(Value::Array(ts)) => ts.iter().any(|t| t == "null"),
            _ => false,
        }
        || m.get("enum")
            .and_then(Value::as_array)
            .is_some_and(|e| e.contains(&Value::Null))
        || m.get("const") == Some(&Value::Null)
        || ["anyOf", "oneOf"].iter().any(|k| {
            m.get(*k)
                .and_then(Value::as_array)
                .is_some_and(|bs| bs.iter().any(accepts_null))
        })
}

/// Whether `schema` is in OpenAI's strict subset as far as ARK uses it: every object closed
/// with every key required, arrays with one `items`, only known keywords, nesting within
/// [`MAX_DEPTH`]. For tests, here and in crates that check their own tools.
pub fn strict_problems(schema: &Value) -> Vec<String> {
    let mut out = Vec::new();
    check(schema, "$", 1, &mut out);
    out
}

fn check(s: &Value, at: &str, depth: usize, out: &mut Vec<String>) {
    let Some(m) = s.as_object() else {
        out.push(format!("{at}: not a schema object"));
        return;
    };
    const ALLOWED: &[&str] = &[
        "type",
        "properties",
        "required",
        "additionalProperties",
        "items",
        "enum",
        "anyOf",
        "description",
    ];
    for k in m.keys() {
        if !ALLOWED.contains(&k.as_str()) {
            out.push(format!("{at}: keyword `{k}`"));
        }
    }
    if let Some(bs) = m.get("anyOf") {
        for (i, b) in bs.as_array().into_iter().flatten().enumerate() {
            check(b, &format!("{at}.anyOf[{i}]"), depth, out);
        }
        return;
    }
    if m.contains_key("enum") && !m.contains_key("type") {
        out.push(format!("{at}: enum without type"));
    }
    match m.get("type").and_then(Value::as_str) {
        Some("object") => {
            if depth > MAX_DEPTH {
                out.push(format!("{at}: nested deeper than {MAX_DEPTH}"));
            }
            if m.get("additionalProperties") != Some(&Value::Bool(false)) {
                out.push(format!("{at}: object not closed"));
            }
            let props = m.get("properties").and_then(Value::as_object);
            let Some(props) = props else {
                out.push(format!("{at}: object without properties"));
                return;
            };
            let required: HashSet<&str> = m
                .get("required")
                .and_then(Value::as_array)
                .map(|r| r.iter().filter_map(Value::as_str).collect())
                .unwrap_or_default();
            for (k, p) in props {
                if !required.contains(k.as_str()) {
                    out.push(format!("{at}.{k}: not required"));
                }
                check(p, &format!("{at}.{k}"), depth + 1, out);
            }
        }
        Some("array") => {
            if depth > MAX_DEPTH {
                out.push(format!("{at}: nested deeper than {MAX_DEPTH}"));
            }
            match m.get("items") {
                Some(i) => check(i, &format!("{at}[]"), depth + 1, out),
                None => out.push(format!("{at}: array without items")),
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool(name: &str, parameters: Value) -> ToolDef {
        ToolDef {
            name: name.into(),
            description: String::new(),
            parameters,
        }
    }

    /// The shape of `files.edit` (ark-files) after ADR 0049: each edit a closed variant.
    fn edit_tool() -> ToolDef {
        tool(
            "files.edit",
            json!({
                "type": "object", "required": ["path", "edits"],
                "properties": {
                    "path": { "type": "string" },
                    "edits": { "type": "array", "items": { "anyOf": [
                        { "type": "object", "required": ["find", "replace"], "properties": { "find": { "type": "string" }, "replace": { "type": "string" } } },
                        { "type": "object", "required": ["cell", "value"], "properties": { "cell": { "type": "string" }, "value": { "type": ["string", "number", "boolean"] }, "sheet": { "type": "string" } } },
                        { "type": "object", "required": ["block", "text"], "properties": { "block": { "type": "integer" }, "text": { "type": "string" } } }
                    ] } },
                    "version": { "type": "string", "description": "the version the last read gave" }
                }
            }),
        )
    }

    fn variant<'a>(shape: &'a ReplyShape, name: &str) -> &'a Value {
        let items = &shape.schema["properties"]["tool_calls"]["items"];
        let vs = items["anyOf"].as_array().cloned().unwrap_or_default();
        if vs.is_empty() {
            return &items["properties"]["arguments"];
        }
        let i = vs
            .iter()
            .position(|v| v["properties"]["name"]["enum"][0] == name)
            .unwrap();
        &items["anyOf"][i]["properties"]["arguments"]
    }

    #[test]
    fn each_tool_is_a_closed_variant_in_strict_form() {
        let tools = vec![
            edit_tool(),
            tool(
                "fs.read",
                json!({"type":"object","required":["path"],"properties":{"path":{"type":"string","minLength":1}}}),
            ),
            tool("ping", json!({"type":"object","properties":{}})),
        ];
        let s = ReplyShape::typed(&tools);
        assert!(s.typed);
        assert_eq!(
            strict_problems(&s.schema),
            Vec::<String>::new(),
            "{}",
            s.schema
        );
        let items = &s.schema["properties"]["tool_calls"]["items"]["anyOf"];
        assert_eq!(items.as_array().unwrap().len(), 3);
        let keys: Vec<&String> = items[0]["properties"].as_object().unwrap().keys().collect();
        assert_eq!(keys, ["name", "arguments"], "the name is written first");
        let read = variant(&s, "fs.read");
        assert_eq!(
            read["properties"]["path"],
            json!({"type":"string"}),
            "constraints are left out"
        );
        let edit = variant(&s, "files.edit");
        assert_eq!(
            edit["properties"]["version"],
            json!({"anyOf":[{"type":"string"},{"type":"null"}]}),
            "an optional key may be null"
        );
        assert_eq!(edit["required"], json!(["path", "edits", "version"]));
        assert_eq!(variant(&s, "ping")["properties"], json!({}));
    }

    #[test]
    fn a_long_edit_with_quotes_newlines_and_code_reads_back_exactly() {
        let s = ReplyShape::typed(&[edit_tool()]);
        // The kind of payload that broke as a string (6,043 characters of JavaScript).
        let code = "const sep = (area.value.endsWith(\"\\n\") ? \" \" : \"\");\n".repeat(80)
            + "console.log('done', {k: [1, \"two\", {x: null}]});\n\t// é ✓ \u{1F600}";
        let reply = json!({
            "path": "src/app.js",
            "edits": [
                { "find": "const a = \"x\";", "replace": code },
                { "cell": "B4", "value": 150, "sheet": null },
                { "block": 3, "text": "**bold**\n- a\n- b" }
            ],
            "version": null
        });
        let got = s.arguments("files.edit", &reply).unwrap();
        assert_eq!(
            got,
            json!({
                "path": "src/app.js",
                "edits": [
                    { "find": "const a = \"x\";", "replace": code },
                    { "cell": "B4", "value": 150 },
                    { "block": 3, "text": "**bold**\n- a\n- b" }
                ]
            }),
            "nulls of optional keys are dropped, everything else is as written"
        );
    }

    #[test]
    fn a_null_the_tool_accepts_is_passed_through() {
        let t = tool(
            "x",
            json!({"type":"object","properties":{"a":{"type":["string","null"]},"b":{"type":"string"}}}),
        );
        let s = ReplyShape::typed(&[t]);
        assert_eq!(
            variant(&s, "x")["properties"]["a"],
            json!({"type":["string","null"]})
        );
        let got = s.arguments("x", &json!({"a": null, "b": null})).unwrap();
        assert_eq!(got, json!({"a": null}));
    }

    #[test]
    fn what_strict_mode_cannot_express_travels_as_a_json_string() {
        let t = tool(
            "mcp.call",
            json!({"type":"object","required":["q","data"],"properties":{
                "q": {"type":"string"},
                "data": {"type":"object"},
                "rows": {"type":"array","items":{"type":"array"}},
                "pick": {"$ref":"#/$defs/x"},
                "pair": {"type":"array","items":[{"type":"string"},{"type":"integer"}]},
                "extra": {"type":"object","properties":{"a":{"type":"string"}},"additionalProperties":true}
            }}),
        );
        let s = ReplyShape::typed(&[t]);
        let args = variant(&s, "mcp.call");
        assert_eq!(
            strict_problems(&s.schema),
            Vec::<String>::new(),
            "{}",
            s.schema
        );
        assert_eq!(args["properties"]["q"], json!({"type":"string"}));
        assert_eq!(args["properties"]["data"]["type"], "string");
        assert_eq!(args["properties"]["rows"]["anyOf"][0]["type"], "array");
        assert_eq!(
            args["properties"]["rows"]["anyOf"][0]["items"]["type"], "string",
            "only the part that cannot be typed is a string"
        );
        let got = s
            .arguments(
                "mcp.call",
                &json!({"q":"a \"b\"","data":"{\"k\":[1,{\"n\":\"x\\ny\"}]}","rows":["[1,\"a\"]"],"pick":null,"pair":null,"extra":"{\"z\":1}"}),
            )
            .unwrap();
        assert_eq!(
            got,
            json!({"q":"a \"b\"","data":{"k":[1,{"n":"x\ny"}]},"rows":[[1,"a"]],"extra":{"z":1}})
        );
    }

    #[test]
    fn a_part_that_is_not_json_is_an_error_and_never_repaired() {
        let t = tool(
            "t",
            json!({"type":"object","required":["data"],"properties":{"data":{"type":"object"}}}),
        );
        let s = ReplyShape::typed(&[t]);
        let e = s
            .arguments("t", &json!({"data": "{\"a\": \"unterminated}"}))
            .unwrap_err();
        assert!(e.contains("`data` is not JSON"), "{e}");
        let e = s.arguments("t", &json!({"data": ""})).unwrap_err();
        assert!(
            e.contains("`data`"),
            "an empty part is not taken as {{}}: {e}"
        );
    }

    #[test]
    fn a_tool_whose_whole_arguments_are_open_keeps_the_string_form() {
        let s = ReplyShape::typed(&[tool("open", json!({"type":"object"}))]);
        assert_eq!(variant(&s, "open")["type"], "string");
        assert_eq!(
            s.arguments("open", &json!("{\"a\":[1,2]}")).unwrap(),
            json!({"a":[1,2]})
        );
        assert_eq!(s.arguments("open", &json!("")).unwrap(), json!({}));
        assert!(s.arguments("open", &json!("{a")).is_err());
    }

    #[test]
    fn variants_are_told_apart_by_their_keys() {
        let t = tool(
            "t",
            json!({"type":"object","required":["e"],"properties":{"e":{"oneOf":[
                {"type":"object","required":["a"],"properties":{"a":{"type":"string"},"s":{"type":"string"}}},
                {"type":"object","required":["b"],"properties":{"b":{"type":"object"}}},
                {"type":"string","const":"none"}
            ]}}}),
        );
        let s = ReplyShape::typed(&[t]);
        assert_eq!(
            strict_problems(&s.schema),
            Vec::<String>::new(),
            "{}",
            s.schema
        );
        let a = s.arguments("t", &json!({"e":{"a":"x","s":null}})).unwrap();
        assert_eq!(a, json!({"e":{"a":"x"}}));
        let b = s.arguments("t", &json!({"e":{"b":"{\"k\":1}"}})).unwrap();
        assert_eq!(b, json!({"e":{"b":{"k":1}}}));
        let c = s.arguments("t", &json!({"e":"none"})).unwrap();
        assert_eq!(c, json!({"e":"none"}));

        // Two branches with the same keys, one needing reading back: the part is a string.
        let t = tool(
            "u",
            json!({"type":"object","required":["e"],"properties":{"e":{"anyOf":[
                {"type":"object","required":["a"],"properties":{"a":{"type":"string"}}},
                {"type":"object","properties":{"a":{"type":"integer"}}}
            ]}}}),
        );
        let s = ReplyShape::typed(&[t]);
        assert_eq!(variant(&s, "u")["properties"]["e"]["type"], "string");
    }

    #[test]
    fn deep_parts_become_strings_within_the_nesting_limit() {
        let mut deep = json!({"type":"string"});
        for _ in 0..12 {
            deep = json!({"type":"object","required":["a"],"properties":{"a": deep}});
        }
        let s = ReplyShape::typed(&[tool("deep", deep)]);
        assert_eq!(
            strict_problems(&s.schema),
            Vec::<String>::new(),
            "{}",
            s.schema
        );
        let mut v = variant(&s, "deep");
        let mut levels = ARGUMENTS_DEPTH;
        while v["type"] == "object" {
            v = &v["properties"]["a"];
            levels += 1;
        }
        assert_eq!(levels, MAX_DEPTH + 1);
        assert_eq!(v["type"], "string");
    }

    #[test]
    fn enums_carry_their_type_and_const_is_an_enum() {
        let t = tool(
            "e",
            json!({"type":"object","required":["k","n","c"],"properties":{
                "k":{"enum":["a","b"]},"n":{"type":"number","enum":[1,2.5]},"c":{"const":3}
            }}),
        );
        let s = ReplyShape::typed(&[t]);
        let p = &variant(&s, "e")["properties"];
        assert_eq!(p["k"], json!({"type":"string","enum":["a","b"]}));
        assert_eq!(p["n"], json!({"type":"number","enum":[1,2.5]}));
        assert_eq!(p["c"], json!({"type":"integer","enum":[3]}));
    }

    #[test]
    fn over_a_limit_the_whole_reply_uses_strings() {
        let big: Map<String, Value> = (0..5100)
            .map(|i| (format!("k{i}"), json!({"type":"string"})))
            .collect();
        let s = ReplyShape::typed(&[tool("big", json!({"type":"object","properties": big}))]);
        assert!(!s.typed);
        assert_eq!(s.schema, ReplyShape::legacy().schema);
        assert!(!ReplyShape::typed(&[]).typed, "no tools: the plain shape");
    }

    #[test]
    fn the_open_and_legacy_shapes_read_objects_and_strings() {
        for s in [ReplyShape::open(), ReplyShape::legacy()] {
            assert_eq!(s.arguments("x", &json!({"a":1})).unwrap(), json!({"a":1}));
            assert_eq!(
                s.arguments("x", &json!("{\"a\":1}")).unwrap(),
                json!({"a":1})
            );
            assert_eq!(s.arguments("x", &json!(" ")).unwrap(), json!({}));
            assert!(s.arguments("x", &json!("{path")).is_err());
        }
        assert_eq!(
            ReplyShape::open().schema["properties"]["tool_calls"]["items"]["properties"]
                ["arguments"],
            json!({"type":"object"})
        );
    }
}
