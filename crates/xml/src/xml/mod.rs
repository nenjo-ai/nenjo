//! XML utilities for prompt rendering.
//!
//! Serialization is handled by `quick-xml` with serde. This module re-exports
//! the key utilities and provides convenience helpers.

pub mod parse;

use serde::Serialize;

/// Serialize a value to a compact XML string.
pub fn to_xml<T: Serialize>(value: &T) -> String {
    quick_xml::se::to_string(value).unwrap_or_default()
}

/// Serialize a value to a pretty-printed XML string with indentation.
pub fn to_xml_pretty<T: Serialize>(value: &T, indent: usize) -> String {
    let mut buf = String::new();
    let ser = {
        let mut s = quick_xml::se::Serializer::new(&mut buf);
        s.indent(' ', indent);
        s
    };
    if value.serialize(ser).is_err() {
        return String::new();
    }
    buf
}

/// Escape XML special characters (`<`, `>`, `&`, `'`, `"`).
pub fn xml_escape(raw: &str) -> String {
    quick_xml::escape::escape(raw).into_owned()
}

/// Unescape XML entities (`&lt;`, `&gt;`, `&amp;`, `&apos;`, `&quot;`).
pub fn xml_unescape(raw: &str) -> String {
    quick_xml::escape::unescape(raw)
        .unwrap_or(std::borrow::Cow::Borrowed(raw))
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_all_entities() {
        assert_eq!(xml_escape("<>&'\""), "&lt;&gt;&amp;&apos;&quot;");
    }

    #[test]
    fn unescape_all_entities() {
        assert_eq!(xml_unescape("&lt;&gt;&amp;&apos;&quot;"), "<>&'\"");
    }

    #[test]
    fn escape_unescape_roundtrip() {
        let input = r#"<tag attr="val" & 'other'>"#;
        assert_eq!(xml_unescape(&xml_escape(input)), input);
    }

    #[test]
    fn escape_plain_text_unchanged() {
        assert_eq!(xml_escape("hello world"), "hello world");
    }

    #[test]
    fn escape_empty_string() {
        assert_eq!(xml_escape(""), "");
        assert_eq!(xml_unescape(""), "");
    }

    #[test]
    fn to_xml_simple_struct() {
        #[derive(Serialize)]
        struct Item {
            name: String,
        }
        let item = Item {
            name: "test".into(),
        };
        let xml = to_xml(&item);
        assert!(xml.contains("<name>test</name>"));
    }

    #[test]
    fn to_xml_pretty_indentation() {
        #[derive(Serialize)]
        struct Item {
            name: String,
        }
        let item = Item {
            name: "test".into(),
        };
        let xml = to_xml_pretty(&item, 2);
        assert!(xml.contains("  <name>test</name>"));
    }
}
