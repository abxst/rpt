//! Debian/Ubuntu native package driver; read-only functionality only.
use super::{DriverKind, InstalledPackage, PackageDriver};
use anyhow::Result;
use std::{cmp::Ordering, collections::BTreeMap, path::Path};
use futures_util::future::BoxFuture;
pub struct DebDriver;
impl PackageDriver for DebDriver {
    fn kind(&self) -> DriverKind { DriverKind::Deb }
    fn artifact_extension(&self) -> &'static str { "deb" }
    fn installed_database(&self) -> &'static str { "/var/lib/dpkg/status" }
    fn compare_versions(&self, left: &str, right: &str) -> Result<Ordering> {
        crate::version::compare(left, right)
    }
    fn update_indexes<'a>(
        &'a self, root: &'a Path, output: &'a Path,
        opts: crate::update::UpdateOptions,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(crate::update::update(root, output, opts))
    }
    fn cache_versions(&self, cache: &Path, name: &str, arch: &str) -> Result<Vec<String>> {
        crate::cache_query::cache_versions(cache, name, arch)
    }
    fn policy_preview(
        &self, cache: &Path, status: &Path, config: &Path,
        name: &str, arch: &str,
    ) -> Result<crate::policy::PolicyPreview> {
        crate::policy::preview(cache, status, config, name, arch)
    }
    fn show_package(
        &self, cache: &Path, arch: &str, name: &str,
    ) -> Result<Option<crate::metadata_query::PackageSummary>> {
        crate::metadata_query::show(cache, arch, name)
    }
    fn search_packages(
        &self, cache: &Path, arch: &str, query: &str, limit: usize,
    ) -> Result<Vec<crate::metadata_query::PackageSummary>> {
        crate::metadata_query::search(cache, arch, query, limit)
    }
    fn list_packages(
        &self, cache: &Path, arch: &str, prefix: &str, limit: usize,
    ) -> Result<Vec<crate::metadata_query::PackageSummary>> {
        crate::metadata_query::list(cache, arch, prefix, limit)
    }
    fn reverse_dependencies(
        &self, cache: &Path, arch: &str, name: &str, limit: usize,
    ) -> Result<Vec<crate::metadata_query::PackageSummary>> {
        crate::metadata_query::rdepends(cache, arch, name, limit)
    }
    fn plan_install_preview(
        &self, cache: &Path, status: &Path, configuration: &Path,
        package: &str, architecture: &str, max_packages: usize,
    ) -> Result<Vec<crate::install_plan::PlanStep>> {
        crate::policy::ensure_no_custom_policy(configuration)?;
        crate::install_plan::preview(cache, status, package, architecture, max_packages)
    }
    fn prepare_download(
        &self, cache: &Path, names: &[String], architecture: &str,
    ) -> Result<crate::Manifest> {
        crate::package_provenance::prepare(cache, names, architecture)
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
