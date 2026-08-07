#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CHECK_HISTORY=0

usage() {
  echo "usage: ./scripts/privacy-check.sh [--history]" >&2
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --history)
      CHECK_HISTORY=1
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      usage
      exit 2
      ;;
  esac
done

cd "$ROOT_DIR"

files=()
scan_files=()
while IFS= read -r -d '' file; do
  if [[ -f "$file" ]]; then
    files+=("$file")
    [[ "$file" == "scripts/privacy-check.sh" ]] || scan_files+=("$file")
  fi
done < <(git ls-files --cached --others --exclude-standard -z)

failed=0
for file in "${files[@]}"; do
  case "$file" in
    .env|.env.*|*/.env|*/.env.*|*credentials*.json|*service-account*.json|*/token.json|*.pem|*.key|*.p12|*.pfx|*.sqlite|*.sqlite3|*.db|*.log)
      printf '[privacy] blocked sensitive filename: %s\n' "$file" >&2
      failed=1
      ;;
  esac
done

patterns=(
  '-----BEGIN (RSA |EC |OPENSSH )?PRIVATE KEY-----'
  'github_pat_[A-Za-z0-9_]+'
  'gh[pousr]_[A-Za-z0-9]{20,}'
  'AIza[0-9A-Za-z_-]{30,}'
  'AKIA[0-9A-Z]{16}'
  'xox[baprs]-[0-9A-Za-z-]+'
  '/home/ubuntu(/|$)'
  '/Users/(?!<you>(/|$))[A-Za-z0-9._-]+/'
  '10\.0\.1\.[0-9]{1,3}'
  '[A-Za-z0-9._%+-]+@(gmail|qq|163|126|outlook|hotmail)\.[A-Za-z]{2,}'
  'surveying-jfs'
)

if [[ "${#scan_files[@]}" -gt 0 ]]; then
  rg_args=(--files-with-matches --ignore-case --pcre2 --no-messages)
  for pattern in "${patterns[@]}"; do
    rg_args+=(-e "$pattern")
  done
  matches="$(rg "${rg_args[@]}" -- "${scan_files[@]}" || true)"
  if [[ -n "$matches" ]]; then
    echo "[privacy] blocked sensitive content in:" >&2
    printf '%s\n' "$matches" | sed 's/^/  - /' >&2
    failed=1
  fi
fi

if [[ "$CHECK_HISTORY" -eq 1 ]]; then
  personal_domains='@(gmail|qq|163|126|outlook|hotmail)\.[A-Za-z]{2,}$'
  if git log --format='%ae%n%ce' | rg --quiet --ignore-case --pcre2 "$personal_domains"; then
    echo "[privacy] warning: Git history contains a public personal email address" >&2
  fi
fi

if [[ "$failed" -ne 0 ]]; then
  echo "[privacy] check failed; no matching content was printed" >&2
  exit 1
fi

echo "[privacy] check passed"
