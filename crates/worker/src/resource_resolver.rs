use anyhow::{Result, anyhow};
use nenjo::{Manifest, Slug};
use uuid::Uuid;

pub struct PlatformResourceResolver<'a> {
    manifest: &'a Manifest,
}

impl<'a> PlatformResourceResolver<'a> {
    pub fn new(manifest: &'a Manifest) -> Self {
        Self { manifest }
    }

    pub fn agent(&self, id: Uuid) -> Result<Slug> {
        self.manifest
            .agents
            .iter()
            .find(|agent| stable_resource_id("agent", &agent.slug) == id)
            .map(|agent| agent.slug.clone())
            .ok_or_else(|| anyhow!("agent not found: {id}"))
    }

    pub fn agent_id(&self, slug: &Slug) -> Result<Uuid> {
        self.manifest
            .agents
            .iter()
            .any(|agent| agent.slug == *slug)
            .then(|| stable_resource_id("agent", slug))
            .ok_or_else(|| anyhow!("agent not found: {slug}"))
    }

    pub fn routine(&self, id: Uuid) -> Result<Slug> {
        self.manifest
            .routines
            .iter()
            .find(|routine| stable_resource_id("routine", &routine.slug) == id)
            .map(|routine| routine.slug.clone())
            .ok_or_else(|| anyhow!("routine not found: {id}"))
    }

    pub fn routine_id(&self, slug: &Slug) -> Result<Uuid> {
        self.manifest
            .routines
            .iter()
            .any(|routine| routine.slug == *slug)
            .then(|| stable_resource_id("routine", slug))
            .ok_or_else(|| anyhow!("routine not found: {slug}"))
    }

    pub fn project(&self, id: Uuid) -> Result<Option<Slug>> {
        if id.is_nil() {
            return Ok(None);
        }
        self.manifest
            .projects
            .iter()
            .find(|project| stable_resource_id("project", &project.slug) == id)
            .map(|project| Some(project.slug.clone()))
            .ok_or_else(|| anyhow!("project not found: {id}"))
    }

    pub fn project_id(&self, slug: &Slug) -> Result<Uuid> {
        self.manifest
            .projects
            .iter()
            .any(|project| project.slug == *slug)
            .then(|| stable_resource_id("project", slug))
            .ok_or_else(|| anyhow!("project not found: {slug}"))
    }
}

pub fn stable_resource_id(kind: &str, slug: &Slug) -> Uuid {
    Uuid::new_v5(
        &Uuid::NAMESPACE_URL,
        format!("nenjo://resource/{kind}/{}", slug.as_str()).as_bytes(),
    )
}
