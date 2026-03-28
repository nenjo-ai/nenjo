//! Simple XML parsing utilities for extracting content and attributes.
//!
//! These functions operate on XML strings and provide basic extraction
//! capabilities without requiring a full XML parser. They are designed for
//! the structured XML used in LLM prompts, which tends to be well-formed
//! and predictable.
//!
//! For complex XML parsing needs, consider using a full parser like `quick-xml`.

use super::xml_unescape;

/// Error type for XML parsing operations.
#[derive(Debug, thiserror::Error)]
pub enum XmlError {
    #[error("tag not found: <{0}>")]
    TagNotFound(String),
    #[error("attribute '{attr}' not found on <{tag}>")]
    AttrNotFound { tag: String, attr: String },
    #[error("parse error: {0}")]
    Parse(String),
}

pub type Result<T> = std::result::Result<T, XmlError>;

/// Extracts the text content of the first occurrence of a tag.
///
/// Returns the unescaped text between `<name>` and `</name>`.
///
/// # Examples
///
/// ```
/// use prompt_render::parse::extract_tag_content;
///
/// let xml = "<task><title>Fix bug</title><priority>high</priority></task>";
/// assert_eq!(extract_tag_content(xml, "title").unwrap(), "Fix bug");
/// assert_eq!(extract_tag_content(xml, "priority").unwrap(), "high");
/// ```
///
/// Handles XML entities:
///
/// ```
/// use prompt_render::parse::extract_tag_content;
///
/// let xml = "<msg>a &lt; b &amp; c</msg>";
/// assert_eq!(extract_tag_content(xml, "msg").unwrap(), "a < b & c");
/// ```
pub fn extract_tag_content(xml: &str, tag_name: &str) -> Result<String> {
    let open_tag_prefix = format!("<{}", tag_name);
    let close_tag = format!("</{}>", tag_name);

    // Find the opening tag (which may have attributes)
    let open_start = xml
        .find(&open_tag_prefix)
        .ok_or_else(|| XmlError::TagNotFound(tag_name.to_string()))?;

    // Find the end of the opening tag (the '>' character)
    let content_start = xml[open_start..]
        .find('>')
        .ok_or_else(|| XmlError::Parse(format!("malformed opening tag <{}>", tag_name)))?
        + open_start
        + 1;

    // Check for self-closing tag
    if xml[open_start..content_start].ends_with("/>") {
        return Ok(String::new());
    }

    // Find the closing tag
    let content_end = xml[content_start..]
        .find(&close_tag)
        .ok_or_else(|| XmlError::TagNotFound(tag_name.to_string()))?
        + content_start;

    let raw_content = &xml[content_start..content_end];
    Ok(xml_unescape(raw_content))
}

/// Extracts all occurrences of a tag's text content.
///
/// Returns a vector of unescaped strings, one per occurrence.
///
/// # Examples
///
/// ```
/// use prompt_render::parse::extract_all_tag_contents;
///
/// let xml = "<list><item>A</item><item>B</item><item>C</item></list>";
/// let items = extract_all_tag_contents(xml, "item").unwrap();
/// assert_eq!(items, vec!["A", "B", "C"]);
/// ```
pub fn extract_all_tag_contents(xml: &str, tag_name: &str) -> Result<Vec<String>> {
    let mut results = Vec::new();
    let open_tag_prefix = format!("<{}", tag_name);
    let close_tag = format!("</{}>", tag_name);
    let mut search_from = 0;

    while let Some(pos) = xml[search_from..].find(&open_tag_prefix) {
        let open_start = search_from + pos;

        // Ensure it's actually a tag boundary (next char is whitespace, '>', or '/')
        let after_prefix = open_start + open_tag_prefix.len();
        if after_prefix < xml.len() {
            let next_char = xml.as_bytes()[after_prefix];
            if next_char != b'>'
                && next_char != b' '
                && next_char != b'/'
                && next_char != b'\t'
                && next_char != b'\n'
            {
                search_from = after_prefix;
                continue;
            }
        }

        // Find end of opening tag
        let content_start = match xml[open_start..].find('>') {
            Some(pos) => open_start + pos + 1,
            None => break,
        };

        // Self-closing tag
        if xml[open_start..content_start].ends_with("/>") {
            results.push(String::new());
            search_from = content_start;
            continue;
        }

        // Find closing tag
        let content_end = match xml[content_start..].find(&close_tag) {
            Some(pos) => content_start + pos,
            None => break,
        };

        results.push(xml_unescape(&xml[content_start..content_end]));
        search_from = content_end + close_tag.len();
    }

    if results.is_empty() {
        return Err(XmlError::TagNotFound(tag_name.to_string()));
    }

    Ok(results)
}

/// Extracts the value of an attribute from the first occurrence of a tag.
///
/// Returns the unescaped attribute value.
///
/// # Examples
///
/// ```
/// use prompt_render::parse::extract_attr;
///
/// let xml = r#"<task id="42" status="open">Fix bug</task>"#;
/// assert_eq!(extract_attr(xml, "task", "id").unwrap(), "42");
/// assert_eq!(extract_attr(xml, "task", "status").unwrap(), "open");
/// ```
pub fn extract_attr(xml: &str, tag_name: &str, attr_name: &str) -> Result<String> {
    let open_tag_prefix = format!("<{}", tag_name);

    let open_start = xml
        .find(&open_tag_prefix)
        .ok_or_else(|| XmlError::TagNotFound(tag_name.to_string()))?;

    // Find the end of the opening tag
    let tag_end = xml[open_start..]
        .find('>')
        .ok_or_else(|| XmlError::Parse(format!("malformed opening tag <{}>", tag_name)))?
        + open_start;

    let tag_str = &xml[open_start..=tag_end];

    // Look for the attribute
    let attr_pattern = format!("{}=\"", attr_name);
    let attr_start = tag_str
        .find(&attr_pattern)
        .ok_or_else(|| XmlError::AttrNotFound {
            tag: tag_name.to_string(),
            attr: attr_name.to_string(),
        })?;

    let value_start = attr_start + attr_pattern.len();
    let value_end = tag_str[value_start..]
        .find('"')
        .ok_or_else(|| XmlError::Parse(format!("unclosed attribute value for '{}'", attr_name)))?
        + value_start;

    let raw_value = &tag_str[value_start..value_end];
    Ok(xml_unescape(raw_value))
}

/// Checks if a specific tag exists in the XML string.
///
/// # Examples
///
/// ```
/// use prompt_render::parse::has_tag;
///
/// let xml = "<root><name>Alice</name></root>";
/// assert!(has_tag(xml, "name"));
/// assert!(!has_tag(xml, "age"));
/// ```
pub fn has_tag(xml: &str, tag_name: &str) -> bool {
    let open_tag_prefix = format!("<{}", tag_name);
    if let Some(pos) = xml.find(&open_tag_prefix) {
        let after = pos + open_tag_prefix.len();
        if after < xml.len() {
            let ch = xml.as_bytes()[after];
            return ch == b'>' || ch == b' ' || ch == b'/' || ch == b'\t' || ch == b'\n';
        }
    }
    false
}

/// Extracts the raw (unprocessed) inner XML of a tag, including nested tags.
///
/// Unlike [`extract_tag_content`], this does NOT unescape entities. Useful
/// when the inner content is itself XML that you want to process further.
///
/// # Examples
///
/// ```
/// use prompt_render::parse::extract_raw_inner_xml;
///
/// let xml = "<root><child>text</child></root>";
/// assert_eq!(extract_raw_inner_xml(xml, "root").unwrap(), "<child>text</child>");
/// ```
pub fn extract_raw_inner_xml(xml: &str, tag_name: &str) -> Result<String> {
    let open_tag_prefix = format!("<{}", tag_name);
    let close_tag = format!("</{}>", tag_name);

    let open_start = xml
        .find(&open_tag_prefix)
        .ok_or_else(|| XmlError::TagNotFound(tag_name.to_string()))?;

    let content_start = xml[open_start..]
        .find('>')
        .ok_or_else(|| XmlError::Parse(format!("malformed opening tag <{}>", tag_name)))?
        + open_start
        + 1;

    if xml[open_start..content_start].ends_with("/>") {
        return Ok(String::new());
    }

    // For nested same-name tags, we need to find the matching close tag
    let mut depth = 1;
    let mut search_pos = content_start;

    while depth > 0 {
        // Find next open or close of same tag name
        let next_open = xml[search_pos..].find(&open_tag_prefix);
        let next_close = xml[search_pos..].find(&close_tag);

        match (next_open, next_close) {
            (_, None) => {
                return Err(XmlError::Parse(format!(
                    "missing closing tag </{}>",
                    tag_name
                )));
            }
            (Some(o), Some(c)) if o < c => {
                // Check it's actually a tag boundary
                let after = search_pos + o + open_tag_prefix.len();
                if after < xml.len() {
                    let ch = xml.as_bytes()[after];
                    if ch == b'>' || ch == b' ' || ch == b'/' || ch == b'\t' || ch == b'\n' {
                        depth += 1;
                    }
                }
                search_pos += o + 1;
            }
            (_, Some(c)) => {
                depth -= 1;
                if depth == 0 {
                    let content_end = search_pos + c;
                    return Ok(xml[content_start..content_end].to_string());
                }
                search_pos += c + close_tag.len();
            }
        }
    }

    Err(XmlError::Parse(format!(
        "unexpected end parsing <{}>",
        tag_name
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- extract_tag_content ----

    #[test]
    fn test_extract_simple() {
        let xml = "<name>Alice</name>";
        assert_eq!(extract_tag_content(xml, "name").unwrap(), "Alice");
    }

    #[test]
    fn test_extract_with_surrounding() {
        let xml = "<root><name>Bob</name><age>30</age></root>";
        assert_eq!(extract_tag_content(xml, "name").unwrap(), "Bob");
        assert_eq!(extract_tag_content(xml, "age").unwrap(), "30");
    }

    #[test]
    fn test_extract_with_attrs() {
        let xml = r#"<task id="1">content</task>"#;
        assert_eq!(extract_tag_content(xml, "task").unwrap(), "content");
    }

    #[test]
    fn test_extract_with_entities() {
        let xml = "<msg>a &lt; b &amp; c &gt; d</msg>";
        assert_eq!(extract_tag_content(xml, "msg").unwrap(), "a < b & c > d");
    }

    #[test]
    fn test_extract_nested_xml_content() {
        let xml = "<root><inner>text</inner></root>";
        // This extracts everything between <root> and </root>, including inner tags
        let content = extract_tag_content(xml, "root").unwrap();
        assert_eq!(content, "<inner>text</inner>");
    }

    #[test]
    fn test_extract_self_closing() {
        let xml = "<item />";
        assert_eq!(extract_tag_content(xml, "item").unwrap(), "");
    }

    #[test]
    fn test_extract_not_found() {
        let xml = "<name>Alice</name>";
        assert!(extract_tag_content(xml, "age").is_err());
    }

    #[test]
    fn test_extract_empty_content() {
        let xml = "<name></name>";
        assert_eq!(extract_tag_content(xml, "name").unwrap(), "");
    }

    // ---- extract_all_tag_contents ----

    #[test]
    fn test_extract_all() {
        let xml = "<list><item>A</item><item>B</item><item>C</item></list>";
        let items = extract_all_tag_contents(xml, "item").unwrap();
        assert_eq!(items, vec!["A", "B", "C"]);
    }

    #[test]
    fn test_extract_all_single() {
        let xml = "<list><item>A</item></list>";
        let items = extract_all_tag_contents(xml, "item").unwrap();
        assert_eq!(items, vec!["A"]);
    }

    #[test]
    fn test_extract_all_not_found() {
        let xml = "<list></list>";
        assert!(extract_all_tag_contents(xml, "item").is_err());
    }

    #[test]
    fn test_extract_all_with_entities() {
        let xml = "<list><v>a &amp; b</v><v>c &lt; d</v></list>";
        let vals = extract_all_tag_contents(xml, "v").unwrap();
        assert_eq!(vals, vec!["a & b", "c < d"]);
    }

    #[test]
    fn test_extract_all_no_false_prefix_match() {
        let xml = "<items><item>A</item><item_extra>B</item_extra></items>";
        let items = extract_all_tag_contents(xml, "item").unwrap();
        assert_eq!(items, vec!["A"]);
    }

    // ---- extract_attr ----

    #[test]
    fn test_extract_attr_simple() {
        let xml = r#"<task id="42">content</task>"#;
        assert_eq!(extract_attr(xml, "task", "id").unwrap(), "42");
    }

    #[test]
    fn test_extract_attr_multiple() {
        let xml = r#"<task id="42" status="open" priority="high">c</task>"#;
        assert_eq!(extract_attr(xml, "task", "id").unwrap(), "42");
        assert_eq!(extract_attr(xml, "task", "status").unwrap(), "open");
        assert_eq!(extract_attr(xml, "task", "priority").unwrap(), "high");
    }

    #[test]
    fn test_extract_attr_with_entities() {
        let xml = r#"<item label="a &amp; b">c</item>"#;
        assert_eq!(extract_attr(xml, "item", "label").unwrap(), "a & b");
    }

    #[test]
    fn test_extract_attr_tag_not_found() {
        let xml = "<name>Alice</name>";
        assert!(extract_attr(xml, "task", "id").is_err());
    }

    #[test]
    fn test_extract_attr_attr_not_found() {
        let xml = r#"<task id="42">c</task>"#;
        assert!(extract_attr(xml, "task", "status").is_err());
    }

    #[test]
    fn test_extract_attr_self_closing() {
        let xml = r#"<br class="clear" />"#;
        assert_eq!(extract_attr(xml, "br", "class").unwrap(), "clear");
    }

    // ---- has_tag ----

    #[test]
    fn test_has_tag_true() {
        assert!(has_tag("<root><name>A</name></root>", "name"));
    }

    #[test]
    fn test_has_tag_false() {
        assert!(!has_tag("<root><name>A</name></root>", "age"));
    }

    #[test]
    fn test_has_tag_no_prefix_match() {
        assert!(!has_tag(
            "<items><item_extra>A</item_extra></items>",
            "item"
        ));
    }

    #[test]
    fn test_has_tag_self_closing() {
        assert!(has_tag("<root><br /></root>", "br"));
    }

    // ---- extract_raw_inner_xml ----

    #[test]
    fn test_raw_inner_simple() {
        let xml = "<root><child>text</child></root>";
        assert_eq!(
            extract_raw_inner_xml(xml, "root").unwrap(),
            "<child>text</child>"
        );
    }

    #[test]
    fn test_raw_inner_nested_same_name() {
        let xml = "<div><div>inner</div></div>";
        assert_eq!(
            extract_raw_inner_xml(xml, "div").unwrap(),
            "<div>inner</div>"
        );
    }

    #[test]
    fn test_raw_inner_preserves_entities() {
        let xml = "<root>a &lt; b</root>";
        assert_eq!(extract_raw_inner_xml(xml, "root").unwrap(), "a &lt; b");
    }

    #[test]
    fn test_raw_inner_self_closing() {
        let xml = "<root />";
        assert_eq!(extract_raw_inner_xml(xml, "root").unwrap(), "");
    }
}
