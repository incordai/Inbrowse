#!/usr/bin/env bash
# Render every repro fixture in inbrowse and Chromium side by side.
# Usage: ./run.sh [outdir]   (default: ./out)
set -uo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN="${INBROWSE_BIN:-$ROOT/target/release/inbrowse}"
CHROME="${CHROME_BIN:-}"
PYTHON="${PYTHON_BIN:-python3}"
DIR="$(cd "$(dirname "$0")" && pwd)"
OUT="${1:-$DIR/out}"
mkdir -p "$OUT"
status=0
for f in "$DIR"/*.html; do
  n=$(basename "$f" .html)
  if ! INBROWSE_SHOT_W=900 INBROWSE_SHOT_H=1000 INBROWSE_ALLOW_PRIVATE_NETWORK=1 \
    timeout 60 "$BIN" fetch "file://$f" --screenshot "$OUT/$n.inbrowse.png" \
      --timeout 30000 --wait 2 >"$OUT/$n.inbrowse.log" 2>&1 || [[ ! -s "$OUT/$n.inbrowse.png" ]]; then
    echo "FAILED inbrowse: $n (see $OUT/$n.inbrowse.log)" >&2
    status=1
    continue
  fi

  chrome_args=("$DIR/capture_chromium.py" "file://$f" "$OUT/$n.chrome.png")
  if [[ -n "$CHROME" ]]; then
    chrome_args+=(--executable "$CHROME")
  fi
  if ! PYTHONDONTWRITEBYTECODE=1 timeout 60 "$PYTHON" "${chrome_args[@]}" \
    >"$OUT/$n.chrome.log" 2>&1 || [[ ! -s "$OUT/$n.chrome.png" ]]; then
    echo "FAILED chromium: $n (see $OUT/$n.chrome.log)" >&2
    status=1
    continue
  fi
  echo "rendered $n"
done
if [[ "$status" -eq 0 ]]; then
  if ! PYTHONDONTWRITEBYTECODE=1 "$PYTHON" "$DIR/check.py" "$OUT"; then
    status=1
  fi
fi
if [[ "$status" -eq 0 ]]; then
  echo "output in $OUT"
else
  echo "one or more renders failed; partial output in $OUT" >&2
fi
exit "$status"
