# Pre-release Bump Redesign

## Problem

The current `--bump pre:<label>` always hardcodes a **minor** bump when entering
a pre-release from a stable version. There's no way to express "I want a
pre-release for the next major" vs "the next patch."

Additionally, there's no way to do automated weekly pre-releases where the
base version (major/minor/patch) is derived from commit history relative to
the last *stable* release, not the last pre-release.

## Proposed Design (npm-aligned)

### New CLI Interface

```
cargo smart-release --bump premajor --pre-id beta
cargo smart-release --bump preminor --pre-id beta
cargo smart-release --bump prepatch --pre-id beta
cargo smart-release --bump prerelease --pre-id beta
cargo smart-release --bump auto --pre-id beta       # auto-compute base, output pre-release
```

**New flag:**

```
--pre-id <LABEL>    Pre-release identifier label (e.g. "beta", "alpha", "rc").
                    Defaults to "rc" if not specified.
                    With premajor/preminor/prepatch/prerelease: sets the label.
                    With auto: switches auto mode to output pre-releases instead
                    of stable versions.
```

**Explicit bump spec values:**

| Spec | From stable `1.2.3` | From pre `1.3.0-beta.2` |
|------|---------------------|--------------------------|
| `premajor` | `2.0.0-beta.0` | `2.0.0-beta.0` |
| `preminor` | `1.3.0-beta.0` | `1.4.0-beta.0` |
| `prepatch` | `1.2.4-beta.0` | `1.3.1-beta.0` |
| `prerelease` | ERROR | `1.3.0-beta.3` |

**Behavior details (following npm):**

- `premajor`: Bump major, zero minor+patch, set pre to `{label}.0`. Always bumps base regardless of current state.
- `preminor`: Bump minor, zero patch, set pre to `{label}.0`. Always bumps base.
- `prepatch`: Bump patch, set pre to `{label}.0`. Always bumps base.
- `prerelease`: Error if not already a pre-release. If same label, increment counter. If different label, replace label, reset to `.0`.

**Starting number:** `.0` (following npm convention).

### Auto + Pre-Id: The Weekly Pre-release Workflow

```
cargo smart-release --bump auto --pre-id beta
```

This enables automated pre-releases where the base version is computed from
conventional commits since the **last stable release**, not the last pre-release.

**Example workflow:**
1. Last stable: `1.0.0`
2. Week 1: fixes land → `1.0.1-beta.0`
3. Week 2: a feature lands → `1.1.0-beta.0` (escalated based on commits since `1.0.0`)
4. Week 3: breaking change → `2.0.0-beta.0` (escalated again)
5. Week 4: more fixes → `2.0.0-beta.1` (base already correct, just increment)
6. Graduate: `cargo smart-release --bump major` → `2.0.0`

**Key insight:** The base version must be computed from commits since the last
*stable* release, not the last pre-release. Otherwise you'd get compounding bumps —
a breaking change on top of `2.0.0-beta.0` would give `3.0.0-beta.0`, which is
wrong — it should stay `2.0.0-beta.1` because the break was already accounted for.

**Algorithm:**
1. Find all commits since the last **stable** tag (skipping pre-release tags)
2. Compute target base version from commit severity (breaking→major, feat→minor, else→patch)
3. Apply that bump to the last stable version to get the target base
4. Compare target base to the current version's base (major.minor.patch):
   - If current base already matches → increment pre-release counter
   - If target base is higher → set new base, reset counter to `.0`

### BumpSpec Enum Changes

```rust
#[derive(Clone)]
pub enum BumpSpec {
    Auto,
    Keep,
    Patch,
    Minor,
    Major,
    PreMajor,
    PreMinor,
    PrePatch,
    PreRelease,
}
```

The label moves **out** of the enum into `Context`:

```rust
pub struct Context {
    // ... existing fields ...
    pub bump: BumpSpec,
    pub bump_dependencies: BumpSpec,
    pub pre_id: String,  // NEW — empty string means "no pre-id specified"
}
```

This is cleaner because:
1. The label is orthogonal to the bump level — it shouldn't be encoded in every variant.
2. `auto + --pre-id` reuses the same `Auto` variant, behavior changes based on `pre_id` presence.

## Implementation Details

### `SegmentScope::UnreleasedSinceStable`

The current `SegmentScope::Unreleased` stops at the first tag it encounters
(pre-release or stable). A new variant skips pre-release tags:

```rust
pub enum SegmentScope {
    Unreleased,
    UnreleasedSinceStable,  // NEW: skip pre-release tags, stop at stable
    EntireHistory,
}
```

In `crate_ref_segments`, when walking commits and hitting a tag:

```rust
SegmentScope::UnreleasedSinceStable => {
    let tag_version = parse_possibly_prefixed_tag_version(
        tag_prefix.as_deref(),
        strip_tag_path(next_ref.name.as_ref()),
    );
    if tag_version.map_or(false, |v| !v.pre.is_empty()) {
        // Pre-release tag — keep accumulating
        add_item_if_package_changed(ctx, &mut segment, &mut filter, item, data)?;
    } else {
        // Stable tag — stop here
        segments.push(segment);
        return Ok(segments);
    }
}
```

### Auto + Pre-Id Logic in `bump_package_with_spec`

```rust
Auto if !ctx.pre_id.is_empty() => {
    let segments = crate_ref_segments(
        package, ctx, history, SegmentScope::UnreleasedSinceStable
    )?;
    let all_commits = &segments[0];

    if all_commits.history.is_empty() {
        false // nothing to release
    } else {
        let base_level = if all_commits.history.iter().any(|i| i.message.breaking) {
            Major
        } else if all_commits.history.iter().any(|i| i.message.kind == Some("feat")) {
            Minor
        } else {
            Patch
        };

        let last_stable = find_last_stable_version(package, ctx);
        let mut target_base = last_stable.clone();
        bump_major_minor_patch(&mut target_base, base_level);

        let label = &ctx.pre_id;
        if v.major == target_base.major && v.minor == target_base.minor && v.patch == target_base.patch {
            // Base already correct — increment counter
            if v.pre.is_empty() {
                v.pre = Prerelease::new(&format!("{label}.0")).unwrap();
            } else {
                let existing_label = extract_pre_label(&v);
                if existing_label == *label {
                    let n = extract_pre_number(&v);
                    v.pre = Prerelease::new(&format!("{label}.{}", n + 1)).unwrap();
                } else {
                    v.pre = Prerelease::new(&format!("{label}.0")).unwrap();
                }
            }
        } else {
            // Escalate base, reset counter
            v.major = target_base.major;
            v.minor = target_base.minor;
            v.patch = target_base.patch;
            v.pre = Prerelease::new(&format!("{label}.0")).unwrap();
        }
        false
    }
}
```

### `prerelease` Errors on Stable

```rust
PreRelease => {
    if v.pre.is_empty() {
        bail!(
            "Cannot use 'prerelease' on stable version {}. \
             Use 'premajor', 'preminor', or 'prepatch' to start a pre-release series.",
            v
        );
    }
    let existing_label = extract_pre_label(&v);
    let label = if ctx.pre_id.is_empty() { &existing_label } else { &ctx.pre_id };
    if *label == existing_label {
        let n = extract_pre_number(&v);
        v.pre = Prerelease::new(&format!("{label}.{}", n + 1)).unwrap();
    } else {
        v.pre = Prerelease::new(&format!("{label}.0")).unwrap();
    }
    false
}
```

### `breaking_version_bump()` Changes

```rust
fn breaking_version_bump(ctx: &Context, package: &Package, bump_when_needed: bool) -> anyhow::Result<Bump> {
    let breaking_spec = if !package.version.pre.is_empty() {
        BumpSpec::PreRelease  // stays in pre-release series
    } else if is_pre_release_version(&package.version) {
        BumpSpec::Minor       // 0.x → bump minor
    } else {
        BumpSpec::Major       // stable → bump major
    };
    version::bump_package_with_spec(package, breaking_spec, ctx, bump_when_needed)
}
```

When `PreRelease` is used here, the label is resolved from the existing version's
pre-release identifier (via `extract_pre_label`), falling back to `ctx.pre_id`.

### CLI Parsing

```rust
fn to_bump_spec(spec: &str) -> anyhow::Result<BumpSpec> {
    use BumpSpec::*;
    Ok(match spec {
        "patch" | "Patch" => Patch,
        "minor" | "Minor" => Minor,
        "major" | "Major" => Major,
        "keep" | "Keep" => Keep,
        "auto" | "Auto" => Auto,
        "premajor" => PreMajor,
        "preminor" => PreMinor,
        "prepatch" => PrePatch,
        "prerelease" => PreRelease,
        unknown => bail!("Unknown bump specification: {:?}", unknown),
    })
}
```

### Label Resolution Order

When any pre-release operation needs a label:

1. `ctx.pre_id` if non-empty
2. Existing label from the version (via `extract_pre_label`) — for `prerelease`
   and `breaking_version_bump` where we want to stay in-series
3. Default `"rc"` as final fallback

## Summary of File Changes

| File | Changes |
|------|---------|
| `src/git/history.rs` | Add `SegmentScope::UnreleasedSinceStable` variant + skip logic (~15 lines) |
| `src/version.rs` | Replace `PreRelease(String)` with 4 variants. New `auto+pre_id` path. Update bump fns to use `.0`. Error on `prerelease` from stable. (~80 lines net) |
| `src/context.rs` | Add `pre_id: String` field |
| `src/cli/options.rs` | Add `--pre-id` flag, update `--bump` help text |
| `src/cli/main.rs` | Update `to_bump_spec()`, add `resolve_pre_id()`, pass `pre_id` to Context |
| `src/traverse.rs` | Update `breaking_version_bump()` to use new variant |
| `src/command/release/mod.rs` | Thread `pre_id` through to Context |
| `src/command/changelog.rs` | Pass default (empty) `pre_id` to Context |

**Estimated total:** ~400-500 lines of diff, medium complexity, ~2-3 days.
