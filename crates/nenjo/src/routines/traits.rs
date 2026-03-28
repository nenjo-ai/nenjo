//! Traits for pluggable routine subsystems.

use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;

/// Output from a lambda script execution.
#[derive(Debug, Clone)]
pub struct LambdaOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

/// Trait for executing lambda scripts (deterministic, non-LLM steps).
///
/// The harness provides a `NativeRuntime` implementation that shells out
/// to the local OS. Other backends (Docker, WASM) can implement this trait.
#[async_trait]
pub trait LambdaRunner: Send + Sync {
    /// Execute a script and return its output.
    async fn run_script(
        &self,
        script_path: &Path,
        interpreter: &str,
        env: HashMap<String, String>,
        timeout: Duration,
    ) -> Result<LambdaOutput>;
}
