use crate::{Result, package_module_source_path, validate_source_path};

pub(crate) struct CommandContentPathCandidate {
    pub(crate) read_path: String,
    pub(crate) package_path: String,
}

pub(crate) fn command_content_path_candidates(
    package_path: &str,
    module_path: &str,
    module_source_path: &str,
    content_path: &str,
) -> Result<Vec<CommandContentPathCandidate>> {
    let content_path = validate_source_path(content_path)?;
    let mut candidates = Vec::new();
    push_unique(
        &mut candidates,
        content_path.clone(),
        package_relative_content_path(&content_path, module_path, module_source_path),
    );
    push_unique(
        &mut candidates,
        package_module_source_path(package_path, &content_path)?,
        content_path.clone(),
    );
    if let Some(stem_path) = module_stem_content_path(module_path, &content_path)? {
        push_unique(&mut candidates, stem_path.clone(), stem_path);
    }
    if let Some((dir, _)) = module_source_path.rsplit_once('/') {
        let module_relative_path = module_path
            .rsplit_once('/')
            .map(|(module_dir, _)| format!("{module_dir}/{content_path}"))
            .unwrap_or_else(|| content_path.clone());
        push_unique(
            &mut candidates,
            validate_source_path(&format!("{dir}/{content_path}"))?,
            validate_source_path(&module_relative_path)?,
        );
    }
    Ok(candidates)
}

fn module_stem_content_path(module_path: &str, content_path: &str) -> Result<Option<String>> {
    let Some((_, filename)) = content_path.rsplit_once('/') else {
        return Ok(None);
    };
    let module_stem = module_path
        .strip_suffix(".yaml")
        .or_else(|| module_path.strip_suffix(".yml"))
        .unwrap_or(module_path);
    Ok(Some(validate_source_path(&format!(
        "{module_stem}/{filename}"
    ))?))
}

fn package_relative_content_path(
    content_path: &str,
    module_path: &str,
    module_source_path: &str,
) -> String {
    let source_dir = module_source_path.rsplit_once('/').map(|(dir, _)| dir);
    let module_dir = module_path.rsplit_once('/').map(|(dir, _)| dir);
    match (source_dir, module_dir) {
        (Some(source_dir), Some(module_dir)) => content_path
            .strip_prefix(&format!("{source_dir}/"))
            .map(|suffix| format!("{module_dir}/{suffix}"))
            .unwrap_or_else(|| content_path.to_string()),
        _ => content_path.to_string(),
    }
}

fn push_unique(
    values: &mut Vec<CommandContentPathCandidate>,
    read_path: String,
    package_path: String,
) {
    if !values
        .iter()
        .any(|existing| existing.read_path == read_path)
    {
        values.push(CommandContentPathCandidate {
            read_path,
            package_path,
        });
    }
}
