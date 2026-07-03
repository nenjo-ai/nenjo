use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, anyhow};

use crate::{PackageKind, ResolvedModule, ResolvedPackage, validate_package_name};

pub(crate) fn unique_modules(package: &ResolvedPackage) -> impl Iterator<Item = &ResolvedModule> {
    package
        .modules
        .iter()
        .filter(|(key, module)| *key == &module.key())
        .map(|(_, module)| module)
}

pub(crate) fn validate_module_imports(
    package: &ResolvedPackage,
    module: &ResolvedModule,
) -> anyhow::Result<()> {
    for import in &module.imports {
        import
            .validate()
            .with_context(|| format!("{} import {}", module.path, import.reference))?;
        if import.surface == "context" {
            let target = resolve_local_import_target(&module.path, &import.reference)
                .with_context(|| {
                    format!("failed to resolve context import {}", import.reference)
                })?;
            let resolved = package.modules.get(&target).ok_or_else(|| {
                anyhow!(
                    "{} imports missing local context module {}",
                    module.path,
                    import.reference
                )
            })?;
            if resolved.kind != PackageKind::ContextBlock {
                anyhow::bail!(
                    "{} imports {} as context, but target is {}",
                    module.path,
                    import.reference,
                    resolved.kind.as_str()
                );
            }
        }
    }
    Ok(())
}

pub(crate) fn validate_context_graph(package: &ResolvedPackage) -> anyhow::Result<()> {
    let mut graph: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for module in unique_modules(package) {
        if module.kind != PackageKind::ContextBlock {
            continue;
        }
        let key = module.key();
        let mut deps = Vec::new();
        for import in module
            .imports
            .iter()
            .filter(|import| import.surface == "context")
        {
            deps.push(resolve_local_import_target(
                &module.path,
                &import.reference,
            )?);
        }
        graph.insert(key, deps);
    }

    let mut temporary = BTreeSet::new();
    let mut permanent = BTreeSet::new();
    for key in graph.keys() {
        visit_context(key, &graph, &mut temporary, &mut permanent, &mut Vec::new())?;
    }
    Ok(())
}

fn visit_context(
    key: &str,
    graph: &BTreeMap<String, Vec<String>>,
    temporary: &mut BTreeSet<String>,
    permanent: &mut BTreeSet<String>,
    stack: &mut Vec<String>,
) -> anyhow::Result<()> {
    if permanent.contains(key) {
        return Ok(());
    }
    if !temporary.insert(key.to_string()) {
        stack.push(key.to_string());
        anyhow::bail!("context import cycle: {}", stack.join(" -> "));
    }
    stack.push(key.to_string());
    for dependency in graph.get(key).into_iter().flatten() {
        if graph.contains_key(dependency) {
            visit_context(dependency, graph, temporary, permanent, stack)?;
        }
    }
    stack.pop();
    temporary.remove(key);
    permanent.insert(key.to_string());
    Ok(())
}

pub(crate) fn package_selector(package: &str) -> anyhow::Result<String> {
    validate_package_name(package)?;
    if let Some((scope, name)) = package
        .strip_prefix('@')
        .and_then(|value| value.split_once('/'))
    {
        Ok(format!(
            "pkg.{}.{}",
            selector_segment(scope),
            selector_segment(name)
        ))
    } else {
        Ok(format!("pkg.{}", selector_segment(package)))
    }
}

pub(crate) fn package_selector_aliases(package: &str) -> anyhow::Result<BTreeSet<String>> {
    validate_package_name(package)?;
    let mut selectors = BTreeSet::from([package_selector(package)?]);
    if let Some((scope, name)) = package
        .strip_prefix('@')
        .and_then(|value| value.split_once('/'))
    {
        selectors.insert(format!(
            "pkg.{}.packages.{}",
            selector_segment(scope),
            selector_segment(name)
        ));
    } else {
        selectors.insert(format!(
            "pkg.nenjo_ai.packages.{}",
            selector_segment(package)
        ));
    }
    Ok(selectors)
}

pub(crate) fn pkg_selector_is_allowed(
    selector: &str,
    package_selectors: &BTreeSet<String>,
    dependency_selectors: &BTreeSet<String>,
) -> bool {
    selector_matches_any(selector, package_selectors)
        || selector_matches_any(selector, dependency_selectors)
        || dependency_selectors.contains(selector)
        || selector_fully_qualified_package_leaf(selector).is_some_and(|leaf| {
            package_selectors
                .iter()
                .any(|package| selector_matches_unscoped_package(package, leaf))
                || dependency_selectors
                    .iter()
                    .any(|dependency| selector_matches_unscoped_package(dependency, leaf))
        })
        || selector_package_slug(selector).is_some_and(|slug| {
            package_selectors
                .iter()
                .any(|package| selector_matches_unscoped_package(package, slug))
                || dependency_selectors
                    .iter()
                    .any(|dependency| selector_matches_unscoped_package(dependency, slug))
        })
}

fn selector_matches_any(selector: &str, allowed: &BTreeSet<String>) -> bool {
    allowed.iter().any(|allowed| {
        selector == allowed
            || selector
                .strip_prefix(allowed)
                .is_some_and(|suffix| suffix.starts_with('.'))
    })
}

fn selector_matches_unscoped_package(package_selector: &str, slug: &str) -> bool {
    package_selector
        .strip_prefix("pkg.")
        .is_some_and(|package| !package.contains('.') && package == slug)
}

fn selector_segment(value: &str) -> String {
    value.replace('-', "_")
}

fn selector_package_slug(selector: &str) -> Option<&str> {
    let mut parts = selector.split('.');
    if parts.next()? != "pkg" {
        return None;
    }
    let first = parts.next()?;
    let second = parts.next();
    second.or(Some(first))
}

pub(crate) fn selector_fully_qualified_package_leaf(selector: &str) -> Option<&str> {
    let mut parts = selector.split('.');
    if parts.next()? != "pkg" {
        return None;
    }
    let _scope = parts.next()?;
    let repo = parts.next()?;
    if repo != "packages" {
        return None;
    }
    parts.next()
}

pub(crate) fn selector_to_package_name(selector: &str) -> String {
    let parts = selector.split('.').collect::<Vec<_>>();
    if let Some(leaf) = selector_fully_qualified_package_leaf(selector) {
        return leaf.to_string();
    }
    if parts.len() >= 3 {
        format!("@{}/{}", parts[1], parts[2])
    } else {
        selector.to_string()
    }
}

pub(crate) fn collect_strings<'a>(value: &'a serde_json::Value, out: &mut Vec<&'a str>) {
    match value {
        serde_json::Value::String(value) => out.push(value),
        serde_json::Value::Array(values) => {
            for value in values {
                collect_strings(value, out);
            }
        }
        serde_json::Value::Object(values) => {
            for value in values.values() {
                collect_strings(value, out);
            }
        }
        _ => {}
    }
}

pub(crate) fn scan_pkg_selectors(value: &str) -> Vec<String> {
    scan_selector_path(value, "pkg.")
        .into_iter()
        .filter(|segments| !segments.is_empty())
        .map(|segments| {
            let package_segment_count = if segments.len() >= 3 && segments[1] == "packages" {
                3
            } else {
                segments.len().min(2)
            };
            format!(
                "pkg.{}",
                segments
                    .into_iter()
                    .take(package_segment_count)
                    .collect::<Vec<_>>()
                    .join(".")
            )
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

pub(crate) fn scan_pkg_reference_selectors(value: &str) -> Vec<String> {
    scan_selector_path(value, "pkg.")
        .into_iter()
        .filter(|segments| !segments.is_empty())
        .map(|segments| format!("pkg.{}", segments.join(".")))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

pub(crate) fn scan_context_selectors(value: &str) -> Vec<String> {
    scan_selector(value, "context.", 1)
        .into_iter()
        .map(|segments| segments[0].clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn scan_selector_path(value: &str, prefix: &str) -> Vec<Vec<String>> {
    let bytes = value.as_bytes();
    let mut out = Vec::new();
    let mut index = 0;
    while let Some(offset) = value[index..].find(prefix) {
        let start = index + offset;
        if start > 0 {
            let previous = bytes[start - 1] as char;
            if is_ident_continue(previous) || previous == '.' {
                index = start + prefix.len();
                continue;
            }
        }
        if has_odd_preceding_backslashes(value, start) {
            index = start + prefix.len();
            continue;
        }
        let mut cursor = start + prefix.len();
        let mut segments = Vec::new();
        while let Some((segment, next_cursor)) = read_ident(value, cursor) {
            segments.push(segment.to_string());
            cursor = next_cursor;
            if value[cursor..].starts_with('.') {
                cursor += 1;
            } else {
                break;
            }
        }
        if !segments.is_empty() {
            out.push(segments);
        }
        index = (start + prefix.len()).min(value.len());
    }
    out
}

fn scan_selector(value: &str, prefix: &str, segment_count: usize) -> Vec<Vec<String>> {
    let bytes = value.as_bytes();
    let mut out = Vec::new();
    let mut index = 0;
    while let Some(offset) = value[index..].find(prefix) {
        let start = index + offset;
        if start > 0 {
            let previous = bytes[start - 1] as char;
            if is_ident_continue(previous) || previous == '.' {
                index = start + prefix.len();
                continue;
            }
        }
        if has_odd_preceding_backslashes(value, start) {
            index = start + prefix.len();
            continue;
        }
        let mut cursor = start + prefix.len();
        let mut segments = Vec::new();
        for segment_index in 0..segment_count {
            let Some((segment, next_cursor)) = read_ident(value, cursor) else {
                break;
            };
            segments.push(segment.to_string());
            cursor = next_cursor;
            if segment_index + 1 < segment_count {
                if value[cursor..].starts_with('.') {
                    cursor += 1;
                } else {
                    break;
                }
            }
        }
        if segments.len() == segment_count {
            out.push(segments);
        }
        index = (start + prefix.len()).min(value.len());
    }
    out
}

fn read_ident(value: &str, start: usize) -> Option<(&str, usize)> {
    let mut chars = value[start..].char_indices();
    let (_, first) = chars.next()?;
    if !is_ident_start(first) {
        return None;
    }
    let mut end = start + first.len_utf8();
    for (offset, ch) in chars {
        if !is_ident_continue(ch) {
            break;
        }
        end = start + offset + ch.len_utf8();
    }
    Some((&value[start..end], end))
}

fn is_ident_start(ch: char) -> bool {
    ch == '_' || ch.is_ascii_alphabetic()
}

fn is_ident_continue(ch: char) -> bool {
    is_ident_start(ch) || ch.is_ascii_digit()
}

fn has_odd_preceding_backslashes(value: &str, start: usize) -> bool {
    value[..start]
        .chars()
        .rev()
        .take_while(|ch| *ch == '\\')
        .count()
        % 2
        == 1
}

pub(crate) fn context_import_name(module_path: &str, reference: &str) -> anyhow::Result<String> {
    if let Some(fragment) = reference.trim().strip_prefix('#') {
        return Ok(fragment.to_string());
    }
    let target = resolve_local_import_target(module_path, reference)?;
    let (_, resource) = target
        .rsplit_once('#')
        .ok_or_else(|| anyhow!("context import {reference} did not resolve to a resource"))?;
    Ok(resource.to_string())
}

pub(crate) fn resolve_local_import_target(
    module_path: &str,
    reference: &str,
) -> anyhow::Result<String> {
    let reference = reference.trim();
    if let Some(fragment) = reference.strip_prefix('#') {
        return Ok(format!("{module_path}#{fragment}"));
    }

    let (path, fragment) = reference
        .split_once('#')
        .map_or((reference, None), |(path, fragment)| (path, Some(fragment)));
    if fragment.is_some_and(str::is_empty) {
        anyhow::bail!("local import {reference} has empty fragment");
    }
    let base_dir = module_path.rsplit_once('/').map_or("", |(dir, _)| dir);
    let normalized = normalize_relative_path(base_dir, path)?;
    let fragment = fragment
        .map(str::to_string)
        .unwrap_or_else(|| inferred_resource_name(&normalized));
    Ok(format!("{normalized}#{fragment}"))
}

fn inferred_resource_name(path: &str) -> String {
    path.rsplit('/')
        .next()
        .and_then(|file| file.rsplit_once('.').map(|(stem, _)| stem))
        .unwrap_or(path)
        .to_string()
}

fn normalize_relative_path(base_dir: &str, path: &str) -> anyhow::Result<String> {
    let mut components = base_dir
        .split('/')
        .filter(|component| !component.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    for component in path.split('/') {
        match component {
            "" | "." => {}
            ".." => {
                if components.pop().is_none() {
                    anyhow::bail!("local import path {path} escapes package root");
                }
            }
            value if value.contains('\\') => anyhow::bail!("invalid local import path {path}"),
            value => components.push(value.to_string()),
        }
    }
    if components.is_empty() {
        anyhow::bail!("local import path {path} resolved to package root");
    }
    Ok(components.join("/"))
}
