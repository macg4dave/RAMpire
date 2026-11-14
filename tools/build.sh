#!/usr/bin/env bash
# Build and test RAMpire the same way on any workstation and in CI.
#
#   tools/build.sh [--container | --host HOST | --local] [XTASK ARGUMENTS...]
#
# With no arguments it runs the gate: a whitespace check here, then format,
# release build, tests, Clippy and shell syntax for every Linux-buildable
# crate (cargo xtask gate checks). Anything else is passed to cargo xtask,
# for example `tools/build.sh package --static`, and a package's output
# directory (dist/ unless --output says otherwise) is copied back here.
#
# Where it runs:
#   --container  in the build container, tools/container/Containerfile, with
#                Docker or Podman (RAMPIRE_CONTAINER_ENGINE picks one). The
#                image is built on first use and again when the file changes.
#                Cargo's registry and target/ stay in the volumes
#                rampire-cargo and rampire-target between runs. The default.
#   --host HOST  on a Linux build host over SSH, in RAMPIRE_BUILD_DIR there,
#                using that host's own Rust and nfpm.
#   --local      here, which must be Linux with the tools installed.
#
# A host can be site configuration, read from the environment or from
# site/build.env (untracked; see deploy/examples/build.env.example):
#
#   RAMPIRE_BUILD_HOST  SSH destination; set, it is the default builder
#   RAMPIRE_BUILD_DIR   Absolute directory on that host the build owns
#   RAMPIRE_SSH         SSH client to use (default: ssh)
#
# The container and the host get exactly the files the build identity
# covers: tracked and untracked files that aren't ignored, as they are on
# disk, with .git so the build is stamped with its commit.
set -euo pipefail

repo=$(cd -- "$(dirname -- "$0")/.." && pwd)
cd "$repo"
if [[ -f site/build.env ]]; then
  # shellcheck source=/dev/null
  source site/build.env
fi
# Git for Windows rewrites arguments that look like paths; the container's
# paths must reach Docker as they are.
export MSYS_NO_PATHCONV=1

builder=
host=${RAMPIRE_BUILD_HOST:-}
case ${1:-} in
  --container) builder=container; shift ;;
  --local) builder=local; shift ;;
  --host) builder=host; host=${2:?--host needs an SSH destination}; shift 2 ;;
esac

engine=${RAMPIRE_CONTAINER_ENGINE:-}
if [[ -z $engine ]]; then
  if command -v docker >/dev/null; then engine=docker
  elif command -v podman >/dev/null; then engine=podman; fi
fi
if [[ -z $builder ]]; then
  if [[ -n $host ]]; then builder=host
  elif [[ -n $engine ]]; then builder=container
  elif [[ $(uname -s) == Linux ]]; then builder=local
  else
    echo 'no builder: install Docker or Podman, or set RAMPIRE_BUILD_HOST' >&2
    exit 1
  fi
fi

gate=0
if (( $# == 0 )); then
  gate=1
  set -- gate checks
fi
# Where `package` writes, to copy back.
output=
if [[ $1 == package ]]; then
  output=dist
  previous=
  for argument in "$@"; do
    [[ $previous == --output ]] && output=$argument
    previous=$argument
  done
  [[ $* == *--print-version* ]] && output=
  if [[ -n $output && ( $output == /* || $output == *..* ) ]]; then
    echo 'package --output must be a directory inside the checkout' >&2
    exit 1
  fi
fi

if (( gate )); then
  # Snapshot the working tree as a Git tree object without touching the
  # user's index: a temporary index takes every non-ignored file.
  index=$(mktemp)
  trap 'rm -f "$index"' EXIT
  rm -f "$index"
  GIT_INDEX_FILE=$index git -c core.safecrlf=false add -A
  tree=$(GIT_INDEX_FILE=$index git write-tree)
  echo "==> whitespace validation (working tree against HEAD)"
  git diff --check HEAD "$tree"
fi

# The files sent to a builder, as a tar stream on stdout.
payload() {
  {
    git ls-files -z --cached --others --exclude-standard |
      while IFS= read -r -d '' path; do
        [[ -e $path ]] && printf '%s\0' "$path"
      done
    printf '.git\0'
  } | tar --null -T - -c -f -
}

case $builder in
  local)
    echo "==> cargo xtask $* (here)"
    cargo run --quiet --locked -p xtask -- "$@"
    ;;
  container)
    [[ -n $engine ]] || { echo 'neither docker nor podman is installed' >&2; exit 1; }
    image=rampire-build:$(sha256sum tools/container/Containerfile | cut -c1-12)
    if ! "$engine" image inspect "$image" >/dev/null 2>&1; then
      echo "==> building $image with $engine"
      "$engine" build -t "$image" -f tools/container/Containerfile tools/container >&2
    fi
    echo "==> cargo xtask $* (in $image)"
    # Inside, the build's output goes to stderr and the stdout carries only
    # the tar of the package output.
    payload | "$engine" run --rm -i \
      -v rampire-cargo:/usr/local/cargo/registry \
      -v rampire-target:/build/src/target \
      -e "RAMPIRE_BUILD_OUTPUT=$output" \
      "$image" bash -c '
        set -eu
        tar -x -f -
        exec 3>&1 1>&2
        cargo run --quiet --locked -p xtask -- "$@"
        if [ -n "$RAMPIRE_BUILD_OUTPUT" ]; then tar -c -f - "$RAMPIRE_BUILD_OUTPUT" >&3; fi
      ' bash "$@" |
      if [[ -n $output ]]; then tar -x -f -; else cat >/dev/null; fi
    ;;
  host)
    remote_dir=${RAMPIRE_BUILD_DIR:-}
    ssh=${RAMPIRE_SSH:-ssh}
    if [[ $remote_dir != /* || $remote_dir == / || $remote_dir =~ [[:space:]\'\"\$\`\\] ]]; then
      echo 'RAMPIRE_BUILD_DIR must be an absolute, plain path other than /' >&2
      exit 1
    fi
    quoted=$(printf ' %q' "$@")
    echo "==> sending the checkout to $host:$remote_dir/src"
    payload | "$ssh" -o BatchMode=yes "$host" "set -eu
      mkdir -p '$remote_dir/target'
      rm -rf '$remote_dir/src.new'
      mkdir '$remote_dir/src.new'
      tar --warning=no-timestamp -x -C '$remote_dir/src.new'
      ln -s ../target '$remote_dir/src.new/target'
      rm -rf '$remote_dir/src'
      mv '$remote_dir/src.new' '$remote_dir/src'"
    echo "==> cargo xtask $* (on $host)"
    "$ssh" -o BatchMode=yes "$host" "set -eu
      cd '$remote_dir/src'
      . \"\$HOME/.cargo/env\" 2>/dev/null || true
      cargo run --quiet --locked -p xtask --$quoted"
    if [[ -n $output ]]; then
      "$ssh" -o BatchMode=yes "$host" "tar -c -C '$remote_dir/src' -f - '$output'" | tar -x -f -
    fi
    ;;
esac

if [[ -n $output ]]; then
  echo "==> packages in $output/"
fi
if (( gate )); then
  # On a Windows workstation the Windows guest builds natively too. This is
  # not the native gate on a Windows VM (cargo xtask windows all), which also
  # runs the release build that gets deployed.
  case $(uname -s) in
    MINGW* | MSYS* | CYGWIN*)
      if command -v cargo >/dev/null; then
        windows=(-p rampire-core -p rampire-guest -p rampire-guest-windows)
        echo "==> Windows guest: tests and Clippy here"
        cargo test "${windows[@]}" --locked --quiet
        cargo clippy "${windows[@]}" --all-targets --locked --quiet -- -D warnings
      fi
      ;;
  esac
  echo "==> gate passed for tree ${tree:0:12}"
fi
