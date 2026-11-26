//! The field list of docs/ui-contract.md section 4.2 (the `contract` code block), and a
//! check that a command's JSON has every listed field. Used by `cli.rs` and `judge.rs`.

use serde_json::Value;

/// One line of the block: `<command> <path>: <field> <field> ...`.
pub struct Line {
    pub command: String,
    pub path: String,
    /// `(name, optional)`; optional fields end with `?` in the block.
    pub fields: Vec<(String, bool)>,
}

/// The `ui_contract = N` value and the lines of the block. `None` in a checkout without the
/// design documents (the public snapshot has no `docs/`): there is no contract to check.
pub fn load() -> Option<(u64, Vec<Line>)> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("docs")
        .join("ui-contract.md");
    let doc = std::fs::read_to_string(&path).ok()?;
    let doc = doc.replace("\r\n", "\n");
    let start = doc.find("```contract\n").expect("contract block") + "```contract\n".len();
    let end = start + doc[start..].find("```").expect("end of contract block");
    let mut version = None;
    let mut lines = Vec::new();
    for raw in doc[start..end].lines().filter(|l| !l.trim().is_empty()) {
        if let Some(v) = raw.strip_prefix("ui_contract = ") {
            version = Some(v.trim().parse().expect("ui_contract is an integer"));
            continue;
        }
        let (head, fields) = raw.split_once(':').expect("<command> <path>: <fields>");
        let (command, path) = head.split_once(' ').expect("<command> <path>");
        let fields = fields
            .split_whitespace()
            .map(|f| match f.strip_suffix('?') {
                Some(name) => (name.to_string(), true),
                None => (f.to_string(), false),
            })
            .collect();
        lines.push(Line {
            command: command.to_string(),
            path: path.trim().to_string(),
            fields,
        });
    }
    Some((version.expect("ui_contract = N in the block"), lines))
}

/// The values at `path` (section 4.2): `.` is the top; `name` an object; `name[]` and a
/// leading `[]` every element of an array. A missing name skips that value.
fn at<'a>(top: &'a Value, path: &str) -> Vec<&'a Value> {
    let mut values = vec![top];
    if path == "." {
        return values;
    }
    for seg in path.split('.') {
        let (name, each) = match seg.strip_suffix("[]") {
            Some(name) => (name, true),
            None => (seg, false),
        };
        let mut next = Vec::new();
        for v in values {
            let v = if name.is_empty() {
                Some(v)
            } else {
                v.get(name)
            };
            let Some(v) = v.filter(|v| !v.is_null()) else {
                continue;
            };
            if each {
                next.extend(v.as_array().expect("an array").iter());
            } else {
                next.push(v);
            }
        }
        values = next;
    }
    values
}

/// Panics unless the values at the line's path exist and carry every listed field (an
/// optional field: at least one of them).
pub fn check(line: &Line, json: &Value) {
    let values = at(json, &line.path);
    assert!(
        !values.is_empty(),
        "{} {}: no value at this path in {json}",
        line.command,
        line.path
    );
    for (field, optional) in &line.fields {
        let with = values.iter().filter(|v| v.get(field).is_some()).count();
        if *optional {
            assert!(
                with > 0,
                "{} {}: no element has {field}",
                line.command,
                line.path
            );
        } else {
            assert_eq!(
                with,
                values.len(),
                "{} {}: {field} is missing in {json}",
                line.command,
                line.path
            );
        }
    }
}
