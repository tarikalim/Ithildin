#!/bin/sh
# Fails when a branch name breaks Conventional Branch 1.1.0 (https://conventionalbranch.org/):
# feat/, fix/, hotfix/, chore/ + lower-case words joined by single hyphens; release/ + a version.
# Also allowed: main, GitHub's revert branches and Dependabot's branches, whose names we do not choose.
#   scripts/check-branch-name.sh feat/pinch-zoom
set -eu
name="${1:?usage: check-branch-name.sh <branch>}"
words='[a-z0-9]+(-[a-z0-9]+)*'
version='[a-z0-9]+([.-][a-z0-9]+)*'
if printf '%s\n' "$name" | grep -Eq "^(main|(feat|fix|hotfix|chore)/$words|release/$version|revert-[0-9]+-.+|dependabot/.+)$"; then
    echo "branch name ok: $name"
else
    echo "branch name $name does not follow Conventional Branch: use feat/, fix/, hotfix/ or chore/ + lower-case-words, or release/<version>" >&2
    exit 1
fi
