//! Arch Linux libalpm/pacman placeholder. No system package operations implemented.
use super::{DriverKind, PackageDriver};
pub struct PacmanDriver;
impl PackageDriver for PacmanDriver {
    fn kind(&self) -> DriverKind { DriverKind::Pacman }
    fn artifact_extension(&self) -> &'static str { "pkg.tar.zst" }
    fn installed_database(&self) -> &'static str { "/var/lib/pacman/local" }
}
