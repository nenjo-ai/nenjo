use anyhow::Result;

/// Heavy session content store for transcript bodies, traces, checkpoints, and
/// similar worker-owned artifacts.
pub trait SessionContentStore: Send + Sync {
    fn read_blob(&self, key: &str) -> Result<Option<Vec<u8>>>;

    fn write_blob(&self, key: &str, body: &[u8]) -> Result<()>;

    fn append_blob(&self, key: &str, body: &[u8]) -> Result<()>;

    fn delete_blob(&self, key: &str) -> Result<()>;
}
