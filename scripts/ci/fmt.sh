#!/usr/bin/env bash
# rustfmt for every workspace member and nothing else: `cargo fmt --all` also
# formats path dependencies, which would rewrite the vendored crates.
# Usage: scripts/ci/fmt.sh            format in place
#        scripts/ci/fmt.sh --check    fail on unformatted code (CI)
set -euo pipefail
cd "$(dirname "$0")/../.."

packages=()
while IFS= read -r name; do
  name="${name%$'\r'}"
  packages+=(-p "$name")
done < <(cargo metadata --no-deps --format-version 1 \
  | python3 -c 'import json, sys; print("\n".join(p["name"] for p in json.load(sys.stdin)["packages"]))')

exec cargo fmt "${packages[@]}" -- "$@"
