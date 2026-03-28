//! Memory → prompt XML injection.

use anyhow::Result;

use super::Memory;
use super::types::MemoryScope;

/// Build `<memory>` XML from all 3 tiers of summaries for prompt injection.
///
/// Output format:
/// ```xml
/// <memory>
/// <memory-core>
///   <category name="security">Always check auth bypass</category>
/// </memory-core>
/// <memory-summaries>
///   <category name="preferences">User prefers Rust</category>
/// </memory-summaries>
/// <memory-shared>
///   <category name="decisions">Using PostgreSQL for DB</category>
/// </memory-shared>
/// </memory>
/// ```
///
/// Returns empty string if no summaries exist in any tier.
pub async fn build_memory_xml(memory: &dyn Memory, scope: &MemoryScope) -> Result<String> {
    let core_summaries = memory.list_summaries(&scope.core).await?;
    let project_summaries = memory.list_summaries(&scope.project).await?;
    let shared_summaries = memory.list_summaries(&scope.shared).await?;

    if core_summaries.is_empty() && project_summaries.is_empty() && shared_summaries.is_empty() {
        return Ok(String::new());
    }

    let mut xml = String::from("<memory>\n");

    if !core_summaries.is_empty() {
        xml.push_str("<memory-core>\n");
        for s in &core_summaries {
            xml.push_str(&format!(
                "  <category name=\"{}\">{}</category>\n",
                s.category, s.text
            ));
        }
        xml.push_str("</memory-core>\n");
    }

    if !project_summaries.is_empty() {
        xml.push_str("<memory-summaries>\n");
        for s in &project_summaries {
            xml.push_str(&format!(
                "  <category name=\"{}\">{}</category>\n",
                s.category, s.text
            ));
        }
        xml.push_str("</memory-summaries>\n");
    }

    if !shared_summaries.is_empty() {
        xml.push_str("<memory-shared>\n");
        for s in &shared_summaries {
            xml.push_str(&format!(
                "  <category name=\"{}\">{}</category>\n",
                s.category, s.text
            ));
        }
        xml.push_str("</memory-shared>\n");
    }

    xml.push_str("</memory>");
    Ok(xml)
}
