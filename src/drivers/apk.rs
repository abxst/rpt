//! Alpine apk-tools placeholder. No system package operations are implemented.
use super::{DriverKind, PackageDriver};
pub struct ApkDriver;
impl PackageDriver for ApkDriver {
    fn kind(&self) -> DriverKind { DriverKind::Apk }
    fn artifact_extension(&self) -> &'static str { "apk" }
    fn installed_database(&self) -> &'static str { "/lib/apk/db/installed" }
}
