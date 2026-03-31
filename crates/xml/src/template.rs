//! Generic template rendering using MiniJinja.
//!
//! Supports Jinja2 syntax: `{{ variable }}`, `{% if %}`, `{% for %}`, filters.
//! Template variables are provided as a flat `HashMap<String, String>` with
//! dotted keys (e.g. `"task.id"`, `"agent.name"`). The tree builder groups
//! these into nested MiniJinja access: `{{ task.id }}`, `{{ agent.name }}`.
//!
//! When a node has its own value AND children, the node's value is used for
//! direct rendering (`{{ agent }}`), while children are still accessible
//! (`{{ agent.id }}`). This enables the pattern where singular keys render
//! full XML and dotted keys render individual fields.

use std::collections::{BTreeMap, HashMap};

use minijinja::{Environment, Value};
use tracing::warn;

/// Error type for template rendering failures.
#[derive(Debug, thiserror::Error)]
#[error("template render failed: {message}")]
pub struct TemplateError {
    pub message: String,
    pub detail: String,
}

/// Render a template with the provided variables.
///
/// Variables are a flat `HashMap<String, String>` with dotted keys that get
/// built into a nested tree for Jinja2 access. Falls back to returning the
/// original template if rendering fails.
///
/// # Examples
///
/// ```
/// use std::collections::HashMap;
/// use nenjo_xml::template::render_template;
///
/// let mut vars = HashMap::new();
/// vars.insert("agent.name".into(), "coder".into());
/// vars.insert("task.title".into(), "Fix bug".into());
///
/// let result = render_template("{{ agent.name }}: {{ task.title }}", &vars);
/// assert_eq!(result, "coder: Fix bug");
/// ```
pub fn render_template(template: &str, vars: &HashMap<String, String>) -> String {
    match try_render_template(template, vars) {
        Ok(rendered) => rendered,
        Err(e) => {
            warn!(
                error = %e.message,
                detail = %e.detail,
                "MiniJinja render failed, returning original template"
            );
            template.to_string()
        }
    }
}

/// Like [`render_template`] but returns an error on failure instead of
/// falling back to the original template.
pub fn try_render_template(
    template: &str,
    vars: &HashMap<String, String>,
) -> Result<String, TemplateError> {
    if template.is_empty() {
        return Ok(String::new());
    }

    let template = &escape_backslash_braces(template);
    let context_value = vars_to_value(vars);

    let mut env = Environment::new();
    env.set_auto_escape_callback(|_| minijinja::AutoEscape::None);
    env.set_undefined_behavior(minijinja::UndefinedBehavior::Chainable);

    env.render_str(template, &context_value)
        .map_err(|e| TemplateError {
            message: e.to_string(),
            detail: e.display_debug_info().to_string(),
        })
}

// ---------------------------------------------------------------------------
// Backslash escape pre-processing
// ---------------------------------------------------------------------------

/// Convert `\{{` and `\{%` sequences into MiniJinja raw blocks so that
/// backslash works as an escape character for template delimiters.
///
/// - `\{{`  → literal `{{` (escape prevents variable interpolation)
/// - `\{%`  → literal `{%` (escape prevents block tag interpretation)
/// - `\\{{` → literal `\` + variable interpolation (escaped backslash)
/// - `\\{%` → literal `\` + block tag interpretation (escaped backslash)
fn escape_backslash_braces(template: &str) -> String {
    let mut result = String::with_capacity(template.len());
    let mut chars = template.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '\\' {
            match (chars.peek().copied(), chars.clone().nth(1)) {
                // \\  → consume both, emit one literal backslash.
                (Some('\\'), _) => {
                    chars.next();
                    result.push('\\');
                }
                // \{{ → emit a raw block that outputs literal {{
                (Some('{'), Some('{')) => {
                    chars.next();
                    chars.next();
                    result.push_str("{% raw %}{{{% endraw %}");
                }
                // \{% → emit a raw block that outputs literal {%
                (Some('{'), Some('%')) => {
                    chars.next();
                    chars.next();
                    result.push_str("{% raw %}{%{% endraw %}");
                }
                // Lone backslash — pass through.
                _ => result.push('\\'),
            }
        } else {
            result.push(c);
        }
    }

    result
}

// ---------------------------------------------------------------------------
// Tree builder: HashMap<String, String> → nested MiniJinja Value
// ---------------------------------------------------------------------------

/// Build a nested MiniJinja `Value` tree from a flat `HashMap<String, String>`
/// with dotted keys.
///
/// When a node has its own leaf value AND children, the leaf value is used
/// for direct rendering (e.g. `{{ agent }}`), while children remain accessible
/// (e.g. `{{ agent.id }}`). Nodes without their own value render as the
/// concatenation of all descendant leaf values.
///
/// # Examples
///
/// ```text
/// Input:
///   "agent"      → "<agent id='1' name='coder'/>"
///   "agent.id"   → "1"
///   "agent.name" → "coder"
///   "agents"     → "<agent .../><agent .../>"
///
/// Template access:
///   {{ agent }}       → "<agent id='1' name='coder'/>"
///   {{ agent.id }}    → "1"
///   {{ agent.name }}  → "coder"
///   {{ agents }}      → "<agent .../><agent .../>"
/// ```
pub fn vars_to_value(vars: &HashMap<String, String>) -> Value {
    if vars.is_empty() {
        return Value::from(BTreeMap::<String, Value>::new());
    }

    let mut root = TreeNode::default();
    for (key, value) in vars {
        let parts: Vec<&str> = key.split('.').collect();
        root.insert(&parts, value);
    }

    if root.children.is_empty() {
        Value::from(BTreeMap::<String, Value>::new())
    } else {
        root.to_value()
    }
}

#[derive(Default)]
struct TreeNode {
    children: BTreeMap<String, TreeNode>,
    leaf_value: Option<String>,
}

impl TreeNode {
    fn insert(&mut self, parts: &[&str], value: &str) {
        if parts.is_empty() {
            return;
        }
        if parts.len() == 1 {
            let child = self.children.entry(parts[0].to_string()).or_default();
            child.leaf_value = Some(value.to_string());
        } else {
            let child = self.children.entry(parts[0].to_string()).or_default();
            child.insert(&parts[1..], value);
        }
    }

    /// Collect all leaf values from descendants only (NOT this node's own value).
    fn collect_descendant_leaves(&self) -> Vec<String> {
        let mut leaves = Vec::new();
        for child in self.children.values() {
            if let Some(ref v) = child.leaf_value {
                leaves.push(v.clone());
            }
            leaves.extend(child.collect_descendant_leaves());
        }
        leaves
    }

    fn to_value(&self) -> Value {
        if self.children.is_empty() {
            return Value::from(self.leaf_value.as_deref().unwrap_or(""));
        }

        let mut map = BTreeMap::new();
        for (key, child) in &self.children {
            map.insert(key.clone(), child.to_value());
        }

        // When this node has its own leaf value, use it for rendering.
        // Otherwise, concatenate all descendant leaves.
        let group_value = if let Some(ref v) = self.leaf_value {
            v.clone()
        } else {
            self.collect_descendant_leaves().join("\n\n")
        };

        Value::from_object(GroupNode {
            group_value,
            children: map,
        })
    }
}

#[derive(Debug)]
struct GroupNode {
    group_value: String,
    children: BTreeMap<String, Value>,
}

impl std::fmt::Display for GroupNode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.group_value)
    }
}

impl minijinja::value::Object for GroupNode {
    fn get_value(self: &std::sync::Arc<Self>, key: &Value) -> Option<Value> {
        let key_str = key.as_str()?;
        self.children.get(key_str).cloned()
    }

    fn enumerate(self: &std::sync::Arc<Self>) -> minijinja::value::Enumerator {
        minijinja::value::Enumerator::Iter(Box::new(
            self.children
                .keys()
                .cloned()
                .map(Value::from)
                .collect::<Vec<_>>()
                .into_iter(),
        ))
    }

    fn render(
        self: &std::sync::Arc<Self>,
        f: &mut std::fmt::Formatter<'_>,
    ) -> Result<(), std::fmt::Error> {
        f.write_str(&self.group_value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn render_basic_variables() {
        let v = vars(&[("agent.name", "dev"), ("project.name", "MyProject")]);
        let result = render_template("Agent {{ agent.name }} in {{ project.name }}", &v);
        assert_eq!(result, "Agent dev in MyProject");
    }

    #[test]
    fn render_dotted_keys_as_tree() {
        let v = vars(&[
            ("task.id", "TASK-42"),
            ("task.title", "Fix bug"),
            ("agent.name", "coder"),
        ]);
        assert_eq!(render_template("{{ task.id }}", &v), "TASK-42");
        assert_eq!(render_template("{{ task.title }}", &v), "Fix bug");
        assert_eq!(render_template("{{ agent.name }}", &v), "coder");
    }

    #[test]
    fn render_singular_with_own_value_and_children() {
        let v = vars(&[
            ("agent", "<agent id=\"1\" name=\"coder\"/>"),
            ("agent.id", "1"),
            ("agent.name", "coder"),
        ]);
        // {{ agent }} renders the full XML (own value), not concatenation of children
        assert_eq!(
            render_template("{{ agent }}", &v),
            "<agent id=\"1\" name=\"coder\"/>"
        );
        // {{ agent.id }} still renders the field
        assert_eq!(render_template("{{ agent.id }}", &v), "1");
        assert_eq!(render_template("{{ agent.name }}", &v), "coder");
    }

    #[test]
    fn render_group_without_own_value_concatenates() {
        let v = vars(&[
            ("custom.coding.standards", "Use snake_case."),
            ("custom.coding.guidelines", "Write tests."),
        ]);
        let result = render_template("{{ custom.coding }}", &v);
        assert!(result.contains("Use snake_case."));
        assert!(result.contains("Write tests."));
    }

    #[test]
    fn render_plural_collections() {
        let v = vars(&[("agents", "<agent name=\"a\"/><agent name=\"b\"/>")]);
        assert_eq!(
            render_template("{{ agents }}", &v),
            "<agent name=\"a\"/><agent name=\"b\"/>"
        );
    }

    #[test]
    fn render_conditional() {
        let v = vars(&[("task.id", "TASK-001"), ("task.title", "Fix bug")]);
        let template = "{% if task.id %}Task: {{ task.title }}{% endif %}";
        assert_eq!(render_template(template, &v), "Task: Fix bug");
    }

    #[test]
    fn render_conditional_empty() {
        let v = HashMap::new();
        let template = "{% if task.id %}Task: {{ task.title }}{% else %}No task{% endif %}";
        assert_eq!(render_template(template, &v), "No task");
    }

    #[test]
    fn render_preserves_raw_xml() {
        let v = vars(&[("agents", "<agent name=\"dev\" />")]);
        assert_eq!(
            render_template("{{ agents }}", &v),
            "<agent name=\"dev\" />"
        );
    }

    #[test]
    fn render_empty_template() {
        assert_eq!(render_template("", &HashMap::new()), "");
    }

    #[test]
    fn render_no_variables() {
        assert_eq!(
            render_template("Just plain text.", &HashMap::new()),
            "Just plain text."
        );
    }

    #[test]
    fn render_undefined_variable_empty() {
        let result = render_template("Value: {{ something.unknown }}", &HashMap::new());
        assert_eq!(result, "Value: ");
    }

    #[test]
    fn render_mixed_jinja_and_vars() {
        let v = vars(&[
            ("project.name", "Test"),
            ("task.title", "Fix bug"),
            ("context.nenjo.current_task", "<task>data</task>"),
        ]);
        let template = r#"{{ context.nenjo.current_task }}
{% if project.name == "Test" %}
Extra context for test project
{% endif %}
Task: {{ task.title }}"#;

        let result = render_template(template, &v);
        assert!(result.contains("<task>data</task>"));
        assert!(result.contains("Extra context for test project"));
        assert!(result.contains("Task: Fix bug"));
    }

    #[test]
    fn try_render_success() {
        let v = vars(&[("agent.name", "dev")]);
        assert_eq!(try_render_template("{{ agent.name }}", &v).unwrap(), "dev");
    }

    #[test]
    fn try_render_error() {
        let result = try_render_template("{% invalid syntax %}", &HashMap::new());
        assert!(result.is_err());
        assert!(!result.unwrap_err().message.is_empty());
    }

    #[test]
    fn try_render_empty() {
        assert_eq!(try_render_template("", &HashMap::new()).unwrap(), "");
    }

    #[test]
    fn render_context_block_as_template() {
        // Context blocks are just templates rendered with the same vars
        let v = vars(&[
            ("task.id", "42"),
            ("task.title", "Fix bug"),
            ("agents", "<agent name=\"dev\"/>"),
        ]);

        let block = r#"<current_task>
  <id>{{ task.id }}</id>
  <title>{{ task.title }}</title>
</current_task>"#;

        let result = render_template(block, &v);
        assert!(result.contains("<id>42</id>"));
        assert!(result.contains("<title>Fix bug</title>"));
    }

    #[test]
    fn render_deeply_nested_keys() {
        let v = vars(&[("a.b.c.d", "deep")]);
        assert_eq!(render_template("{{ a.b.c.d }}", &v), "deep");
    }

    // -----------------------------------------------------------------------
    // MiniJinja escaping behaviour
    // -----------------------------------------------------------------------

    #[test]
    fn auto_escape_is_disabled() {
        // HTML/XML special characters pass through unescaped because
        // auto-escape is set to None — variables containing pre-built XML
        // must be injected verbatim.
        let v = vars(&[("val", "<b>bold & \"quoted\"</b>")]);
        assert_eq!(render_template("{{ val }}", &v), "<b>bold & \"quoted\"</b>");
    }

    #[test]
    fn escape_filter_html_escapes() {
        // The |e (|escape) filter explicitly applies HTML escaping even
        // though auto-escape is off. Useful for user-supplied values that
        // must be safe inside XML attributes or content.
        let v = vars(&[("val", "<script>alert('xss')</script>")]);
        assert_eq!(
            render_template("{{ val|e }}", &v),
            "&lt;script&gt;alert(&#x27;xss&#x27;)&lt;&#x2f;script&gt;"
        );
    }

    #[test]
    fn escape_filter_ampersand_and_quotes() {
        let v = vars(&[("val", "a & b \"c\" 'd'")]);
        assert_eq!(
            render_template("{{ val|escape }}", &v),
            "a &amp; b &quot;c&quot; &#x27;d&#x27;"
        );
    }

    #[test]
    fn escape_filter_preserves_safe_text() {
        let v = vars(&[("val", "plain text 123")]);
        assert_eq!(render_template("{{ val|e }}", &v), "plain text 123");
    }

    #[test]
    fn backslash_in_plain_text_passes_through() {
        // A backslash NOT followed by {{ is left as-is.
        let v = vars(&[("val", "hello")]);
        assert_eq!(
            render_template("before\\nafter {{ val }}", &v),
            "before\\nafter hello"
        );
    }

    #[test]
    fn raw_block_preserves_delimiters() {
        // {% raw %} prevents MiniJinja from interpreting {{ }} delimiters.
        let v = vars(&[("val", "ignored")]);
        assert_eq!(
            render_template("{% raw %}{{ val }}{% endraw %}", &v),
            "{{ val }}"
        );
    }

    #[test]
    fn literal_braces_via_string_expression() {
        // To output a literal {{ in MiniJinja, use a string expression.
        assert_eq!(
            render_template("{{ '{{' }} content {{ '}}' }}", &HashMap::new()),
            "{{ content }}"
        );
    }

    #[test]
    fn escape_filter_on_xml_fragment() {
        // Applying |e to an XML fragment escapes all tags — useful when
        // the value should appear as visible text, not parsed XML.
        let v = vars(&[("xml", "<agent name=\"dev\"/>")]);
        assert_eq!(
            render_template("{{ xml|e }}", &v),
            "&lt;agent name=&quot;dev&quot;&#x2f;&gt;"
        );
    }

    #[test]
    fn no_filter_preserves_xml_fragment() {
        // Without |e, XML passes through raw (the default behaviour).
        let v = vars(&[("xml", "<agent name=\"dev\"/>")]);
        assert_eq!(render_template("{{ xml }}", &v), "<agent name=\"dev\"/>");
    }

    // -----------------------------------------------------------------------
    // Backslash escape pre-processor (unit tests on escape_backslash_braces)
    // -----------------------------------------------------------------------

    #[test]
    fn esc_plain_text_unchanged() {
        assert_eq!(escape_backslash_braces("no braces here"), "no braces here");
    }

    #[test]
    fn esc_normal_braces_unchanged() {
        assert_eq!(escape_backslash_braces("{{ val }}"), "{{ val }}");
    }

    #[test]
    fn esc_backslash_braces_converted() {
        assert_eq!(
            escape_backslash_braces(r"\{{ val }}"),
            "{% raw %}{{{% endraw %} val }}"
        );
    }

    #[test]
    fn esc_double_backslash_then_braces() {
        // \\{{ → literal \ followed by normal {{ (variable expression)
        assert_eq!(escape_backslash_braces(r"\\{{ val }}"), r"\{{ val }}");
    }

    #[test]
    fn esc_triple_backslash_then_braces() {
        // \\\{{ → \\ consumes to \, then \{{ escapes the braces
        assert_eq!(
            escape_backslash_braces(r"\\\{{ val }}"),
            "\\{% raw %}{{{% endraw %} val }}"
        );
    }

    #[test]
    fn esc_quadruple_backslash_then_braces() {
        // \\\\{{ → two \\ pairs → two literal backslashes + normal {{
        assert_eq!(escape_backslash_braces(r"\\\\{{ val }}"), r"\\{{ val }}");
    }

    #[test]
    fn esc_lone_backslash_at_end() {
        assert_eq!(escape_backslash_braces("text\\"), "text\\");
    }

    #[test]
    fn esc_backslash_before_single_brace() {
        // \{ is not \{{ — left as-is
        assert_eq!(escape_backslash_braces(r"\{ nope"), r"\{ nope");
    }

    #[test]
    fn esc_backslash_before_non_brace() {
        assert_eq!(escape_backslash_braces(r"\n\t"), r"\n\t");
    }

    #[test]
    fn esc_multiple_escaped_braces() {
        assert_eq!(
            escape_backslash_braces(r"\{{ a }} and \{{ b }}"),
            "{% raw %}{{{% endraw %} a }} and {% raw %}{{{% endraw %} b }}"
        );
    }

    #[test]
    fn esc_at_start_of_string() {
        assert_eq!(
            escape_backslash_braces(r"\{{ start }}"),
            "{% raw %}{{{% endraw %} start }}"
        );
    }

    #[test]
    fn esc_adjacent_escaped_braces() {
        assert_eq!(
            escape_backslash_braces(r"\{{\{{"),
            "{% raw %}{{{% endraw %}{% raw %}{{{% endraw %}"
        );
    }

    #[test]
    fn esc_empty_string() {
        assert_eq!(escape_backslash_braces(""), "");
    }

    #[test]
    fn esc_only_backslash() {
        assert_eq!(escape_backslash_braces("\\"), "\\");
    }

    #[test]
    fn esc_only_double_backslash() {
        assert_eq!(escape_backslash_braces("\\\\"), "\\");
    }

    #[test]
    fn esc_mixed_escaped_and_normal() {
        assert_eq!(
            escape_backslash_braces(r"{{ a }} \{{ b }} {{ c }}"),
            "{{ a }} {% raw %}{{{% endraw %} b }} {{ c }}"
        );
    }

    #[test]
    fn esc_backslash_far_from_braces() {
        assert_eq!(
            escape_backslash_braces(r"path\to\file {{ val }}"),
            r"path\to\file {{ val }}"
        );
    }

    #[test]
    fn esc_double_backslash_without_braces() {
        // \\ not followed by {{ → literal backslash
        assert_eq!(escape_backslash_braces(r"a\\b"), r"a\b");
    }

    // -----------------------------------------------------------------------
    // Backslash escape end-to-end (through render_template)
    // -----------------------------------------------------------------------

    #[test]
    fn render_backslash_escapes_braces() {
        let v = vars(&[("val", "replaced")]);
        assert_eq!(render_template(r"text \{{ val }}", &v), "text {{ val }}");
    }

    #[test]
    fn render_backslash_escape_mixed_with_variable() {
        let v = vars(&[("name", "dev")]);
        assert_eq!(
            render_template(r"hello {{ name }}, use \{{ var }} for literals", &v),
            "hello dev, use {{ var }} for literals"
        );
    }

    #[test]
    fn render_backslash_escape_multiple() {
        assert_eq!(
            render_template(r"\{{ a }} and \{{ b }}", &HashMap::new()),
            "{{ a }} and {{ b }}"
        );
    }

    #[test]
    fn render_double_backslash_renders_variable() {
        // \\{{ → literal \ + render variable
        let v = vars(&[("val", "hello")]);
        assert_eq!(render_template(r"\\{{ val }}", &v), r"\hello");
    }

    #[test]
    fn render_triple_backslash_escapes_braces() {
        // \\\{{ → literal \ + literal {{
        assert_eq!(
            render_template(r"\\\{{ val }}", &HashMap::new()),
            r"\{{ val }}"
        );
    }

    #[test]
    fn render_no_backslash_renders_variable() {
        let v = vars(&[("val", "hello")]);
        assert_eq!(render_template("{{ val }}", &v), "hello");
    }

    #[test]
    fn render_backslash_in_json_prompt() {
        // Real-world case: prompt contains JSON with escaped braces
        let v = vars(&[("format", "ignored")]);
        assert_eq!(
            render_template(r#"Respond in JSON: \{{ "key": "value" }}"#, &v,),
            r#"Respond in JSON: {{ "key": "value" }}"#
        );
    }

    #[test]
    fn render_backslash_with_jinja_block() {
        // \{{ inside a conditional block
        let v = vars(&[("show", "yes")]);
        assert_eq!(
            render_template(r"{% if show %}literal: \{{ x }}{% endif %}", &v),
            "literal: {{ x }}"
        );
    }

    #[test]
    fn render_backslash_only_escapes_double_brace() {
        // \{ alone (single brace) is not special
        let v = vars(&[("val", "hi")]);
        assert_eq!(
            render_template(r"\{ not escaped {{ val }}", &v),
            r"\{ not escaped hi"
        );
    }

    // -----------------------------------------------------------------------
    // \{% block tag escaping — pre-processor unit tests
    // -----------------------------------------------------------------------

    #[test]
    fn esc_block_tag_converted() {
        assert_eq!(
            escape_backslash_braces(r"\{% if x %}"),
            "{% raw %}{%{% endraw %} if x %}"
        );
    }

    #[test]
    fn esc_block_tag_normal_unchanged() {
        assert_eq!(escape_backslash_braces("{% if x %}"), "{% if x %}");
    }

    #[test]
    fn esc_double_backslash_block_tag() {
        // \\{% → literal \ + real block tag
        assert_eq!(escape_backslash_braces(r"\\{% if x %}"), r"\{% if x %}");
    }

    #[test]
    fn esc_mixed_var_and_block_escapes() {
        assert_eq!(
            escape_backslash_braces(r"\{% if ok %}\{{ val }}\{% endif %}"),
            "{% raw %}{%{% endraw %} if ok %}{% raw %}{{{% endraw %} val }}{% raw %}{%{% endraw %} endif %}"
        );
    }

    #[test]
    fn esc_block_tag_at_start() {
        assert_eq!(
            escape_backslash_braces(r"\{% raw %}"),
            "{% raw %}{%{% endraw %} raw %}"
        );
    }

    #[test]
    fn esc_block_tag_adjacent() {
        assert_eq!(
            escape_backslash_braces(r"\{%\{%"),
            "{% raw %}{%{% endraw %}{% raw %}{%{% endraw %}"
        );
    }

    // -----------------------------------------------------------------------
    // \{% block tag escaping — end-to-end through render_template
    // -----------------------------------------------------------------------

    #[test]
    fn render_escaped_block_tags_literal() {
        // \{% if %} should output literal {% if %}, not be interpreted
        assert_eq!(
            render_template(
                r"\{% if task.id %}\{{ task.title }}\{% endif %}",
                &HashMap::new()
            ),
            "{% if task.id %}{{ task.title }}{% endif %}"
        );
    }

    #[test]
    fn render_escaped_block_mixed_with_real_block() {
        // Real {% if %} block + escaped \{{ inside it
        let v = vars(&[("show", "yes")]);
        assert_eq!(
            render_template(r"{% if show %}use \{{ var }} syntax{% endif %}", &v),
            "use {{ var }} syntax"
        );
    }

    #[test]
    fn render_escaped_conditional_example() {
        // The exact pattern from the template_vars_guide conditional_rendering section
        assert_eq!(
            render_template(
                r"\{% if task.id %}Task: \{{ task.title }}\{% endif %}",
                &HashMap::new(),
            ),
            "{% if task.id %}Task: {{ task.title }}{% endif %}"
        );
    }

    #[test]
    fn render_escaped_if_else_example() {
        assert_eq!(
            render_template(
                r"\{% if task.id %}...\{% else %}No task context\{% endif %}",
                &HashMap::new(),
            ),
            "{% if task.id %}...{% else %}No task context{% endif %}"
        );
    }

    #[test]
    fn render_double_backslash_block_tag() {
        // \\{% → literal \ + real block tag executed
        let v = vars(&[("x", "yes")]);
        assert_eq!(render_template(r"\\{% if x %}ok{% endif %}", &v), r"\ok");
    }

    // -----------------------------------------------------------------------
    // Multi-byte UTF-8 preservation
    // -----------------------------------------------------------------------

    #[test]
    fn esc_preserves_em_dash() {
        assert_eq!(escape_backslash_braces("hello — world"), "hello — world");
    }

    #[test]
    fn esc_preserves_emoji() {
        assert_eq!(
            escape_backslash_braces("status: ✅ done"),
            "status: ✅ done"
        );
    }

    #[test]
    fn esc_preserves_multibyte_with_escapes() {
        assert_eq!(
            escape_backslash_braces(r"\{{ val }} — description"),
            "{% raw %}{{{% endraw %} val }} — description"
        );
    }

    #[test]
    fn render_preserves_em_dash() {
        let v = vars(&[("name", "dev")]);
        assert_eq!(
            render_template(r"\{{ self }} — Full XML {{ name }}", &v),
            "{{ self }} — Full XML dev"
        );
    }

    #[test]
    fn render_preserves_cjk() {
        assert_eq!(
            render_template("日本語テスト", &HashMap::new()),
            "日本語テスト"
        );
    }

    #[test]
    fn esc_preserves_mixed_unicode_and_escapes() {
        assert_eq!(
            escape_backslash_braces(r"\{{ agent.id }} — UUID • \{{ agent.name }} — Name"),
            "{% raw %}{{{% endraw %} agent.id }} — UUID • {% raw %}{{{% endraw %} agent.name }} — Name"
        );
    }
}
