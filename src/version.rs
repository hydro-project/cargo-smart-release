use cargo_metadata::Package;
use semver::{Prerelease, Version};

use crate::Context;

#[derive(Clone, Copy)]
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
        ctx.bump
    } else {
        ctx.bump_dependencies
    }
}

/// Bump major/minor/patch version. When `pre_id` is non-empty, always bumps the base
/// and sets a pre-release identifier. When empty, may graduate (strip pre) if appropriate.
/// Returns true if this would be a breaking change for `v`.
fn bump_major_minor_patch(v: &mut semver::Version, bump_spec: BumpSpec, pre_id: &str) -> bool {
    use BumpSpec::*;
    match bump_spec {
        Major => {
            if pre_id.is_empty() && !v.pre.is_empty() && v.minor == 0 && v.patch == 0 {
                // Graduate: e.g. 2.0.0-beta.1 → 2.0.0
                v.pre = Prerelease::EMPTY;
            } else {
                v.major += 1;
                v.minor = 0;
                v.patch = 0;
                v.pre = Prerelease::EMPTY;
            }
        }
        Minor => {
            if pre_id.is_empty() && !v.pre.is_empty() && v.patch == 0 {
                // Graduate: e.g. 1.1.0-beta.1 → 1.1.0
                v.pre = Prerelease::EMPTY;
            } else {
                v.minor += 1;
                v.patch = 0;
                v.pre = Prerelease::EMPTY;
            }
        }
        Patch => {
            if pre_id.is_empty() && !v.pre.is_empty() {
                // Graduate: e.g. 1.0.1-beta.1 → 1.0.1
                v.pre = Prerelease::EMPTY;
            } else {
                v.patch += 1;
                v.pre = Prerelease::EMPTY;
            }
        }
        Keep | Auto | PreRelease => unreachable!("BUG: auto mode, keep, or pre-release are unsupported here"),
    }

    if !pre_id.is_empty() {
        v.pre = Prerelease::new(&format!("{pre_id}.0")).expect("valid prerelease");
        false
    } else {
        match bump_spec {
            Major => true,
            Minor => is_pre_release(v),
            Patch => false,
            _ => unreachable!(),
        }
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
    /// Returns true if the version bump produces a pre-release that existing caret
    /// requirements on `package_version` cannot match. This happens when `next_release`
    /// is pre-release and has a different major.minor.patch base than `package_version`.
    pub(crate) fn is_pre_release_incompatible(&self) -> bool {
        let lhs = &self.package_version;
        let rhs = &self.next_release;
        !rhs.pre.is_empty() && (lhs.major != rhs.major || lhs.minor != rhs.minor || lhs.patch != rhs.patch)
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
        Major | Minor | Patch => bump_major_minor_patch(&mut v, bump_spec, &ctx.pre_id),
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
            let n = if label == existing_label {
                extract_pre_number(&v) + 1
            } else {
                0
            };
            v.pre = Prerelease::new(&format!("{label}.{n}")).expect("valid prerelease");
            false
        }
        Keep => false,
        Auto => {
            use anyhow::Context;
            let history_ref = ctx
                .history
                .as_ref()
                .context("Did not have access to the Git history - please assure to not be on a detached HEAD")?;

            if !ctx.pre_id.is_empty() {
                // Auto + pre-id: compute base from commits since last stable release
                let segments = crate::git::history::crate_ref_segments(
                    package,
                    ctx,
                    history_ref,
                    crate::git::history::SegmentScope::UnreleasedSinceStable,
                )?;
                assert!(
                    !segments.is_empty(),
                    "there should be at least one section when using UnreleasedSinceStable"
                );
                let all_commits = &segments[0];
                if all_commits.history.is_empty() {
                    false
                } else {
                    let label = &ctx.pre_id;

                    // Compute target base from last stable version
                    let last_stable = find_last_stable_version(package, ctx);

                    // Determine target base from commit history severity,
                    // respecting 0.x semver conventions (breaking = minor for 0.x crates)
                    let base_level = if all_commits.history.iter().any(|item| item.message.breaking) {
                        if is_pre_release(&last_stable) {
                            Minor
                        } else {
                            Major
                        }
                    } else if all_commits.history.iter().any(|item| item.message.kind == Some("feat")) {
                        if is_pre_release(&last_stable) {
                            Patch
                        } else {
                            Minor
                        }
                    } else {
                        Patch
                    };

                    let mut target_base = last_stable.clone();
                    bump_major_minor_patch(&mut target_base, base_level, "");

                    if v.major == target_base.major && v.minor == target_base.minor && v.patch == target_base.patch {
                        // Base already correct — increment counter
                        let existing_label = extract_pre_label(&v);
                        if !v.pre.is_empty() && existing_label == *label {
                            let n = extract_pre_number(&v);
                            v.pre = Prerelease::new(&format!("{label}.{}", n + 1)).expect("valid prerelease");
                        } else {
                            v.major = target_base.major;
                            v.minor = target_base.minor;
                            v.patch = target_base.patch;
                            v.pre = Prerelease::new(&format!("{label}.0")).expect("valid prerelease");
                        }
                    } else {
                        // Escalate base, reset counter
                        v.major = target_base.major;
                        v.minor = target_base.minor;
                        v.patch = target_base.patch;
                        v.pre = Prerelease::new(&format!("{label}.0")).expect("valid prerelease");
                    }
                    false
                }
            } else {
                // Standard auto mode: compute from commits since last tag
                let segments = crate::git::history::crate_ref_segments(
                    package,
                    ctx,
                    history_ref,
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
                    // Already in a pre-release without --pre-id: graduate to stable
                    v.pre = Prerelease::EMPTY;
                    false
                } else if unreleased.history.iter().any(|item| item.message.breaking) {
                    let is_breaking = if is_pre_release(&v) {
                        bump_major_minor_patch(&mut v, Minor, "")
                    } else {
                        bump_major_minor_patch(&mut v, Major, "")
                    };
                    assert!(is_breaking, "BUG: breaking changes are…breaking :D");
                    is_breaking
                } else if unreleased.history.iter().any(|item| item.message.kind == Some("feat")) {
                    let is_breaking = if is_pre_release(&v) {
                        bump_major_minor_patch(&mut v, Patch, "")
                    } else {
                        bump_major_minor_patch(&mut v, Minor, "")
                    };
                    assert!(!is_breaking, "BUG: new features are never breaking");
                    is_breaking
                } else {
                    let is_breaking = bump_major_minor_patch(&mut v, Patch, "");
                    assert!(!is_breaking, "BUG: patch releases are never breaking");
                    false
                }
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

/// Find the last stable (non-pre-release) version for a package.
/// Checks the crates index first, falls back to stripping pre from current version,
/// or 0.0.0 if no stable version exists.
fn find_last_stable_version(package: &Package, ctx: &Context) -> Version {
    if let Some(published_crate) = ctx.crates_index.crate_(&package.name) {
        // Find highest version without a pre-release identifier
        let stable = published_crate
            .versions()
            .iter()
            .filter_map(|v| semver::Version::parse(v.version()).ok())
            .filter(|v| v.pre.is_empty())
            .max();
        if let Some(v) = stable {
            return v;
        }
    }
    // Fallback: if current version has a pre, strip it; otherwise use it as-is
    let mut fallback = package.version.clone();
    if !fallback.pre.is_empty() {
        fallback.pre = Prerelease::EMPTY;
        // The base was already bumped when entering pre-release, so "un-bump" by using
        // a zero version if this is truly the first release
        if fallback.major == 0 && fallback.minor == 0 && fallback.patch == 0 {
            return fallback;
        }
        // Can't reliably un-bump, so use the current base as the floor
        // This means the first auto --pre-id on an already-pre version may not escalate correctly
        // but it's the best we can do without published history
        return fallback;
    }
    fallback
}

pub(crate) fn is_pre_release(semver: &Version) -> bool {
    crate::utils::is_pre_release_version(semver)
}

/// Extract the label portion of a pre-release identifier (e.g. "beta" from "beta.2").
pub(crate) fn extract_pre_label(v: &Version) -> String {
    let pre = v.pre.as_str();
    if let Some((label, numeric)) = pre.rsplit_once('.') {
        if numeric.parse::<u64>().is_ok() {
            return label.to_owned();
        }
    }
    pre.to_owned()
}

/// Extract the numeric suffix of a pre-release identifier (e.g. 2 from "beta.2").
/// Panics if the last dot-separated segment exists but is not a valid number.
/// Returns 0 if there is no dot separator (e.g. "beta").
fn extract_pre_number(v: &Version) -> u64 {
    let pre = v.pre.as_str();
    if let Some((_, numeric)) = pre.rsplit_once('.') {
        numeric
            .parse::<u64>()
            .unwrap_or_else(|_| panic!("BUG: expected numeric pre-release suffix, got '{numeric}' in '{pre}'"))
    } else {
        0
    }
}

/// Returns true if a crate moving from version `lhs` to version `rhs` represents a
/// breaking change for its dependents.
///
/// For stable versions: major or minor increases are breaking.
/// For pre-release pairs: any base version component increase (including patch) is breaking,
/// since pre-releases within the same base are expected to share an API contract.
///
/// Label changes (e.g. beta→rc) at the same base are NOT breaking — they represent
/// maturity progression, not API changes. Cargo's strict pre-release matching (`^1.0.0-beta.0`
/// won't match `1.0.0-rc.0`) is handled separately by force-updating dependency requirements
/// in manifest.rs.
pub(crate) fn rhs_is_breaking_bump_for_lhs(lhs: &Version, rhs: &Version) -> bool {
    let both_pre = !lhs.pre.is_empty() && !rhs.pre.is_empty();
    rhs.major > lhs.major || rhs.minor > lhs.minor || (both_pre && rhs.patch > lhs.patch)
}

#[cfg(test)]
mod tests {
    use semver::Version;

    use super::*;

    mod graduation {
        use super::*;

        #[test]
        fn patch_on_pre_release_strips_pre() {
            let mut v = Version::parse("1.0.0-beta.1").unwrap();
            bump_major_minor_patch(&mut v, BumpSpec::Patch, "");
            assert_eq!(v, Version::parse("1.0.0").unwrap());
        }

        #[test]
        fn patch_on_pre_release_with_patch() {
            let mut v = Version::parse("1.0.1-beta.1").unwrap();
            bump_major_minor_patch(&mut v, BumpSpec::Patch, "");
            assert_eq!(v, Version::parse("1.0.1").unwrap());
        }

        #[test]
        fn minor_on_pre_release_strips_pre() {
            let mut v = Version::parse("1.1.0-beta.1").unwrap();
            bump_major_minor_patch(&mut v, BumpSpec::Minor, "");
            assert_eq!(v, Version::parse("1.1.0").unwrap());
        }

        #[test]
        fn minor_on_pre_release_with_patch_bumps_minor() {
            let mut v = Version::parse("1.0.1-beta.1").unwrap();
            bump_major_minor_patch(&mut v, BumpSpec::Minor, "");
            assert_eq!(v, Version::parse("1.1.0").unwrap());
        }

        #[test]
        fn major_on_pre_release_strips_pre() {
            let mut v = Version::parse("2.0.0-beta.1").unwrap();
            bump_major_minor_patch(&mut v, BumpSpec::Major, "");
            assert_eq!(v, Version::parse("2.0.0").unwrap());
        }

        #[test]
        fn major_on_pre_release_with_minor_bumps_major() {
            let mut v = Version::parse("1.1.0-beta.1").unwrap();
            bump_major_minor_patch(&mut v, BumpSpec::Major, "");
            assert_eq!(v, Version::parse("2.0.0").unwrap());
        }

        #[test]
        fn patch_on_stable_increments_patch() {
            let mut v = Version::parse("1.0.0").unwrap();
            bump_major_minor_patch(&mut v, BumpSpec::Patch, "");
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

    mod pre_release_incompatible {
        use super::*;

        fn bump(from: &str, to: &str) -> Bump {
            Bump {
                package_version: Version::parse(from).unwrap(),
                next_release: Version::parse(to).unwrap(),
                latest_release: None,
                desired_release: Version::parse(to).unwrap(),
            }
        }

        #[test]
        fn stable_to_pre_release_different_patch() {
            assert!(bump("0.1.0", "0.1.1-alpha.0").is_pre_release_incompatible());
        }

        #[test]
        fn stable_to_pre_release_different_minor() {
            assert!(bump("1.0.0", "1.1.0-alpha.0").is_pre_release_incompatible());
        }

        #[test]
        fn stable_to_pre_release_different_major() {
            assert!(bump("1.0.0", "2.0.0-alpha.0").is_pre_release_incompatible());
        }

        #[test]
        fn pre_to_pre_same_base_is_compatible() {
            assert!(!bump("1.0.0-beta.0", "1.0.0-beta.1").is_pre_release_incompatible());
        }

        #[test]
        fn pre_to_pre_same_base_different_label_is_compatible() {
            assert!(!bump("1.0.0-beta.0", "1.0.0-rc.0").is_pre_release_incompatible());
        }

        #[test]
        fn pre_to_pre_different_base_is_incompatible() {
            assert!(bump("0.1.1-alpha.0", "0.1.2-alpha.0").is_pre_release_incompatible());
        }

        #[test]
        fn pre_to_stable_graduation_is_compatible() {
            assert!(!bump("1.0.0-beta.0", "1.0.0").is_pre_release_incompatible());
        }

        #[test]
        fn stable_to_stable_is_compatible() {
            assert!(!bump("1.0.0", "1.0.1").is_pre_release_incompatible());
            assert!(!bump("1.0.0", "1.1.0").is_pre_release_incompatible());
            assert!(!bump("0.1.0", "0.1.1").is_pre_release_incompatible());
        }
    }

    mod pre_id_modifier {
        use super::*;

        #[test]
        fn major_from_stable() {
            let mut v = Version::parse("1.2.3").unwrap();
            bump_major_minor_patch(&mut v, BumpSpec::Major, "beta");
            assert_eq!(v, Version::parse("2.0.0-beta.0").unwrap());
        }

        #[test]
        fn minor_from_stable() {
            let mut v = Version::parse("1.2.3").unwrap();
            bump_major_minor_patch(&mut v, BumpSpec::Minor, "beta");
            assert_eq!(v, Version::parse("1.3.0-beta.0").unwrap());
        }

        #[test]
        fn patch_from_stable() {
            let mut v = Version::parse("1.2.3").unwrap();
            bump_major_minor_patch(&mut v, BumpSpec::Patch, "beta");
            assert_eq!(v, Version::parse("1.2.4-beta.0").unwrap());
        }

        #[test]
        fn major_from_pre_release() {
            let mut v = Version::parse("2.0.0-beta.3").unwrap();
            bump_major_minor_patch(&mut v, BumpSpec::Major, "beta");
            assert_eq!(v, Version::parse("3.0.0-beta.0").unwrap());
        }

        #[test]
        fn minor_from_pre_release() {
            let mut v = Version::parse("1.3.0-beta.2").unwrap();
            bump_major_minor_patch(&mut v, BumpSpec::Minor, "beta");
            assert_eq!(v, Version::parse("1.4.0-beta.0").unwrap());
        }

        #[test]
        fn patch_from_pre_release() {
            let mut v = Version::parse("1.2.4-beta.1").unwrap();
            bump_major_minor_patch(&mut v, BumpSpec::Patch, "rc");
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
