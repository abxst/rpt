//! RPM-family placeholder (Fedora, RHEL, Rocky, AlmaLinux).
//! Not a DNF/YUM or librpm implementation. All package operations fail closed.
use super::{DriverKind, PackageDriver};
pub struct RpmDriver;
impl PackageDriver for RpmDriver {
    fn kind(&self) -> DriverKind { DriverKind::Rpm }
    fn artifact_extension(&self) -> &'static str { "rpm" }
    fn installed_database(&self) -> &'static str { "/var/lib/rpm" }
}
