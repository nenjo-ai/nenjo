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
            .find(|agent| agent.id == id)
            .map(|agent| Slug::derive(&agent.name))
            .ok_or_else(|| anyhow!("agent not found: {id}"))
    }

    pub fn agent_id(&self, slug: &Slug) -> Result<Uuid> {
        self.manifest
            .agents
            .iter()
            .find(|agent| Slug::derive(&agent.name) == *slug)
            .map(|agent| agent.id)
            .ok_or_else(|| anyhow!("agent not found: {slug}"))
    }

    pub fn routine(&self, id: Uuid) -> Result<Slug> {
        self.manifest
            .routines
            .iter()
            .find(|routine| routine.id == id)
            .map(|routine| Slug::derive(&routine.name))
            .ok_or_else(|| anyhow!("routine not found: {id}"))
    }

    pub fn routine_id(&self, slug: &Slug) -> Result<Uuid> {
        self.manifest
            .routines
            .iter()
            .find(|routine| Slug::derive(&routine.name) == *slug)
            .map(|routine| routine.id)
            .ok_or_else(|| anyhow!("routine not found: {slug}"))
    }

    pub fn project(&self, id: Uuid) -> Result<Option<Slug>> {
        if id.is_nil() {
            return Ok(None);
        }
        self.manifest
            .projects
            .iter()
            .find(|project| project.id == id)
            .map(|project| Some(project.slug.clone()))
            .ok_or_else(|| anyhow!("project not found: {id}"))
    }

    pub fn project_id(&self, slug: &Slug) -> Result<Uuid> {
        self.manifest
            .projects
            .iter()
            .find(|project| project.slug == *slug)
            .map(|project| project.id)
            .ok_or_else(|| anyhow!("project not found: {slug}"))
    }
}
