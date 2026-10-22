#!/usr/bin/env bash
# Publish this tree's argmend version when crates.io does not have it yet.
set -euo pipefail

echo "PLAN: publish argmend from $(pwd)"
if [ ! -f Cargo.toml ]; then
  echo "FAIL: Cargo.toml is missing"
  exit 1
fi

version=$(awk -F'"' '/^version = / { print $2; exit }' Cargo.toml)
if [ -z "$version" ]; then
  echo "FAIL: Cargo.toml has no version"
  exit 1
fi
echo "DO: Cargo.toml version ${version}"

if [ -n "${TAG:-}" ]; then
  expect="${TAG#v}"
  if [ "$expect" != "$version" ]; then
    echo "FAIL: tag ${TAG} does not match Cargo.toml ${version}"
    exit 1
  fi
  echo "OK: tag ${TAG} matches the crate version"
fi

ua="argmend-publish (https://github.com/argmend/argmend)"
url="https://crates.io/api/v1/crates/argmend/${version}"
code=""
attempt=0
while [ "$attempt" -lt 4 ]; do
  attempt=$((attempt + 1))
  code=$(curl -sS -A "$ua" -o /tmp/argmend-crate.json -w "%{http_code}" "$url" || true)
  echo "DO: crates.io GET ${code:-none} (attempt ${attempt})"
  if [ "$code" = "200" ]; then
    echo "OK: argmend ${version} is already on crates.io"
    echo "DONE: skip"
    exit 0
  fi
  if [ "$code" = "404" ]; then
    break
  fi
  if [ "$code" = "429" ] && [ "$attempt" -lt 4 ]; then
    echo "WAIT: crates.io rate limit, retrying"
    sleep 15
    continue
  fi
  echo "FAIL: unexpected crates.io HTTP ${code:-none}"
  exit 1
done

if [ "${DRY_RUN:-0}" = "1" ]; then
  echo "DO: cargo publish --dry-run --locked"
  cargo publish --dry-run --locked
  echo "DONE: dry-run"
  exit 0
fi

if [ -z "${CARGO_REGISTRY_TOKEN:-}" ]; then
  echo "FAIL: CARGO_REGISTRY_TOKEN is empty"
  exit 1
fi

echo "DO: cargo publish --locked"
log=$(mktemp)
set +e
cargo publish --locked >"$log" 2>&1
status=$?
set -e
cat "$log"
if [ "$status" -eq 0 ]; then
  echo "OK: published argmend ${version}"
  echo "DONE: published"
  exit 0
fi
if grep -E -q "already uploaded|already exists" "$log"; then
  echo "OK: cargo says argmend ${version} is already published"
  echo "DONE: skip"
  exit 0
fi
echo "FAIL: cargo publish exited ${status}"
exit 1
