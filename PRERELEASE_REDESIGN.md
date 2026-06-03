# Pre-release Version Support

## Design

`--pre-id` is a modifier flag that changes the behavior of `--bump`:

| Command | From stable `1.2.3` | From pre `2.0.0-beta.2` |
|---------|---------------------|--------------------------|
| `--bump major --pre-id beta` | `2.0.0-beta.0` | `3.0.0-beta.0` |
| `--bump minor --pre-id beta` | `1.3.0-beta.0` | `2.1.0-beta.0` |
| `--bump patch --pre-id beta` | `1.2.4-beta.0` | `2.0.1-beta.0` |
| `--bump auto --pre-id beta` | computed from history | computed, or increment |
| `--bump prerelease` | ERROR | `2.0.0-beta.3` |
| `--bump prerelease --pre-id rc` | ERROR | `2.0.0-rc.0` |
| `--bump major` | `2.0.0` | `2.0.0` (graduate) |
| `--bump auto` | computed from history | graduate to stable |

### Key Behaviors

- **`--pre-id` with major/minor/patch:** Always bumps the base version and sets
  pre-release to `{label}.0`. Follows npm premajor/preminor/prepatch semantics.

- **`--bump prerelease`:** Increments the existing pre-release counter. Errors if
  not already a pre-release. With `--pre-id`, can change labels (resets to `.0`).

- **`--bump auto --pre-id`:** Computes base version from all commits since the last
  *stable* tag (skipping pre-release tags). If the computed base matches the current
  base, increments the counter. If higher, escalates and resets to `.0`.

- **`--bump auto` (no pre-id) on pre-release:** Graduates to stable (strips pre).

- **Starting number:** `.0` (npm convention).

### Weekly Pre-release Workflow

```bash
# CI runs every week — same command every time:
cargo smart-release --bump auto --pre-id beta -e

# When ready to ship stable:
cargo smart-release --bump auto -e
```

## BumpSpec Enum

```rust
pub enum BumpSpec {
    Auto,
    Keep,
    Patch,
    Minor,
    Major,
    PreRelease,  // increment counter, error if not pre-release
}
```

The label lives in `Context::pre_id` (empty string = not specified).

## Implementation

- `src/version.rs`: Core logic — pre-id modifier, prerelease variant, auto+pre-id, find_last_stable_version
- `src/context.rs`: `pre_id: String` field
- `src/git/history.rs`: `SegmentScope::UnreleasedSinceStable` — skips pre-release tags
- `src/traverse.rs`: `breaking_version_bump()` uses `BumpSpec::PreRelease`
- `src/cli/options.rs`: `--pre-id` flag
- `src/cli/main.rs`: `to_bump_spec()` accepts "prerelease"
