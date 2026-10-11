use std::{collections::BTreeSet, env, fs, path::PathBuf};

use serde_json::Value;

fn main() -> anyhow::Result<()> {
    let root = PathBuf::from(env::args().nth(1).unwrap_or_else(|| ".".into()));
    let schema = serde_json::to_value(schemars::schema_for!(ark_protocol::ProtocolSchema))?;
    let schema_path = root.join("schema/v1/protocol.schema.json");
    let swift_path = root.join("generated/ARKProtocol.swift");
    fs::create_dir_all(schema_path.parent().unwrap())?;
    fs::create_dir_all(swift_path.parent().unwrap())?;
    fs::write(
        &schema_path,
        format!("{}\n", serde_json::to_string_pretty(&schema)?),
    )?;
    fs::write(&swift_path, swift(&schema)?)?;
    Ok(())
}

fn swift(schema: &Value) -> anyhow::Result<String> {
    let defs = schema["$defs"]
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("schema has no definitions"))?;
    let mut out = String::from(
        "// Generated from protocol.schema.json. Do not edit.\n\nimport Foundation\n\n",
    );
    out.push_str(
        "public enum JSONValue: Codable, Equatable, Sendable {\n\
         case null\n    case bool(Bool)\n    case number(Double)\n    case string(String)\n\
         case array([JSONValue])\n    case object([String: JSONValue])\n\n\
         public init(from decoder: Decoder) throws {\n        let value = try decoder.singleValueContainer()\n\
         if value.decodeNil() { self = .null }\n        else if let decoded = try? value.decode(Bool.self) { self = .bool(decoded) }\n\
         else if let decoded = try? value.decode(Double.self) { self = .number(decoded) }\n\
         else if let decoded = try? value.decode(String.self) { self = .string(decoded) }\n\
         else if let decoded = try? value.decode([JSONValue].self) { self = .array(decoded) }\n\
         else { self = .object(try value.decode([String: JSONValue].self)) }\n    }\n\n\
         public func encode(to encoder: Encoder) throws {\n        var value = encoder.singleValueContainer()\n\
         switch self {\n        case .null: try value.encodeNil()\n        case let .bool(v): try value.encode(v)\n\
         case let .number(v): try value.encode(v)\n        case let .string(v): try value.encode(v)\n\
         case let .array(v): try value.encode(v)\n        case let .object(v): try value.encode(v)\n        }\n    }\n}\n\n",
    );
    out.push_str(
        "public struct DecimalString: Codable, Hashable, Sendable, Comparable, ExpressibleByStringLiteral {\n\
         public let rawValue: String\n    public init(_ value: String) throws {\n\
         guard !value.isEmpty, value.allSatisfy({ $0.isASCII && $0.isNumber }) else { throw DecimalStringError.invalid(value) }\n\
         let normalized = value.drop(while: { $0 == \"0\" })\n        rawValue = normalized.isEmpty ? \"0\" : String(normalized)\n    }\n\
         public init(stringLiteral value: String) { self = try! DecimalString(value) }\n\
         public init(from decoder: Decoder) throws { try self.init(decoder.singleValueContainer().decode(String.self)) }\n\
         public func encode(to encoder: Encoder) throws { var value = encoder.singleValueContainer(); try value.encode(rawValue) }\n\
         public static func < (lhs: Self, rhs: Self) -> Bool { lhs.rawValue.count == rhs.rawValue.count ? lhs.rawValue < rhs.rawValue : lhs.rawValue.count < rhs.rawValue.count }\n}\n\
         public enum DecimalStringError: Error, Equatable { case invalid(String) }\n\n",
    );

    let mut names: Vec<_> = defs
        .keys()
        .filter(|name| *name != "DecimalString")
        .collect();
    names.sort();
    for name in names {
        let definition = &defs[name];
        let Some(properties) = definition["properties"].as_object() else {
            continue;
        };
        let required: BTreeSet<&str> = definition["required"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        out.push_str(&format!(
            "public struct {name}: Codable, Equatable, Sendable {{\n"
        ));
        let mut fields: Vec<_> = properties.iter().collect();
        fields.sort_by_key(|(field, _)| *field);
        for (field, value) in &fields {
            let mut ty = swift_type(value)?;
            if !required.contains(field.as_str()) {
                ty.push('?');
            }
            out.push_str(&format!("    public let {field}: {ty}\n"));
        }
        out.push_str("\n    public init(\n");
        for (index, (field, value)) in fields.iter().enumerate() {
            let mut ty = swift_type(value)?;
            let default = if required.contains(field.as_str()) {
                ""
            } else {
                ty.push('?');
                " = nil"
            };
            let comma = if index + 1 == fields.len() { "" } else { "," };
            out.push_str(&format!("        {field}: {ty}{default}{comma}\n"));
        }
        out.push_str("    ) {\n");
        for (field, _) in &fields {
            out.push_str(&format!("        self.{field} = {field}\n"));
        }
        out.push_str("    }\n}\n\n");
    }
    while out.ends_with('\n') {
        out.pop();
    }
    out.push('\n');
    Ok(out)
}

fn swift_type(schema: &Value) -> anyhow::Result<String> {
    if let Some(reference) = schema["$ref"].as_str() {
        return Ok(reference
            .rsplit('/')
            .next()
            .unwrap_or("JSONValue")
            .to_string());
    }
    if let Some(any) = schema["anyOf"].as_array() {
        if let Some(value) = any.iter().find(|value| value["type"] != "null") {
            return swift_type(value);
        }
    }
    if let Some(types) = schema["type"].as_array() {
        if let Some(value) = types
            .iter()
            .filter_map(Value::as_str)
            .find(|value| *value != "null")
        {
            return swift_type(&serde_json::json!({ "type": value }));
        }
    }
    match schema["type"].as_str() {
        Some("string") => Ok("String".into()),
        Some("integer") => Ok("Int".into()),
        Some("number") => Ok("Double".into()),
        Some("boolean") => Ok("Bool".into()),
        Some("array") => Ok(format!("[{}]", swift_type(&schema["items"])?)),
        Some("object") => {
            if let Some(values) = schema.get("additionalProperties") {
                Ok(format!("[String: {}]", swift_type(values)?))
            } else {
                Ok("JSONValue".into())
            }
        }
        _ => Ok("JSONValue".into()),
    }
}
