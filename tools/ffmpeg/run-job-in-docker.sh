#!/usr/bin/env bash
# Run a job of build-ffmpeg.yml locally in Docker, from the workflow text itself.
#
# Usage: run-job-in-docker.sh <work-dir> [job] [image]
#        (defaults: job "linux", image "ubuntu:22.04")
#
# Extracts the job's `run:` steps in order, applies the workflow and job `env`
# blocks, and executes them in a fresh container with GitHub's default bash
# flags (-eo pipefail) and GITHUB_ENV semantics between steps, so what runs
# here is what CI will run. `uses:` steps are handled as follows: checkout is
# replaced by a copy of this working tree's .github/ and tools/, upload-artifact
# is skipped (outputs stay in <work-dir>/workspace). Any other `uses:`, any
# step `if:`, or any `${{ }}` expression inside a run block stops the script
# rather than being silently approximated.
#
# Differences from a GitHub runner, by design: a `sudo` shim (the container
# runs as root), DEBIAN_FRONTEND=noninteractive, and none of the runner image's
# preinstalled software.
#
# Needs: docker, python3 with PyYAML on the host.

set -euo pipefail

[ $# -ge 1 ] || { echo "usage: $0 <work-dir> [job] [image]" >&2; exit 2; }
work=$(mkdir -p "$1" && cd "$1" && pwd)
job=${2:-linux}
image=${3:-ubuntu:22.04}
repo=$(cd "$(dirname "$0")/../.." && pwd)
workflow="$repo/.github/workflows/build-ffmpeg.yml"

rm -rf "$work/steps" "$work/workspace"
mkdir -p "$work/steps" "$work/workspace" "$work/home"
cp -R "$repo/.github" "$repo/tools" "$work/workspace/"

python3 -I - "$workflow" "$job" "$work/steps" <<'PY'
import shlex
import sys

import yaml

workflow_path, job_name, out_dir = sys.argv[1:4]
with open(workflow_path, encoding="utf-8") as f:
    wf = yaml.safe_load(f)

job = wf["jobs"][job_name]
env = dict(wf.get("env") or {})
env.update(job.get("env") or {})
for key, value in env.items():
    if "${{" in str(value):
        sys.exit(f"env {key} uses an expression; not supported locally")

with open(f"{out_dir}/env.sh", "w", encoding="utf-8") as f:
    for key, value in env.items():
        f.write(f"export {key}={shlex.quote(str(value))}\n")

n = 0
for step in job["steps"]:
    name = step.get("name", "(unnamed)")
    if "if" in step:
        sys.exit(f"step '{name}' has an if: condition; not supported locally")
    if "uses" in step:
        action = step["uses"].split("@")[0]
        if action in ("actions/checkout", "actions/upload-artifact"):
            print(f"skip  {name} ({action})")
            continue
        sys.exit(f"step '{name}' uses {action}; not supported locally")
    run = step["run"]
    if "${{" in run:
        sys.exit(f"step '{name}' contains a ${{{{ }}}} expression; not supported locally")
    n += 1
    with open(f"{out_dir}/{n:02d}.sh", "w", encoding="utf-8") as f:
        f.write(f"# {name}\n{run}")
    print(f"step  {n:02d} {name}")
PY

cat >"$work/steps/driver.sh" <<'SH'
#!/bin/bash
set -euo pipefail
printf '#!/bin/sh\nexec "$@"\n' >/usr/local/bin/sudo
chmod +x /usr/local/bin/sudo
export DEBIAN_FRONTEND=noninteractive CI=true HOME=/root
export GITHUB_WORKSPACE=/work/workspace GITHUB_ENV=/work/github_env
: >"$GITHUB_ENV"
. /work/steps/env.sh
status=0
for step in /work/steps/[0-9]*.sh; do
  # Values appended to GITHUB_ENV by earlier steps (KEY=VALUE lines only).
  while IFS= read -r line; do [ -n "$line" ] && export "$line"; done <"$GITHUB_ENV"
  echo "::: $(head -n 1 "$step" | sed 's/^# //')"
  start=$(date +%s)
  cd "$GITHUB_WORKSPACE"
  if ! bash --noprofile --norc -eo pipefail "$step"; then
    echo "::: FAILED: $(head -n 1 "$step" | sed 's/^# //')"
    status=1
    break
  fi
  echo "::: done in $(( $(date +%s) - start ))s"
done
chown -R "$HOST_UID:$HOST_GID" /work /root
exit $status
SH
chmod +x "$work/steps/driver.sh"

docker run --rm \
  -e HOST_UID="$(id -u)" -e HOST_GID="$(id -g)" \
  -v "$work:/work" -v "$work/home:/root" \
  "$image" /work/steps/driver.sh
