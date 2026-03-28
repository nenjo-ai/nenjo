//! Generic rendering helpers.

use serde::Serialize;

use crate::xml::to_xml_pretty;

/// Render a slice of `Serialize` items as pretty-printed, indented XML lines.
/// Filters out empty items. Returns empty string if no items render.
pub fn render_items<T: Serialize>(items: &[T]) -> String {
    let rendered: Vec<String> = items
        .iter()
        .map(|item| to_xml_pretty(item, 2))
        .filter(|xml| !xml.is_empty())
        .collect();
    if rendered.is_empty() {
        String::new()
    } else {
        rendered
            .iter()
            .flat_map(|xml| xml.lines().map(|l| format!("  {l}")))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// Convert a project `settings.metadata` JSON object to XML entries.
///
/// Input: `{"language": "rust", "team": "backend"}`
/// Output: `<metadata>\n  <entry key="language">rust</entry>\n  <entry key="team">backend</entry>\n</metadata>`
pub fn metadata_json_to_xml(settings: &serde_json::Value) -> String {
    let metadata = match settings.get("metadata") {
        Some(m) if m.is_object() => m,
        _ => return String::new(),
    };

    let entries: Vec<String> = metadata
        .as_object()
        .unwrap()
        .iter()
        .filter(|(_, v)| !v.is_null())
        .map(|(k, v)| {
            let val = match v {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            format!("  <entry key=\"{k}\">{val}</entry>")
        })
        .collect();

    if entries.is_empty() {
        return String::new();
    }

    format!("<metadata>\n{}\n</metadata>", entries.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_items_empty_slice() {
        let items: Vec<serde_json::Value> = vec![];
        assert_eq!(render_items(&items), "");
    }

    #[test]
    fn render_items_with_values() {
        #[derive(Serialize)]
        struct Item {
            name: String,
        }
        let items = vec![Item { name: "a".into() }, Item { name: "b".into() }];
        let result = render_items(&items);
        assert!(result.contains("a"));
        assert!(result.contains("b"));
    }

    #[test]
    fn metadata_json_to_xml_basic() {
        let settings = serde_json::json!({
            "metadata": {
                "language": "rust",
                "team": "backend"
            }
        });
        let result = metadata_json_to_xml(&settings);
        assert!(result.starts_with("<metadata>"));
        assert!(result.contains(r#"<entry key="language">rust</entry>"#));
        assert!(result.contains(r#"<entry key="team">backend</entry>"#));
        assert!(result.ends_with("</metadata>"));
    }

    #[test]
    fn metadata_json_to_xml_no_metadata() {
        assert_eq!(metadata_json_to_xml(&serde_json::json!({})), "");
        assert_eq!(metadata_json_to_xml(&serde_json::Value::Null), "");
    }

    #[test]
    fn metadata_json_to_xml_null_values_filtered() {
        let settings = serde_json::json!({
            "metadata": { "a": "keep", "b": null }
        });
        let result = metadata_json_to_xml(&settings);
        assert!(result.contains("keep"));
        assert!(!result.contains("\"b\""));
    }
}
