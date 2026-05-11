# Plan for Supporting Pre-release Versions

## Problem Statement

The README explicitly lists this as a missing feature: *"Handle pre-release versions, like 1.0.0-beta.1"*. Currently, the tool's concept of "pre-release" is limited to `major == 0` (the `is_pre_release_version()` function in `src/utils.rs`). It has no support for semver pre-release identifiers like `-beta.1`, `-alpha.2`, `-rc.1`.

Key gaps:
- `bump_major_minor_patch()` always clears `v.pre = Prerelease::EMPTY` — so bumping `1.0.0-beta.1` produces `1.0.1` instead of `1.0.0-beta.2`
- No `BumpSpec` variant exists for pre-release bumps
- Dependency version requirements don't account for cargo's strict pre-release matching semantics
- Auto mode doesn't know how to stay within a pre-release series

## What Already Works

- **Tag parsing**: `parse_tag_version()` uses `semver::Version::parse()`, so tags like `v1.0.0-beta.1` are already recognized
- **Changelog `Version` enum**: Already wraps `semver::Version` which can hold pre-release identifiers
- **Version display**: Tags and changelogs will render pre-release versions correctly

---

## Phase 1: Core Version Bumping Logic (`src/version.rs`)

### 1a. Extend `BumpSpec`

```rust
pub enum BumpSpec {
    Auto,
    Keep,
    Patch,
    Minor,
    Major,
    PreRelease(String), // e.g., "beta", "alpha", "rc"
}
```

### 1b. Add pre-release bump function

New function `bump_pre_release(v: &mut Version, label: &str)`:
- If `v.pre` is empty: apply a minor bump first, then set `v.pre` to `{label}.1`
  - e.g., `1.0.0` → `1.1.0-beta.1`
- If `v.pre` starts with the same label: increment the numeric suffix
  - e.g., `1.1.0-beta.1` → `1.1.0-beta.2`
- If `v.pre` has a different label: replace with `{label}.1` (label change, e.g., beta→rc)
  - e.g., `1.1.0-beta.3` → `1.1.0-rc.1`

### 1c. Modify `bump_major_minor_patch()` for graduation

When `Patch`/`Minor`/`Major` is applied to a version with a non-empty `pre`:
- `Patch` on `1.0.0-beta.1` → `1.0.0` (strip pre-release, don't increment patch since the base version was never released)
- `Minor` on `1.0.0-beta.1` → `1.0.0` (same logic — the minor was already bumped)
- `Major` on `1.0.0-beta.1` → `1.0.0` (if pre-release is for this major) or `2.0.0` (if truly a new major)

The key insight: if the version already has a pre-release identifier, a non-pre-release bump should "graduate" it by stripping the pre-release, unless the bump level exceeds what's already encoded.

### 1d. Update Auto mode in `bump_package_with_spec()`

When in `Auto` mode and `!v.pre.is_empty()`:
- Breaking changes → increment pre-release number (stay in pre-release series)
- Features → increment pre-release number
- Fixes → increment pre-release number

This keeps the version in pre-release until explicitly graduated via `--bump patch/minor/major`.

---

## Phase 2: Pre-release Stability Classification (`src/utils.rs`, `src/traverse.rs`)

### 2a. Update `is_pre_release_version()`

```rust
pub fn is_pre_release_version(semver: &Version) -> bool {
    semver.major == 0 || !semver.pre.is_empty()
}
```

This makes versions like `1.0.0-beta.1` behave like 0.x versions for:
- Auto-publishing of dependent crates (they're considered "unstable")
- Breaking change propagation (minor bump = breaking for pre-release)
- Safety bump calculations

### 2b. Update `rhs_is_breaking_bump_for_lhs()`

```rust
pub(crate) fn rhs_is_breaking_bump_for_lhs(lhs: &Version, rhs: &Version) -> bool {
    if !lhs.pre.is_empty() && !rhs.pre.is_empty() {
        // Within same pre-release series (same major.minor.patch), not breaking
        // Different base version = breaking
        rhs.major > lhs.major || rhs.minor > lhs.minor || rhs.patch > lhs.patch
    } else if !lhs.pre.is_empty() && rhs.pre.is_empty() {
        // Graduating from pre-release: breaking if base version changed
        rhs.major > lhs.major || rhs.minor > lhs.minor
    } else {
        rhs.major > lhs.major || rhs.minor > lhs.minor
    }
}
```

### 2c. Update `breaking_version_bump()` in `src/traverse.rs`

```rust
fn breaking_version_bump(ctx: &Context, package: &Package, bump_when_needed: bool) -> anyhow::Result<Bump> {
    let breaking_spec = if !package.version.pre.is_empty() {
        // For pre-release versions, a breaking change just increments the pre-release
        BumpSpec::PreRelease(extract_pre_label(&package.version))
    } else if is_pre_release_version(&package.version) {
        BumpSpec::Minor
    } else {
        BumpSpec::Major
    };
    version::bump_package_with_spec(package, breaking_spec, ctx, bump_when_needed)
}
```

---

## Phase 3: CLI Interface (`src/cli/options.rs`, `src/cli/main.rs`)

### 3a. Extend `--bump` parsing in `to_bump_spec()`

```rust
fn to_bump_spec(spec: &str) -> anyhow::Result<BumpSpec> {
    Ok(match spec {
        "patch" | "Patch" => Patch,
        "minor" | "Minor" => Minor,
        "major" | "Major" => Major,
        "keep" | "Keep" => Keep,
        "auto" | "Auto" => Auto,
        s if s.starts_with("pre:") => PreRelease(s[4..].to_string()),
        s if s == "pre" => PreRelease("rc".to_string()), // default label
        unknown => anyhow::bail!("Unknown bump specification: {:?}", unknown),
    })
}
```

### 3b. Update CLI help text

Update the `--bump` option description to mention `pre:<label>` (e.g., `pre:beta`, `pre:alpha`, `pre:rc`).

---

## Phase 4: Dependency Version Requirements (`src/command/release/manifest.rs`)

### 4a. Handle pre-release version requirements

Cargo's semver matching for pre-releases is strict: `^1.0.0-beta.1` does NOT match `1.0.0-beta.2`. This means whenever a dependency's version has a pre-release identifier, the version requirement in dependents MUST be updated on every bump.

In `set_version_and_update_package_dependency()`, update the `force_update` logic:

```rust
let force_update = conservative_pre_release_version_handling
    && (version::is_pre_release(new_version) || !new_version.pre.is_empty())
    && req_as_version(&version_req)
        .is_some_and(|req_version| !version::rhs_is_breaking_bump_for_lhs(&req_version, new_version));
```

Actually, the simpler fix: if `!new_version.pre.is_empty()`, always force-update the requirement since cargo won't do range matching.

---

## Phase 5: Crates Index Interaction (`src/version.rs`)

In `bump_package_with_spec()`, the code uses `published_crate.highest_version()`. The `crates-index` crate's `highest_version()` uses semver ordering, where pre-release versions sort LOWER than their release counterparts. This is correct behavior — no change needed.

However, we need to be careful: if the latest published version is `1.0.0` and we're trying to publish `1.1.0-beta.1`, the comparison `latest_release >= desired_release` would be false (since `1.1.0-beta.1 > 1.0.0` in semver), so it would proceed correctly.

Edge case: if `1.1.0-beta.1` is already published and we compute `1.1.0-beta.2`, the comparison works correctly since `1.1.0-beta.2 > 1.1.0-beta.1`.

No changes needed here, but add a test to verify.

---

## Phase 6: Display and Logging

**`BumpSpec::Display`** — add the pre-release variant:

```rust
BumpSpec::PreRelease(label) => write!(f, "pre:{}", label),
```

---

## Phase 7: Tests

1. **Unit tests in `src/version.rs`**:
   - `bump_pre_release()` with empty pre, same label, different label
   - Graduation: patch/minor/major on pre-release version
   - Auto mode with pre-release current version

2. **Unit tests in `src/utils.rs`**:
   - `is_pre_release_version()` returns true for `1.0.0-beta.1`
   - `rhs_is_breaking_bump_for_lhs()` with pre-release versions

3. **Integration tests**:
   - Workspace fixture with a crate at `1.0.0-beta.1`
   - Verify tag lookup works for pre-release tags
   - Verify dependency requirement updates for pre-release versions

---

## Summary of Files to Modify

| File | Changes |
|------|---------|
| `src/version.rs` | Add `PreRelease` variant, `bump_pre_release()`, update auto mode, update graduation logic |
| `src/utils.rs` | Update `is_pre_release_version()` to include `!pre.is_empty()` |
| `src/traverse.rs` | Update `breaking_version_bump()` for pre-release |
| `src/cli/main.rs` | Extend `to_bump_spec()` parser |
| `src/cli/options.rs` | Update help text for `--bump` |
| `src/command/release/manifest.rs` | Force-update version reqs for pre-release deps |
| `src/command/mod.rs` | No structural changes needed |

## Design Decisions

1. **Pre-release versions are "unstable"**: Treated like 0.x for auto-publish and breaking change propagation.
2. **Auto mode preserves pre-release**: Won't graduate automatically — graduation requires explicit `--bump patch/minor/major`.
3. **Dependency pinning**: Pre-release version requirements are always updated (cargo doesn't range-match pre-releases).
4. **Label-based bumping**: `--bump pre:beta` creates/increments beta series; changing label (e.g., `pre:rc`) resets the counter.
