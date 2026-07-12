//! Multi-version package content selection for logical selectors.
//!
//! Package content (context, knowledge, abilities, domains) may coexist as
//! versioned instances. Authors always use **logical** (unversioned) refs.
//! Version is chosen by policy:
//!
//! - [`PkgResolvePolicy::HighestSemver`] — platform / native agents
//! - [`PkgResolvePolicy::DependencyLock`] — package-authored agents

use std::cmp::Ordering;
use std::collections::BTreeMap;

/// How to pick among multiple installed versions of the same logical resource.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum PkgResolvePolicy {
    /// Prefer the highest installed semantic version (platform-authored agents).
    #[default]
    HighestSemver,
    /// Prefer exact versions from a package install lock (package-authored agents).
    ///
    /// Keys are package names as stored in the lock (`context`, `@nenjo-ai/context`, …).
    DependencyLock(BTreeMap<String, String>),
}

/// A versioned package content candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionedCandidate {
    /// Owning package name when known (`@nenjo-ai/context` or `context`).
    pub package_name: Option<String>,
    /// Package version when known (`1.0.4` or path segment `v1_0_4`).
    pub package_version: Option<String>,
    /// Versioned storage path (e.g. `pkg/nenjo_ai/packages/v1_0_4/context/tools`).
    pub path: String,
    /// Resource leaf name (context block name, ability name, …).
    pub name: String,
}

impl VersionedCandidate {
    pub fn logical_dotted_key(&self) -> String {
        logical_dotted_key(&self.path, &self.name)
    }

    pub fn version_rank(&self) -> Vec<u64> {
        if let Some(version) = self.package_version.as_deref() {
            return parse_semver_rank(version);
        }
        version_label_from_path(&self.path)
            .map(|version| parse_semver_rank(&version))
            .unwrap_or_default()
    }

    pub fn package_leaf(&self) -> Option<String> {
        self.package_name
            .as_deref()
            .map(package_name_leaf)
            .or_else(|| package_leaf_from_path(&self.path))
    }
}

/// Strip a single version segment (`v1_0_4` / `v1.0.4`) from a storage path.
pub fn logical_path(path: &str) -> String {
    path.split('/')
        .filter(|segment| !segment.is_empty() && !is_version_segment(segment))
        .collect::<Vec<_>>()
        .join("/")
}

/// Logical dotted selector key: `pkg.nenjo_ai.packages.context.tools.tool_usage`.
pub fn logical_dotted_key(path: &str, name: &str) -> String {
    let logical = logical_path(path);
    if logical.is_empty() {
        name.to_string()
    } else {
        format!("{}.{}", logical.replace('/', "."), name)
    }
}

/// True if path segment looks like a package version label (`v1_0_4`, `v1.0.4`).
pub fn is_version_segment(segment: &str) -> bool {
    let s = segment.trim();
    let Some(rest) = s.strip_prefix('v').or_else(|| s.strip_prefix('V')) else {
        return false;
    };
    if rest.is_empty() {
        return false;
    }
    rest.chars()
        .all(|ch| ch.is_ascii_digit() || ch == '_' || ch == '.')
        && rest.chars().any(|ch| ch.is_ascii_digit())
}

/// Extract version label from a path (`v1_0_4` → `1.0.4` for ranking).
pub fn version_label_from_path(path: &str) -> Option<String> {
    path.split('/')
        .find(|segment| is_version_segment(segment))
        .map(|segment| {
            let rest = segment.trim().trim_start_matches(['v', 'V']);
            rest.replace('_', ".")
        })
}

/// Parse a semver-like string into comparable numeric parts.
pub fn parse_semver_rank(version: &str) -> Vec<u64> {
    let cleaned = version.trim().trim_start_matches(['v', 'V']);
    if cleaned.is_empty() {
        return Vec::new();
    }
    cleaned
        .split(|ch: char| !ch.is_ascii_digit())
        .filter(|part| !part.is_empty())
        .map(|part| part.parse::<u64>().unwrap_or(0))
        .collect()
}

fn compare_semver_rank(a: &[u64], b: &[u64]) -> Ordering {
    let len = a.len().max(b.len());
    for i in 0..len {
        let left = a.get(i).copied().unwrap_or(0);
        let right = b.get(i).copied().unwrap_or(0);
        match left.cmp(&right) {
            Ordering::Equal => {}
            other => return other,
        }
    }
    Ordering::Equal
}

/// Leaf package name: `@nenjo-ai/context` → `context`, `context` → `context`.
pub fn package_name_leaf(package_name: &str) -> String {
    package_name
        .trim()
        .trim_start_matches('@')
        .rsplit('/')
        .next()
        .unwrap_or(package_name)
        .to_ascii_lowercase()
}

fn package_leaf_from_path(path: &str) -> Option<String> {
    // pkg/<scope...>/<version?>/<package_leaf>/...
    let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
    if parts.first().copied() != Some("pkg") {
        return None;
    }
    let mut i = 1usize;
    // skip scope segments until version or package leaf heuristics
    while i < parts.len() && !is_version_segment(parts[i]) {
        // stop before module-ish dirs if we already have scope+leaf
        i += 1;
        if i >= 3 {
            break;
        }
    }
    if i < parts.len() && is_version_segment(parts[i]) {
        i += 1;
    }
    parts.get(i).map(|s| s.to_ascii_lowercase())
}

fn package_names_match(candidate: &str, lock_key: &str) -> bool {
    let c = candidate
        .trim()
        .trim_start_matches('@')
        .to_ascii_lowercase();
    let k = lock_key.trim().trim_start_matches('@').to_ascii_lowercase();
    c == k || package_name_leaf(&c) == package_name_leaf(&k)
}

fn version_matches(candidate_version: &str, locked: &str) -> bool {
    let a = parse_semver_rank(candidate_version);
    let b = parse_semver_rank(locked);
    !a.is_empty() && a == b
        || candidate_version.trim() == locked.trim()
        || candidate_version.replace('_', ".") == locked.replace('_', ".")
        || format!(
            "v{}",
            candidate_version
                .trim_start_matches(['v', 'V'])
                .replace('.', "_")
        ) == format!(
            "v{}",
            locked.trim_start_matches(['v', 'V']).replace('.', "_")
        )
}

/// Prefer highest semver among candidates (stable order for ties).
pub fn prefer_highest_semver<T>(
    candidates: impl IntoIterator<Item = (T, VersionedCandidate)>,
) -> Option<(T, VersionedCandidate)> {
    let mut best: Option<(T, VersionedCandidate)> = None;
    for (item, cand) in candidates {
        match &best {
            None => best = Some((item, cand)),
            Some((_, best_cand)) => {
                let cmp = compare_semver_rank(&cand.version_rank(), &best_cand.version_rank());
                if cmp == Ordering::Greater {
                    best = Some((item, cand));
                }
            }
        }
    }
    best
}

/// Pick a winner among candidates that already share a logical identity.
///
/// Used for knowledge pack selectors, ability names, and domain names after
/// filtering to the same logical key.
pub fn pick_version_winner<T: Clone>(
    candidates: &[(T, VersionedCandidate)],
    policy: &PkgResolvePolicy,
) -> Option<T> {
    if candidates.is_empty() {
        return None;
    }
    match policy {
        PkgResolvePolicy::HighestSemver => {
            prefer_highest_semver(candidates.iter().cloned()).map(|(item, _)| item)
        }
        PkgResolvePolicy::DependencyLock(lock) => {
            let locked: Vec<(T, VersionedCandidate)> = candidates
                .iter()
                .filter(|(_, c)| {
                    let Some(leaf) = c.package_leaf() else {
                        return false;
                    };
                    let version = if let Some(version) = c.package_version.as_deref() {
                        version.to_string()
                    } else if let Some(version) = version_label_from_path(&c.path) {
                        version
                    } else {
                        return false;
                    };
                    lock.iter().any(|(pkg, locked_ver)| {
                        (package_names_match(&leaf, pkg)
                            || c.package_name
                                .as_deref()
                                .is_some_and(|n| package_names_match(n, pkg)))
                            && version_matches(&version, locked_ver)
                    })
                })
                .cloned()
                .collect();
            if let Some((item, _)) = prefer_highest_semver(locked) {
                return Some(item);
            }
            // Fallback: highest semver if lock has no entry for this package.
            prefer_highest_semver(candidates.iter().cloned()).map(|(item, _)| item)
        }
    }
}

/// Resolve a logical dotted key under a policy.
///
/// `candidates` should already be filtered to the same logical key (or will be filtered here).
pub fn resolve_logical_key<T: Clone>(
    logical_key: &str,
    candidates: &[(T, VersionedCandidate)],
    policy: &PkgResolvePolicy,
) -> Option<T> {
    let matching: Vec<(T, VersionedCandidate)> = candidates
        .iter()
        .filter(|(_, c)| c.logical_dotted_key() == logical_key)
        .cloned()
        .collect();
    pick_version_winner(&matching, policy)
}

/// Build a map of logical_key → preferred item index under policy.
pub fn resolve_all_logical_winners<T: Clone>(
    candidates: &[(T, VersionedCandidate)],
    policy: &PkgResolvePolicy,
) -> BTreeMap<String, T> {
    let mut by_logical: BTreeMap<String, Vec<(T, VersionedCandidate)>> = BTreeMap::new();
    for (item, cand) in candidates {
        by_logical
            .entry(cand.logical_dotted_key())
            .or_default()
            .push((item.clone(), cand.clone()));
    }
    let mut winners = BTreeMap::new();
    for (key, group) in by_logical {
        if let Some(item) = resolve_logical_key(&key, &group, policy) {
            winners.insert(key, item);
        }
    }
    winners
}

/// Policy from agent metadata (`source_type` + `resolved_dependencies`).
pub fn policy_from_agent_metadata(
    source_type: Option<&str>,
    metadata: Option<&serde_json::Value>,
) -> PkgResolvePolicy {
    let is_package = source_type.is_some_and(|s| s.eq_ignore_ascii_case("package"))
        || metadata.is_some_and(|m| {
            m.get("install").is_some()
                || m.get("package").is_some()
                || m.get("resolved_dependencies").is_some()
        });
    if !is_package {
        return PkgResolvePolicy::HighestSemver;
    }
    let lock = metadata
        .and_then(|m| m.get("resolved_dependencies"))
        .and_then(|v| v.as_object())
        .map(|obj| {
            obj.iter()
                .filter_map(|(k, v)| v.as_str().map(|version| (k.clone(), version.to_string())))
                .collect::<BTreeMap<_, _>>()
        })
        .unwrap_or_default();
    if lock.is_empty() {
        PkgResolvePolicy::HighestSemver
    } else {
        PkgResolvePolicy::DependencyLock(lock)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(
        path: &str,
        name: &str,
        package: Option<&str>,
        version: Option<&str>,
    ) -> VersionedCandidate {
        VersionedCandidate {
            package_name: package.map(str::to_string),
            package_version: version.map(str::to_string),
            path: path.to_string(),
            name: name.to_string(),
        }
    }

    #[test]
    fn strips_version_segment_from_path() {
        assert_eq!(
            logical_path("pkg/nenjo_ai/packages/v1_0_4/context/tools"),
            "pkg/nenjo_ai/packages/context/tools"
        );
        assert_eq!(
            logical_dotted_key("pkg/nenjo_ai/packages/v1_0_4/context/tools", "tool_usage"),
            "pkg.nenjo_ai.packages.context.tools.tool_usage"
        );
    }

    #[test]
    fn highest_semver_wins() {
        let items = vec![
            (
                "old",
                cand(
                    "pkg/nenjo_ai/packages/v1_0_3/context/tools",
                    "tool_usage",
                    Some("context"),
                    Some("1.0.3"),
                ),
            ),
            (
                "new",
                cand(
                    "pkg/nenjo_ai/packages/v1_0_4/context/tools",
                    "tool_usage",
                    Some("context"),
                    Some("1.0.4"),
                ),
            ),
        ];
        let key = "pkg.nenjo_ai.packages.context.tools.tool_usage";
        let winner = resolve_logical_key(key, &items, &PkgResolvePolicy::HighestSemver);
        assert_eq!(winner, Some("new"));
    }

    #[test]
    fn dependency_lock_selects_exact_version() {
        let items = vec![
            (
                "old",
                cand(
                    "pkg/nenjo_ai/packages/v1_0_3/context/tools",
                    "tool_usage",
                    Some("@nenjo-ai/context"),
                    Some("1.0.3"),
                ),
            ),
            (
                "new",
                cand(
                    "pkg/nenjo_ai/packages/v1_0_4/context/tools",
                    "tool_usage",
                    Some("@nenjo-ai/context"),
                    Some("1.0.4"),
                ),
            ),
        ];
        let key = "pkg.nenjo_ai.packages.context.tools.tool_usage";
        let mut lock = BTreeMap::new();
        lock.insert("context".to_string(), "1.0.3".to_string());
        let winner = resolve_logical_key(key, &items, &PkgResolvePolicy::DependencyLock(lock));
        assert_eq!(winner, Some("old"));
    }

    #[test]
    fn dependency_lock_falls_back_to_highest_when_package_unlisted() {
        let items = vec![
            (
                "old",
                cand(
                    "pkg/nenjo_ai/packages/v1_0_3/context/tools",
                    "tool_usage",
                    Some("context"),
                    Some("1.0.3"),
                ),
            ),
            (
                "new",
                cand(
                    "pkg/nenjo_ai/packages/v1_0_4/context/tools",
                    "tool_usage",
                    Some("context"),
                    Some("1.0.4"),
                ),
            ),
        ];
        let key = "pkg.nenjo_ai.packages.context.tools.tool_usage";
        let mut lock = BTreeMap::new();
        lock.insert("knowledge".to_string(), "2.0.0".to_string());
        let winner = resolve_logical_key(key, &items, &PkgResolvePolicy::DependencyLock(lock));
        assert_eq!(winner, Some("new"));
    }

    #[test]
    fn policy_from_native_metadata_is_highest() {
        assert_eq!(
            policy_from_agent_metadata(Some("native"), None),
            PkgResolvePolicy::HighestSemver
        );
    }

    #[test]
    fn policy_from_package_metadata_uses_lock() {
        let meta = serde_json::json!({
            "resolved_dependencies": {
                "context": "1.0.4",
                "knowledge": "1.0.2"
            }
        });
        match policy_from_agent_metadata(Some("package"), Some(&meta)) {
            PkgResolvePolicy::DependencyLock(lock) => {
                assert_eq!(lock.get("context").map(String::as_str), Some("1.0.4"));
            }
            other => panic!("expected lock policy, got {other:?}"),
        }
    }

    #[test]
    fn prefer_highest_among_path_only_versions() {
        let items = vec![
            (
                0usize,
                cand("pkg/acme/v1_0_0/abilities/review", "review", None, None),
            ),
            (
                1usize,
                cand("pkg/acme/v2_0_0/abilities/review", "review", None, None),
            ),
        ];
        let key = logical_dotted_key("pkg/acme/v2_0_0/abilities/review", "review");
        assert_eq!(
            resolve_logical_key(&key, &items, &PkgResolvePolicy::HighestSemver),
            Some(1)
        );
    }

    #[test]
    fn pick_version_winner_uses_lock_without_logical_key_filter() {
        let items = vec![
            (
                "v103",
                cand(
                    "pkg/nenjo_ai/knowledge/v1_0_3",
                    "core",
                    Some("@nenjo-ai/knowledge"),
                    Some("1.0.3"),
                ),
            ),
            (
                "v104",
                cand(
                    "pkg/nenjo_ai/knowledge/v1_0_4",
                    "core",
                    Some("@nenjo-ai/knowledge"),
                    Some("1.0.4"),
                ),
            ),
        ];
        let mut lock = BTreeMap::new();
        lock.insert("knowledge".to_string(), "1.0.3".to_string());
        assert_eq!(
            pick_version_winner(&items, &PkgResolvePolicy::DependencyLock(lock)),
            Some("v103")
        );
        assert_eq!(
            pick_version_winner(&items, &PkgResolvePolicy::HighestSemver),
            Some("v104")
        );
    }
}
