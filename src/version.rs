use cargo_metadata::Package;
use semver::{Prerelease, Version};

use crate::Context;

#[derive(Clone)]
pub enum BumpSpec {
    Auto,
    Keep,
    Patch,
    Minor,
    Major,
    /// Increment existing pre-release counter. Errors if not already a pre-release.
    PreRelease,
}

impl std::fmt::Display for BumpSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            BumpSpec::Auto => "auto",
            BumpSpec::Keep => "no",
            BumpSpec::Patch => "patch",
            BumpSpec::Minor => "minor",
            BumpSpec::Major => "major",
            BumpSpec::PreRelease => "prerelease",
        })
    }
}

#[allow(clippy::ptr_arg)]
pub(crate) fn select_publishee_bump_spec(name: &String, ctx: &Context) -> BumpSpec {
    if ctx.crate_names.contains(name) {
        ctx.bump.clone()
    } else {
        ctx.bump_dependencies.clone()
    }
}

/// Returns true if this would be a breaking change for `v`.
fn bump_major_minor_patch(v: &mut semver::Version, bump_spec: BumpSpec) -> bool {
    use BumpSpec::*;
    match bump_spec {
        Major => {
            if !v.pre.is_empty() && v.minor == 0 && v.patch == 0 {
                // Graduate: e.g. 2.0.0-beta.1 → 2.0.0
                v.pre = Prerelease::EMPTY;
            } else {
                v.major += 1;
                v.minor = 0;
                v.patch = 0;
                v.pre = Prerelease::EMPTY;
            }
            true
        }
        Minor => {
            if !v.pre.is_empty() && v.patch == 0 {
                // Graduate: e.g. 1.1.0-beta.1 → 1.1.0
                v.pre = Prerelease::EMPTY;
            } else {
                v.minor += 1;
                v.patch = 0;
                v.pre = Prerelease::EMPTY;
            }
            is_pre_release(v)
        }
        Patch => {
            if !v.pre.is_empty() {
                // Graduate: e.g. 1.0.1-beta.1 → 1.0.1 or 1.0.0-beta.1 → 1.0.0
                v.pre = Prerelease::EMPTY;
            } else {
                v.patch += 1;
            }
            false
        }
        Keep | Auto | PreRelease => unreachable!("BUG: auto mode, keep, or pre-release are unsupported here"),
    }
}

#[derive(Clone, Debug)]
pub struct Bump {
    pub next_release: semver::Version,
    /// The current version of the crate as read from Cargo.toml.
    pub package_version: semver::Version,
    /// The latest released version of the package, as read from the crates-index.
    pub latest_release: Option<semver::Version>,
    /// The computed version, for example based on a user version bump or a computed version bump.
    pub desired_release: semver::Version,
}

impl Bump {
    pub(crate) fn next_release_changes_manifest(&self) -> bool {
        self.next_release > self.package_version
    }
    pub(crate) fn is_breaking(&self) -> bool {
        rhs_is_breaking_bump_for_lhs(&self.package_version, &self.next_release)
    }
}

pub(crate) fn bump_package_with_spec(
    package: &Package,
    bump_spec: BumpSpec,
    ctx: &Context,
    bump_when_needed: bool,
) -> anyhow::Result<Bump> {
    let mut v = package.version.clone();
    use BumpSpec::*;
    let package_version_must_be_breaking = match bump_spec {
        Major | Minor | Patch => {
            if ctx.pre_id.is_empty() {
                bump_major_minor_patch(&mut v, bump_spec)
            } else {
                // With --pre-id, always bump the base and set pre-release (npm premajor/preminor/prepatch)
                match bump_spec {
                    Major => {
                        v.major += 1;
                        v.minor = 0;
                        v.patch = 0;
                    }
                    Minor => {
                        v.minor += 1;
                        v.patch = 0;
                    }
                    Patch => {
                        v.patch += 1;
                    }
                    _ => unreachable!(),
                }
                v.pre = Prerelease::new(&format!("{}.0", ctx.pre_id)).expect("valid prerelease");
                // Pre-releases are not considered breaking in the same way
                false
            }
        }
        PreRelease => {
            if v.pre.is_empty() {
                anyhow::bail!(
                    "Cannot use 'prerelease' on stable version {}. \
                     Use '--bump major/minor/patch --pre-id <label>' to start a pre-release series.",
                    v
                );
            }
            let existing_label = extract_pre_label(&v);
            let label = if ctx.pre_id.is_empty() {
                existing_label.clone()
            } else {
                ctx.pre_id.clone()
            };
            if label == existing_label {
                // Same label: increment counter
                let n = extract_pre_number(&v);
                v.pre = Prerelease::new(&format!("{label}.{}", n + 1)).expect("valid prerelease");
            } else {
                // Different label: replace, reset to 0
                v.pre = Prerelease::new(&format!("{label}.0")).expect("valid prerelease");
            }
            false
        }
        Keep => false,
        Auto => {
            use anyhow::Context;
            let segments = crate::git::history::crate_ref_segments(
                package,
                ctx,
                ctx.history
                    .as_ref()
                    .context("Did not have access to the Git history - please assure to not be on a detached HEAD")?,
                crate::git::history::SegmentScope::Unreleased,
            )?;
            assert_eq!(
                segments.len(),
                1,
                "there should be exactly one section, the 'unreleased' one"
            );
            let unreleased = &segments[0];
            if unreleased.history.is_empty() {
                false
            } else if !v.pre.is_empty() {
                // In a pre-release series: always increment the pre-release number
                let label = extract_pre_label(&v);
                bump_pre_release(&mut v, &label);
                false
            } else if unreleased.history.iter().any(|item| item.message.breaking) {
                let is_breaking = if is_pre_release(&v) {
                    bump_major_minor_patch(&mut v, Minor)
                } else {
                    bump_major_minor_patch(&mut v, Major)
                };
                assert!(is_breaking, "BUG: breaking changes are…breaking :D");
                is_breaking
            } else if unreleased.history.iter().any(|item| item.message.kind == Some("feat")) {
                let is_breaking = if is_pre_release(&v) {
                    bump_major_minor_patch(&mut v, Patch)
                } else {
                    bump_major_minor_patch(&mut v, Minor)
                };
                assert!(!is_breaking, "BUG: new features are never breaking");
                is_breaking
            } else {
                let is_breaking = bump_major_minor_patch(&mut v, Patch);
                assert!(!is_breaking, "BUG: patch releases are never breaking");
                false
            }
        }
    };
    let desired_release = v;
    let (latest_release, next_release) = match ctx.crates_index.crate_(&package.name) {
        Some(published_crate) => {
            let latest_release = semver::Version::parse(published_crate.highest_version().version())
                .expect("valid version in crate index");
            let next_release = if latest_release >= desired_release {
                desired_release.clone()
            } else {
                let mut next_release = desired_release.clone();
                if bump_when_needed && package.version > latest_release && desired_release != package.version {
                    if package_version_must_be_breaking {
                        if rhs_is_breaking_bump_for_lhs(&latest_release, &package.version) {
                            next_release = package.version.clone();
                        }
                    } else {
                        next_release = package.version.clone();
                    };
                }
                next_release
            };
            (Some(latest_release), next_release)
        }
        None => (
            None,
            if bump_when_needed {
                package.version.clone()
            } else {
                desired_release.clone()
            },
        ),
    };
    Ok(Bump {
        next_release,
        package_version: package.version.clone(),
        desired_release,
        latest_release,
    })
}

pub(crate) fn bump_package(package: &Package, ctx: &Context, bump_when_needed: bool) -> anyhow::Result<Bump> {
    let bump_spec = select_publishee_bump_spec(&package.name, ctx);
    bump_package_with_spec(package, bump_spec, ctx, bump_when_needed)
}

pub(crate) fn is_pre_release(semver: &Version) -> bool {
    crate::utils::is_pre_release_version(semver)
}

/// Extract the label portion of a pre-release identifier (e.g. "beta" from "beta.2").
pub(crate) fn extract_pre_label(v: &Version) -> String {
    let pre = v.pre.as_str();
    match pre.rsplit_once('.') {
        Some((label, numeric)) if numeric.parse::<u64>().is_ok() => label.to_owned(),
        _ => pre.to_owned(),
    }
}

/// Extract the numeric suffix of a pre-release identifier (e.g. 2 from "beta.2").
/// Returns 0 if there is no numeric suffix.
fn extract_pre_number(v: &Version) -> u64 {
    let pre = v.pre.as_str();
    match pre.rsplit_once('.') {
        Some((_, numeric)) => numeric.parse::<u64>().unwrap_or(0),
        _ => 0,
    }
}

/// Bump the pre-release portion of `v` using `label`.
/// - If `v.pre` is empty, apply a minor bump first, then set pre to `{label}.1`.
/// - If `v.pre` starts with the same label, increment the numeric suffix.
/// - If `v.pre` has a different label, replace with `{label}.1`.
fn bump_pre_release(v: &mut semver::Version, label: &str) {
    if v.pre.is_empty() {
        v.minor += 1;
        v.patch = 0;
        v.pre = Prerelease::new(&format!("{label}.1")).expect("valid prerelease");
    } else {
        let pre = v.pre.as_str();
        match pre.rsplit_once('.') {
            Some((existing_label, numeric)) if numeric.parse::<u64>().is_ok() => {
                if existing_label == label {
                    let n: u64 = numeric.parse().unwrap();
                    v.pre = Prerelease::new(&format!("{label}.{}", n + 1)).expect("valid prerelease");
                } else {
                    v.pre = Prerelease::new(&format!("{label}.1")).expect("valid prerelease");
                }
            }
            _ => {
                // No numeric suffix, start at 1
                v.pre = Prerelease::new(&format!("{label}.1")).expect("valid prerelease");
            }
        }
    }
}

pub(crate) fn rhs_is_breaking_bump_for_lhs(lhs: &Version, rhs: &Version) -> bool {
    if !lhs.pre.is_empty() && !rhs.pre.is_empty() {
        // Different base version is breaking within pre-release
        rhs.major > lhs.major || rhs.minor > lhs.minor || rhs.patch > lhs.patch
    } else {
        rhs.major > lhs.major || rhs.minor > lhs.minor
    }
}

#[cfg(test)]
mod tests {
    use semver::Version;

    use super::*;

    mod bump_pre_release_fn {
        use super::*;

        #[test]
        fn from_stable_creates_new_pre_release() {
            let mut v = Version::parse("1.0.0").unwrap();
            bump_pre_release(&mut v, "beta");
            assert_eq!(v, Version::parse("1.1.0-beta.1").unwrap());
        }

        #[test]
        fn increments_same_label() {
            let mut v = Version::parse("1.1.0-beta.1").unwrap();
            bump_pre_release(&mut v, "beta");
            assert_eq!(v, Version::parse("1.1.0-beta.2").unwrap());
        }

        #[test]
        fn changes_label_resets_counter() {
            let mut v = Version::parse("1.1.0-beta.3").unwrap();
            bump_pre_release(&mut v, "rc");
            assert_eq!(v, Version::parse("1.1.0-rc.1").unwrap());
        }

        #[test]
        fn from_zero_x_stable() {
            let mut v = Version::parse("0.5.0").unwrap();
            bump_pre_release(&mut v, "alpha");
            assert_eq!(v, Version::parse("0.6.0-alpha.1").unwrap());
        }

        #[test]
        fn pre_without_numeric_suffix() {
            let mut v = Version::parse("1.0.0-beta").unwrap();
            bump_pre_release(&mut v, "beta");
            assert_eq!(v, Version::parse("1.0.0-beta.1").unwrap());
        }
    }

    mod graduation {
        use super::*;

        #[test]
        fn patch_on_pre_release_strips_pre() {
            let mut v = Version::parse("1.0.0-beta.1").unwrap();
            bump_major_minor_patch(&mut v, BumpSpec::Patch);
            assert_eq!(v, Version::parse("1.0.0").unwrap());
        }

        #[test]
        fn patch_on_pre_release_with_patch() {
            let mut v = Version::parse("1.0.1-beta.1").unwrap();
            bump_major_minor_patch(&mut v, BumpSpec::Patch);
            assert_eq!(v, Version::parse("1.0.1").unwrap());
        }

        #[test]
        fn minor_on_pre_release_strips_pre() {
            let mut v = Version::parse("1.1.0-beta.1").unwrap();
            bump_major_minor_patch(&mut v, BumpSpec::Minor);
            assert_eq!(v, Version::parse("1.1.0").unwrap());
        }

        #[test]
        fn minor_on_pre_release_with_patch_bumps_minor() {
            let mut v = Version::parse("1.0.1-beta.1").unwrap();
            bump_major_minor_patch(&mut v, BumpSpec::Minor);
            assert_eq!(v, Version::parse("1.1.0").unwrap());
        }

        #[test]
        fn major_on_pre_release_strips_pre() {
            let mut v = Version::parse("2.0.0-beta.1").unwrap();
            bump_major_minor_patch(&mut v, BumpSpec::Major);
            assert_eq!(v, Version::parse("2.0.0").unwrap());
        }

        #[test]
        fn major_on_pre_release_with_minor_bumps_major() {
            let mut v = Version::parse("1.1.0-beta.1").unwrap();
            bump_major_minor_patch(&mut v, BumpSpec::Major);
            assert_eq!(v, Version::parse("2.0.0").unwrap());
        }

        #[test]
        fn patch_on_stable_increments_patch() {
            let mut v = Version::parse("1.0.0").unwrap();
            bump_major_minor_patch(&mut v, BumpSpec::Patch);
            assert_eq!(v, Version::parse("1.0.1").unwrap());
        }
    }

    mod rhs_is_breaking {
        use super::*;

        #[test]
        fn same_pre_release_series_not_breaking() {
            let lhs = Version::parse("1.1.0-beta.1").unwrap();
            let rhs = Version::parse("1.1.0-beta.2").unwrap();
            assert!(!rhs_is_breaking_bump_for_lhs(&lhs, &rhs));
        }

        #[test]
        fn different_base_in_pre_release_is_breaking() {
            let lhs = Version::parse("1.1.0-beta.1").unwrap();
            let rhs = Version::parse("1.2.0-beta.1").unwrap();
            assert!(rhs_is_breaking_bump_for_lhs(&lhs, &rhs));
        }

        #[test]
        fn graduation_same_base_not_breaking() {
            let lhs = Version::parse("1.1.0-beta.1").unwrap();
            let rhs = Version::parse("1.1.0").unwrap();
            assert!(!rhs_is_breaking_bump_for_lhs(&lhs, &rhs));
        }

        #[test]
        fn graduation_higher_minor_is_breaking() {
            let lhs = Version::parse("1.0.0-beta.1").unwrap();
            let rhs = Version::parse("1.1.0").unwrap();
            assert!(rhs_is_breaking_bump_for_lhs(&lhs, &rhs));
        }

        #[test]
        fn stable_minor_bump_is_breaking() {
            let lhs = Version::parse("1.0.0").unwrap();
            let rhs = Version::parse("1.1.0").unwrap();
            assert!(rhs_is_breaking_bump_for_lhs(&lhs, &rhs));
        }

        #[test]
        fn stable_patch_bump_not_breaking() {
            let lhs = Version::parse("1.0.0").unwrap();
            let rhs = Version::parse("1.0.1").unwrap();
            assert!(!rhs_is_breaking_bump_for_lhs(&lhs, &rhs));
        }
    }

    mod pre_id_modifier {
        use super::*;

        /// Helper to simulate --pre-id modifier on major/minor/patch
        fn bump_with_pre_id(v: &mut Version, bump_spec: BumpSpec, pre_id: &str) {
            match bump_spec {
                BumpSpec::Major => {
                    v.major += 1;
                    v.minor = 0;
                    v.patch = 0;
                }
                BumpSpec::Minor => {
                    v.minor += 1;
                    v.patch = 0;
                }
                BumpSpec::Patch => {
                    v.patch += 1;
                }
                _ => unreachable!(),
            }
            v.pre = Prerelease::new(&format!("{pre_id}.0")).unwrap();
        }

        #[test]
        fn major_from_stable() {
            let mut v = Version::parse("1.2.3").unwrap();
            bump_with_pre_id(&mut v, BumpSpec::Major, "beta");
            assert_eq!(v, Version::parse("2.0.0-beta.0").unwrap());
        }

        #[test]
        fn minor_from_stable() {
            let mut v = Version::parse("1.2.3").unwrap();
            bump_with_pre_id(&mut v, BumpSpec::Minor, "beta");
            assert_eq!(v, Version::parse("1.3.0-beta.0").unwrap());
        }

        #[test]
        fn patch_from_stable() {
            let mut v = Version::parse("1.2.3").unwrap();
            bump_with_pre_id(&mut v, BumpSpec::Patch, "beta");
            assert_eq!(v, Version::parse("1.2.4-beta.0").unwrap());
        }

        #[test]
        fn major_from_pre_release() {
            let mut v = Version::parse("2.0.0-beta.3").unwrap();
            bump_with_pre_id(&mut v, BumpSpec::Major, "beta");
            assert_eq!(v, Version::parse("3.0.0-beta.0").unwrap());
        }

        #[test]
        fn minor_from_pre_release() {
            let mut v = Version::parse("1.3.0-beta.2").unwrap();
            bump_with_pre_id(&mut v, BumpSpec::Minor, "beta");
            assert_eq!(v, Version::parse("1.4.0-beta.0").unwrap());
        }

        #[test]
        fn patch_from_pre_release() {
            let mut v = Version::parse("1.2.4-beta.1").unwrap();
            bump_with_pre_id(&mut v, BumpSpec::Patch, "rc");
            assert_eq!(v, Version::parse("1.2.5-rc.0").unwrap());
        }
    }

    mod extract_label {
        use super::*;

        #[test]
        fn extracts_beta_from_beta_2() {
            let v = Version::parse("1.0.0-beta.2").unwrap();
            assert_eq!(extract_pre_label(&v), "beta");
        }

        #[test]
        fn extracts_rc_from_rc_1() {
            let v = Version::parse("1.0.0-rc.1").unwrap();
            assert_eq!(extract_pre_label(&v), "rc");
        }

        #[test]
        fn returns_whole_pre_without_numeric() {
            let v = Version::parse("1.0.0-beta").unwrap();
            assert_eq!(extract_pre_label(&v), "beta");
        }

        #[test]
        fn handles_dotted_label() {
            let v = Version::parse("1.0.0-pre.beta.3").unwrap();
            assert_eq!(extract_pre_label(&v), "pre.beta");
        }
    }

    mod extract_number {
        use super::*;

        #[test]
        fn extracts_number() {
            let v = Version::parse("1.0.0-beta.2").unwrap();
            assert_eq!(extract_pre_number(&v), 2);
        }

        #[test]
        fn returns_zero_without_numeric() {
            let v = Version::parse("1.0.0-beta").unwrap();
            assert_eq!(extract_pre_number(&v), 0);
        }

        #[test]
        fn extracts_from_dotted_label() {
            let v = Version::parse("1.0.0-pre.beta.5").unwrap();
            assert_eq!(extract_pre_number(&v), 5);
        }
    }
}
