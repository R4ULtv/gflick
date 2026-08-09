#!/usr/bin/env bash
set -euo pipefail

if [[ $# -lt 3 || $# -gt 4 ]]; then
  echo "usage: $0 <base-ref> <head-ref> <owner/repo> [head-label]" >&2
  exit 2
fi

base_ref=$1
head_ref=$2
repository=$3
head_label=${4:-$head_ref}

git rev-parse --verify --quiet "${base_ref}^{commit}" >/dev/null || {
  echo "base ref does not resolve to a commit: $base_ref" >&2
  exit 2
}
git rev-parse --verify --quiet "${head_ref}^{commit}" >/dev/null || {
  echo "head ref does not resolve to a commit: $head_ref" >&2
  exit 2
}
[[ $repository =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]] || {
  echo "repository must use owner/name form: $repository" >&2
  exit 2
}

printf '## Features\n\n'

feature_count=0
while IFS=$'\x1f' read -r sha short_sha author_name author_email subject; do
  [[ $subject =~ ^feat(\([^\)]+\))?(!)?:[[:space:]]+ ]] || continue

  author=$author_name
  if [[ $author_email =~ ^[0-9]+\+([^@]+)@users\.noreply\.github\.com$ ]]; then
    login=${BASH_REMATCH[1]}
    author="[$login](https://github.com/$login)"
  elif [[ $author_email =~ ^([^@]+)@users\.noreply\.github\.com$ ]]; then
    login=${BASH_REMATCH[1]}
    author="[$login](https://github.com/$login)"
  fi

  printf -- '- %s by %s in [`%s`](https://github.com/%s/commit/%s)\n' \
    "$subject" "$author" "$short_sha" "$repository" "$sha"
  feature_count=$((feature_count + 1))
done < <(
  git log --reverse --no-merges \
    --format='%H%x1f%h%x1f%aN%x1f%aE%x1f%s' \
    "${base_ref}..${head_ref}"
)

if (( feature_count == 0 )); then
  printf -- '- No feature commits in this release.\n'
fi

printf '\n**Full Changelog**: [`%s...%s`](https://github.com/%s/compare/%s...%s)\n' \
  "$base_ref" "$head_label" "$repository" "$base_ref" "$head_label"
