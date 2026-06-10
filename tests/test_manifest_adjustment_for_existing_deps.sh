#!/bin/bash
# Test that verifies a dependent crate already found by depth-first traversal
# still gets a manifest adjustment when one of its dependencies is bumped.
#
# Scenario: top → mid → leaf → utils, and mid also depends on utils (optional + build-dep).
# When `utils` has changes and gets bumped, `mid` (already found as a dependency of `top`)
# should still get its manifest updated to reflect the new `utils` version.
set -eu

exe="${1:?First argument must be the executable to test}"
root="$(cd "${0%/*}" && pwd)"
fixtures="$root/fixtures"

# Create a temporary sandbox
sandbox="$(mktemp -t sandbox-four-depth.XXXXXX -d)"
trap "rm -rf $sandbox" EXIT
cd "$sandbox"

# Set static git environment
export GIT_AUTHOR_DATE="2021-09-09 09:06:03 +0200"
export GIT_COMMITTER_DATE="${GIT_AUTHOR_DATE}"
export GIT_AUTHOR_NAME="Test User"
export GIT_COMMITTER_NAME="${GIT_AUTHOR_NAME}"
export GIT_AUTHOR_EMAIL="test@example.com"
export GIT_COMMITTER_EMAIL="${GIT_AUTHOR_EMAIL}"
export CARGO_HOME="$(mktemp -t cargo-home.XXXXXX -d)"

cp -R "$fixtures/four-depth-workspace/"* .
echo 'target/' > .gitignore
git init .
git config commit.gpgsign false
git config tag.gpgsign false
git add . && git commit -q -m "initial"

# Tag all versions so they look "released"
git tag top-v0.1.0
git tag mid-v0.1.0
git tag leaf-v0.1.0
git tag utils-v0.1.0

# Make a change to utils
(cd utils && touch change && git add change && git commit -q -m "feat: utils change")

# Run smart-release on 'top' in dry-run mode
output=$("$exe" smart-release top --no-push --no-publish -v --allow-dirty 2>&1) || true

echo "$output"
echo "---"

# Check that 'mid' is listed for manifest version constraint adjustment
if echo "$output" | grep -q "adjust version constraints.*mid"; then
    echo "PASS: 'mid' gets manifest version constraint adjustment"
    exit 0
else
    echo "FAIL: 'mid' was NOT listed for manifest version constraint adjustment"
    echo "This is the bug: mid depends on utils (which was bumped) but since mid was"
    echo "already in the traversal result as DependencyOrDependentOfUserSelection,"
    echo "the tool didn't mark it for a manifest update."
    exit 1
fi
