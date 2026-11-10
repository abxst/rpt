//! Debian/Ubuntu native package driver; read-only functionality only.
use super::{DriverKind, InstalledPackage, PackageDriver};
use anyhow::Result;
use std::{cmp::Ordering, collections::BTreeMap, path::Path};
pub struct DebDriver;
impl PackageDriver for DebDriver {
    fn kind(&self) -> DriverKind { DriverKind::Deb }
    fn artifact_extension(&self) -> &'static str { "deb" }
    fn installed_database(&self) -> &'static str { "/var/lib/dpkg/status" }
    fn compare_versions(&self, left: &str, right: &str) -> Result<Ordering> {
        crate::version::compare(left, right)
    }
    fn installed_packages(&self, status: &Path, arch: &str) -> Result<BTreeMap<String, InstalledPackage>> {
        let catalog = crate::cache_query::installed_catalog(status, arch)?;
        Ok(catalog.into_iter().map(|(name, version)| (
            name.clone(),
            InstalledPackage {
                name, version, architecture: arch.to_owned(),
                state: "install ok installed".to_owned(),
            },
        )).collect())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn debian_version_is_not_lexicographic() {
        assert_eq!(DebDriver.compare_versions("1.0~rc1", "1.0").unwrap(), Ordering::Less);
    }
}
