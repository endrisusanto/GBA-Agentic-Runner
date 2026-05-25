#!/usr/bin/env bash
set -Eeuo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

BUMP="patch"
PUSH=false

for arg in "$@"; do
  case "$arg" in
    major|minor|patch) BUMP="$arg" ;;
    --push) PUSH=true ;;
    -h|--help)
      cat <<'HELP'
Usage: scripts/release-next.sh [patch|minor|major] [--push]

Creates a release commit and annotated semver tag.
Default bump is patch. Pushes to origin only when --push is passed.
HELP
      exit 0
      ;;
    *) echo "Unknown argument: $arg" >&2; exit 1 ;;
  esac
done

if ! git rev-parse --is-inside-work-tree >/dev/null 2>&1; then
  echo "[release] Not inside a git repository." >&2
  exit 1
fi

if ! command -v node >/dev/null 2>&1; then
  echo "[release] Missing required command: node" >&2
  exit 1
fi

git fetch --tags --quiet || true

LATEST_TAG="$(git tag --list 'v[0-9]*.[0-9]*.[0-9]*' --sort=-v:refname | head -n 1)"
if [[ -z "$LATEST_TAG" ]]; then
  LATEST_TAG="v0.0.0"
fi

VERSION="${LATEST_TAG#v}"
IFS='.' read -r MAJOR MINOR PATCH <<< "$VERSION"

case "$BUMP" in
  major) MAJOR=$((MAJOR + 1)); MINOR=0; PATCH=0 ;;
  minor) MINOR=$((MINOR + 1)); PATCH=0 ;;
  patch) PATCH=$((PATCH + 1)) ;;
esac

NEXT_VERSION="${MAJOR}.${MINOR}.${PATCH}"
NEXT_TAG="v${NEXT_VERSION}"

if git rev-parse "$NEXT_TAG" >/dev/null 2>&1; then
  echo "[release] Tag already exists: $NEXT_TAG" >&2
  exit 1
fi

echo "[release] Latest tag: $LATEST_TAG"
echo "[release] Next tag:   $NEXT_TAG"

node - "$NEXT_VERSION" <<'NODE'
const fs = require("fs");
const version = process.argv[2];

for (const file of ["package.json", "package-lock.json"]) {
  if (!fs.existsSync(file)) continue;
  const data = JSON.parse(fs.readFileSync(file, "utf8"));
  data.version = version;
  if (data.packages && data.packages[""]) data.packages[""].version = version;
  fs.writeFileSync(file, JSON.stringify(data, null, 2) + "\n");
}

const tauriPath = "src-tauri/tauri.conf.json";
if (fs.existsSync(tauriPath)) {
  const data = JSON.parse(fs.readFileSync(tauriPath, "utf8"));
  data.version = version;
  fs.writeFileSync(tauriPath, JSON.stringify(data, null, 2) + "\n");
}
NODE

sed -i -E "0,/^version = \".*\"/s//version = \"${NEXT_VERSION}\"/" src-tauri/Cargo.toml

git add \
  .gitignore \
  README.md \
  agentic.png \
  auto.sh \
  build-linux.sh \
  package.json \
  package-lock.json \
  index.html \
  src \
  src-tauri/Cargo.toml \
  src-tauri/Cargo.lock \
  src-tauri/tauri.conf.json \
  src-tauri/build.rs \
  src-tauri/capabilities \
  src-tauri/icons \
  src-tauri/src \
  scripts \
  .github

if git diff --cached --quiet; then
  echo "[release] Nothing staged to commit." >&2
  exit 1
fi

git commit -m "chore: release ${NEXT_TAG}"
git tag -a "$NEXT_TAG" -m "Release ${NEXT_TAG}"

echo "[release] Created commit and tag ${NEXT_TAG}."

if [[ "$PUSH" == true ]]; then
  git push origin HEAD
  git push origin "$NEXT_TAG"
  echo "[release] Pushed branch and tag. GitHub Actions will publish the release."
else
  echo "[release] Local only. Run: git push origin HEAD && git push origin ${NEXT_TAG}"
fi
