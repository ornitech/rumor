#!/usr/bin/env bash
# Remove Docker containers (and their Compose networks) started from a workspace folder.
#
# Deterministic: Docker Compose stamps every container it creates with the label
# `com.docker.compose.project.working_dir` holding the absolute path the project was
# launched from. We match on that path, so cleanup needs no per-project config and
# never touches containers belonging to other workspaces.
#
# Usage:
#   cleanup-docker.sh [workspace-dir]     # default: $CONDUCTOR_WORKSPACE_PATH, else $PWD
#   cleanup-docker.sh --dry-run [dir]     # list what would be removed, remove nothing
#   cleanup-docker.sh --volumes [dir]     # ALSO remove the projects' named volumes (deletes data)
#
# Built for Conductor's archive hook (scripts.archive) but safe to run by hand.
# Idempotent: a no-op when nothing matches. Never removes volumes unless --volumes.
set -euo pipefail

dry_run=0
remove_volumes=0
target=""
for arg in "$@"; do
    case "$arg" in
        --dry-run) dry_run=1 ;;
        --volumes) remove_volumes=1 ;;
        -*) echo "unknown option: $arg" >&2; exit 2 ;;
        *)  target="$arg" ;;
    esac
done

target="${target:-${CONDUCTOR_WORKSPACE_PATH:-$PWD}}"

command -v docker >/dev/null 2>&1 || { echo "docker not found; nothing to do"; exit 0; }
docker info  >/dev/null 2>&1 || { echo "docker daemon not running; nothing to do"; exit 0; }

# Resolve to an absolute path with no trailing slash. The dir may already be gone by
# the time an archive hook runs, so fall back to the literal argument when missing.
[ -d "$target" ] && target="$(cd "$target" && pwd)"
target="${target%/}"

fmt='{{.Label "com.docker.compose.project"}}'$'\t''{{.Label "com.docker.compose.project.working_dir"}}'
projects="$(
    docker ps -a --format "$fmt" \
        | awk -F'\t' -v t="$target" '$2 != "" && ($2 == t || index($2, t "/") == 1) { print $1 }' \
        | sort -u
)"

if [ -z "$projects" ]; then
    echo "no containers started from $target"
    exit 0
fi

echo "workspace: $target"
echo "compose projects:"
printf '  %s\n' $projects

for proj in $projects; do
    cids="$(docker ps -aq --filter "label=com.docker.compose.project=$proj")"
    nids="$(docker network ls -q --filter "label=com.docker.compose.project=$proj")"
    if [ "$dry_run" -eq 1 ]; then
        echo "[dry-run] $proj: $(printf '%s' "$cids" | grep -c . || true) container(s), $(printf '%s' "$nids" | grep -c . || true) network(s)"
        [ "$remove_volumes" -eq 1 ] && echo "[dry-run] $proj: would also remove named volumes"
        continue
    fi
    [ -n "$cids" ] && docker rm -f $cids >/dev/null
    [ -n "$nids" ] && docker network rm $nids >/dev/null 2>&1 || true
    if [ "$remove_volumes" -eq 1 ]; then
        vids="$(docker volume ls -q --filter "label=com.docker.compose.project=$proj")"
        [ -n "$vids" ] && docker volume rm $vids >/dev/null 2>&1 || true
    fi
    echo "removed $proj"
done
