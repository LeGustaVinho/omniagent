use serde::Serialize;
use ts_rs::TS;

use crate::{conversations::SessionScope, runner::ExecutionPlatform};

use super::{Provider, SessionMode};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "../../src/bindings/")]
pub enum OsSandboxSupport {
    Unsupported,
    PrerequisitesMet,
    Unverified,
}

pub(crate) fn support(
    provider: Provider,
    platform: Option<&ExecutionPlatform>,
) -> OsSandboxSupport {
    use OsSandboxSupport::*;
    match provider {
        // The native Claude Code executable has no OS command sandbox.
        Provider::Claude => Unsupported,
        // Codex's Windows implementations depend on device policy and setup.
        Provider::Codex => Unverified,
        Provider::Antigravity => {
            let Some(platform) = platform else {
                return Unverified;
            };
            let windows = platform.guest.as_ref().unwrap_or(&platform.host);
            if windows.server {
                return Unverified;
            }
            // Google requires Windows 11 24H2+ with the August 2026 update.
            // https://antigravity.google/docs/sandbox?tab=cli
            // The monthly builds are release-family specific.
            // https://learn.microsoft.com/en-us/windows/release-health/windows11-release-information
            let min_revision = match windows.build {
                ..26100 => return Unsupported,
                26100 | 26200 => 9168,
                26300 => 9457,
                28000 => 2704,
                _ => return Unverified,
            };
            if windows.revision >= min_revision {
                PrerequisitesMet
            } else if matches!(windows.build, 26100 | 26200) && windows.revision >= 9106 {
                // The August Hotpatch build differs from the general update.
                Unverified
            } else {
                Unsupported
            }
        }
    }
}

pub(crate) fn admission_blocker(
    scope: &SessionScope,
    platform: Option<&ExecutionPlatform>,
    direct: bool,
) -> Option<&'static str> {
    if support(scope.provider, platform) == OsSandboxSupport::Unsupported {
        return Some(
            "The project rule requires an OS command sandbox that this CLI and Windows runner do not support",
        );
    }
    if direct && scope.provider == Provider::Antigravity && scope.mode == SessionMode::ApprovedWrite
    {
        let home = std::env::var_os("USERPROFILE").and_then(|home| dunce::canonicalize(home).ok());
        if home
            .as_ref()
            .is_none_or(|home| !scope.checkout.starts_with(home))
        {
            return Some(
                "Antigravity's Windows sandbox requires the writable checkout inside the Windows user profile",
            );
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::WindowsVersion;

    #[test]
    fn antigravity_support_uses_the_actual_windows_target_and_servicing_build() {
        let windows = |build, revision| WindowsVersion {
            major: 10,
            minor: 0,
            build,
            revision,
            native_architecture: 9,
            server: false,
        };
        use OsSandboxSupport::*;
        for (build, revision, expected) in [
            (19045, 6466, Unsupported),
            (22631, 9999, Unsupported),
            (26100, 8875, Unsupported),
            (26100, 9106, Unverified),
            (26100, 9168, PrerequisitesMet),
            (26200, 9168, PrerequisitesMet),
            (26300, 9457, PrerequisitesMet),
            (28000, 2704, PrerequisitesMet),
        ] {
            let platform = ExecutionPlatform {
                host: windows(build, revision),
                guest: None,
            };
            assert_eq!(
                support(Provider::Antigravity, Some(&platform)),
                expected,
                "{build}.{revision}"
            );
        }
        let platform = ExecutionPlatform {
            host: windows(26200, 9168),
            guest: Some(windows(19045, 6466)),
        };
        assert_eq!(support(Provider::Antigravity, Some(&platform)), Unsupported);
        assert_eq!(support(Provider::Claude, Some(&platform)), Unsupported);
        assert_eq!(support(Provider::Codex, Some(&platform)), Unverified);
    }
}
