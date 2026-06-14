use crate::manifest_contract::parse_library_pack_slug;
use nenjo::Slug;
use serde::Deserialize;

pub(crate) fn deserialize_library_pack_slug<'de, D>(deserializer: D) -> Result<Slug, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = String::deserialize(deserializer)?;
    parse_library_pack_slug(&value).map_err(serde::de::Error::custom)
}
