#!/bin/bash
# Test that a crate in the dependency chain gets PUBLISHED (not just
# manifest-updated) when its dependency is bumped to a pre-release version.
#
# Scenario: top → mid → utils (mid also has optional dep on utils)
# - top has changes and is being published
# - utils has changes and gets bumped to 0.1.1-alpha.0 (via --pre-id alpha)
# - mid depends on utils with ^0.1.0, which can't match 0.1.1-alpha.0 on crates.io
# - Therefore mid MUST also be published with an updated utils constraint
set -eu

exe="${1:?First argument must be the executable to test}"
root="$(cd "${0%/*}" && pwd)"
fixtures="$root/fixtures"

sandbox="$(mktemp -t sandbox-pre-release-prop.XXXXXX -d)"
trap "rm -rf $sandbox" EXIT
cd "$sandbox"

export GIT_AUTHOR_DATE="2021-09-09 09:06:03 +0200"
export GIT_COMMITTER_DATE="${GIT_AUTHOR_DATE}"
export GIT_AUTHOR_NAME="Test User"
export GIT_COMMITTER_NAME="${GIT_AUTHOR_NAME}"
export GIT_AUTHOR_EMAIL="test@example.com"
export GIT_COMMITTER_EMAIL="${GIT_AUTHOR_EMAIL}"
export CARGO_HOME="$(mktemp -t cargo-home.XXXXXX -d)"

cp -R "$fixtures/four-depth-workspace/"* .
echo 'target/' > .gitignore
git init . &>/dev/null
git config commit.gpgsign false
git config tag.gpgsign false
git add . && git commit -q -m "initial"

# Tag all versions so they look "released"
git tag top-v0.1.0
git tag mid-v0.1.0
git tag leaf-v0.1.0
git tag utils-v0.1.0

# Make changes to both top and utils
(cd utils && touch change && git add change && git commit -q -m "feat: utils change") &>/dev/null
(cd top && touch change && git add change && git commit -q -m "feat: top change") &>/dev/null

# Run smart-release with --pre-id alpha and patch bumps for dependencies
# utils gets bumped to 0.1.1-alpha.0 (patch + pre-id)
# mid depends on utils ^0.1.0 — which won't match 0.1.1-alpha.0 on crates.io
# mid MUST be published too
output=$("$exe" smart-release top --no-push --no-publish -v --allow-dirty --pre-id alpha --no-bump-on-demand -d patch 2>&1) || true

echo "$output"
echo "---"

# mid must be listed for publishing (auto-bump or patch-bump), not just manifest adjustment
if echo "$output" | grep -q "bump.*mid.*for publishing"; then
    echo "PASS: 'mid' is scheduled for publishing (pre-release propagation works)"
    exit 0
else
    if echo "$output" | grep -q "adjust version constraints.*mid"; then
        echo "FAIL: 'mid' only gets a manifest adjustment, but needs to be PUBLISHED"
        echo "because crates.io's mid@0.1.0 has utils ^0.1.0 which can't match 0.1.1-alpha.0"
        exit 1
    else
        echo "FAIL: 'mid' is not even mentioned"
        exit 1
    fi
fi
