#!/bin/bash
set -euo pipefail

# Trigger the rolling nightly by hand: a `workflow_dispatch` of publish.yml on
# main. Called from `make nightly` and `make nightly-docker`.
#
#   scripts/nightly.sh                 everything: desktop (macOS / Linux / Windows) + the web image
#   scripts/nightly.sh docker          only the web image (vicanso/zedis-web:nightly)
#   scripts/nightly.sh --watch         …and follow the run to its end
#   scripts/nightly.sh --sign-test     sign the Windows build with SignPath's test certificate
#   scripts/nightly.sh --yes           no confirmation prompt
#   scripts/nightly.sh --force         go ahead past a failed check
#
# The scheduled nightly (18:00 UTC) skips itself when main has not moved; a
# manual run always builds. It builds **origin/main**, never the working tree
# — the checks below exist because each of them has already cost a build:
#
#   - unpushed or uncommitted work is not in the nightly, however recent;
#   - two runs at once race on the same `nightly` release;
#   - a `docker` run used to sweep the nightly release it then uploaded
#     nothing to, leaving it empty and making the next scheduled run skip
#     ("main unchanged since last nightly"). publish.yml guards that now, and
#     this script refuses a `docker` run against a main that predates the
#     guard.

cd "$(dirname "$0")/.."

WORKFLOW=publish.yml
targets=all
signpath=none
watch=false
assume_yes=false
force=false

for arg in "$@"; do
  case "$arg" in
    all | docker) targets=$arg ;;
    --watch) watch=true ;;
    --sign-test) signpath=test-signing ;;
    --yes | -y) assume_yes=true ;;
    --force) force=true ;;
    -h | --help)
      sed -n '4,12p' "$0" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    *)
      echo "unknown argument: $arg (see --help)" >&2
      exit 2
      ;;
  esac
done

# A failed check stops the run unless --force says the caller knows better.
problem() {
  echo "✗ $1" >&2
  if [ "$force" = true ]; then
    echo "  --force: continuing anyway" >&2
  else
    echo "  (--force to go ahead regardless)" >&2
    exit 1
  fi
}

command -v gh >/dev/null 2>&1 || { echo "gh (GitHub CLI) is required: brew install gh" >&2; exit 1; }
gh auth status >/dev/null 2>&1 || { echo "gh is not signed in: gh auth login" >&2; exit 1; }

git fetch --quiet origin main
remote_sha=$(git rev-parse origin/main)
echo "nightly builds origin/main: $(git log -1 --format='%h %s' "$remote_sha")"

# What is here and not there is not going to be in the build.
ahead=$(git rev-list --count origin/main..HEAD 2>/dev/null || echo 0)
dirty=$(git status --porcelain | wc -l | tr -d ' ')
if [ "$ahead" != 0 ]; then
  echo "! $ahead local commit(s) are not pushed — they will not be in this nightly:"
  git log --format='    %h %s' origin/main..HEAD
fi
if [ "$dirty" != 0 ]; then
  echo "! $dirty uncommitted file(s) — they will not be in this nightly:"
  git status --porcelain | sed 's/^/    /'
fi

# The sweep guard, read from the workflow main actually has.
if [ "$targets" = docker ]; then
  guard="Delete old nightly releases and tag"
  if ! git show "origin/main:.github/workflows/$WORKFLOW" \
    | grep -A 20 "name: $guard" | grep -q "inputs.targets != 'docker'"; then
    problem "publish.yml on origin/main still sweeps the nightly release on a docker-only run: this would leave the desktop nightly with no assets. Push the workflow fix first, or run \`all\`."
  fi
fi

# One run at a time: they all write to the same release.
running=$(gh run list --workflow "$WORKFLOW" --json status,databaseId,event \
  -q '[.[] | select(.status == "in_progress" or .status == "queued")] | length')
if [ "$running" != 0 ]; then
  problem "$running publish run(s) already in progress or queued — a second one races it on the nightly release. Wait, or cancel it: gh run list --workflow $WORKFLOW"
fi

echo
echo "  targets:         $targets"
echo "  signpath_policy: $signpath"
if [ "$targets" = all ]; then
  echo "  The desktop jobs sign and notarize on macOS; a full run takes about 30 minutes"
  echo "  and replaces every asset of the nightly release."
else
  echo "  Only vicanso/zedis-web:nightly is rebuilt; the nightly release is left alone."
fi
if [ "$assume_yes" != true ]; then
  printf "Trigger it? [y/N] "
  read -r answer
  case "$answer" in
    y | Y | yes) ;;
    *)
      echo "not triggered"
      exit 0
      ;;
  esac
fi

# `gh workflow run` answers before the run exists and does not say which run
# it started, so remember the newest one and wait for a newer.
latest() {
  gh run list --workflow "$WORKFLOW" --event workflow_dispatch --limit 1 \
    --json databaseId -q '.[0].databaseId // 0'
}
before=$(latest)
gh workflow run "$WORKFLOW" --ref main -f "targets=$targets" -f "signpath_policy=$signpath"

run_id=$before
for _ in $(seq 1 30); do
  sleep 2
  run_id=$(latest)
  [ "$run_id" != "$before" ] && break
done
if [ "$run_id" = "$before" ]; then
  echo "triggered, but the run has not shown up yet: gh run list --workflow $WORKFLOW"
  exit 0
fi

url=$(gh run view "$run_id" --json url -q .url)
echo "started: $url"

if [ "$watch" != true ]; then
  echo "follow it: gh run watch $run_id"
  exit 0
fi

# --exit-status: a failed run fails this script too.
gh run watch "$run_id" --exit-status
if [ "$targets" = all ]; then
  assets=$(gh release view nightly --json assets -q '.assets | length')
  echo "nightly release: $assets asset(s) — https://github.com/vicanso/zedis/releases/tag/nightly"
fi
