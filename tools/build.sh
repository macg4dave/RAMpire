#!/usr/bin/env bash
# Build and test RAMpire the same way on any workstation.
#
#   tools/build.sh [--container | --host HOST | --local] [XTASK ARGUMENTS...]
#   tools/build.sh install-check [DIRECTORY]
#   tools/build.sh install
#
# With no arguments it runs the gate: a whitespace check here, then format,
# release build, tests, Clippy and shell syntax for every Linux-buildable
# crate (cargo xtask gate checks). Anything else is passed to cargo xtask,
# for example `tools/build.sh package --static`, and a package's output
# directory (dist/ unless --output says otherwise) is copied back here.
#
# install builds the packages, then installs the host package on this
# machine with its package manager (dnf or apt), upgrading what is
# installed, and checks that rampire reports the build just made. This is
# how a development or test host is kept on the current build.
#
# install-check installs the host and guest packages in DIRECTORY (default
# dist/) in a clean container of each supported family: Debian 13
# (.deb), Rocky Linux 10 and Fedora 45 (RPM). It checks that each installs
# with its dependencies, reports its build, and leaves the service account
# and both units. RAMPIRE_INSTALL_IMAGES replaces the list of images.
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

if [[ ${1:-} == install-check ]]; then
  [[ -n $engine ]] || { echo 'install-check needs docker or podman' >&2; exit 1; }
  packages=${2:-dist}
  [[ -d $packages ]] || { echo "no $packages/: run tools/build.sh package first" >&2; exit 1; }
  packages=$(cd -- "$packages" && pwd)
  # Docker on Windows takes a Windows path for a bind mount.
  if command -v cygpath >/dev/null; then packages=$(cygpath -m "$packages"); fi
  export MSYS_NO_PATHCONV=1
  failed=0
  for image in ${RAMPIRE_INSTALL_IMAGES:-debian:trixie rockylinux/rockylinux:10 fedora:45}; do
    echo >&2 "==> installing the packages on $image"
    if "$engine" run --rm -v "$packages:/dist:ro" "$image" bash -c '
      set -eu
      if command -v apt-get >/dev/null; then
        apt-get update -qq >/dev/null
        apt-get install -y -qq /dist/rampire_*_amd64.deb /dist/rampire-guest_*_amd64.deb >/tmp/install.log 2>&1           || { tail -20 /tmp/install.log; exit 1; }
      else
        dnf install -y -q /dist/rampire-[0-9]*.x86_64.rpm /dist/rampire-guest-[0-9]*.x86_64.rpm >/tmp/install.log 2>&1           || { tail -20 /tmp/install.log; exit 1; }
      fi
      echo "host:  $(rampire --version)"
      echo "guest: $(/usr/libexec/rampire-guest version)"
      getent passwd rampire >/dev/null || { echo "no rampire account" >&2; exit 1; }
      for unit in rampire.service rampire-guest.service; do
        test -f "/usr/lib/systemd/system/$unit" || { echo "no $unit" >&2; exit 1; }
      done
    '; then
      echo >&2 "==> $image: ok"
    else
      echo >&2 "==> $image: FAILED"
      failed=1
    fi
  done
  exit "$failed"
fi

gate=0
if (( $# == 0 )); then
  gate=1
  set -- gate checks
fi
# Where `package` writes, to copy back.
output=
install=0
if [[ $1 == install ]]; then
  install=1
  shift
  set -- package "$@"
fi
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
  echo >&2 "==> whitespace validation (working tree against HEAD)"
  git diff --check HEAD "$tree"
fi

# Before new packages are copied back, remove what an earlier build left in
# the output directory, so it holds one version (as cargo xtask package
# does in the builder).
clear_output() {
  [[ -d $output ]] || return 0
  find "$output" -maxdepth 1 -type f \( -name SHA256SUMS -o     \( -name 'rampire*' \( -name '*.rpm' -o -name '*.deb' -o -name '*.tar.gz' \) \) \) -delete
}

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
    echo >&2 "==> cargo xtask $* (here)"
    cargo run --quiet --locked -p xtask -- "$@"
    ;;
  container)
    [[ -n $engine ]] || { echo 'neither docker nor podman is installed' >&2; exit 1; }
    # Git for Windows rewrites arguments that look like paths; the
    # container's paths must reach the engine as they are.
    export MSYS_NO_PATHCONV=1
    image=rampire-build:$(sha256sum tools/container/Containerfile | cut -c1-12)
    if ! "$engine" image inspect "$image" >/dev/null 2>&1; then
      echo >&2 "==> building $image with $engine"
      "$engine" build -t "$image" -f tools/container/Containerfile tools/container >&2
    fi
    echo >&2 "==> cargo xtask $* (in $image)"
    # With package output to copy back, the build's own output goes to
    # stderr and stdout carries only the tar of it. Otherwise the command's
    # stdout passes through, so `package --print-version` can be captured.
    payload | "$engine" run --rm -i \
      -v rampire-cargo:/usr/local/cargo/registry \
      -v rampire-target:/build/src/target \
      -e "RAMPIRE_BUILD_OUTPUT=$output" \
      "$image" bash -c '
        set -eu
        tar -x -f -
        if [ -n "$RAMPIRE_BUILD_OUTPUT" ]; then exec 3>&1 1>&2; fi
        cargo run --quiet --locked -p xtask -- "$@"
        if [ -n "$RAMPIRE_BUILD_OUTPUT" ]; then tar -c -f - "$RAMPIRE_BUILD_OUTPUT" >&3; fi
      ' bash "$@" |
      if [[ -n $output ]]; then clear_output; tar -x -f -; else cat; fi
    ;;
  host)
    remote_dir=${RAMPIRE_BUILD_DIR:-}
    ssh=${RAMPIRE_SSH:-ssh}
    if [[ $remote_dir != /* || $remote_dir == / || $remote_dir =~ [[:space:]\'\"\$\`\\] ]]; then
      echo 'RAMPIRE_BUILD_DIR must be an absolute, plain path other than /' >&2
      exit 1
    fi
    quoted=$(printf ' %q' "$@")
    echo >&2 "==> sending the checkout to $host:$remote_dir/src"
    # target/ moves from the last run's tree into the new one: a real
    # directory, which Git ignores, unlike a link to one.
    payload | "$ssh" -o BatchMode=yes "$host" "set -eu
      mkdir -p '$remote_dir'
      rm -rf '$remote_dir/src.new'
      mkdir '$remote_dir/src.new'
      tar --warning=no-timestamp -x -C '$remote_dir/src.new'
      if [ -d '$remote_dir/src/target' ] && [ ! -L '$remote_dir/src/target' ]; then
        mv '$remote_dir/src/target' '$remote_dir/src.new/target'
      fi
      rm -rf '$remote_dir/src'
      mv '$remote_dir/src.new' '$remote_dir/src'"
    echo >&2 "==> cargo xtask $* (on $host)"
    "$ssh" -o BatchMode=yes "$host" "set -eu
      cd '$remote_dir/src'
      . \"\$HOME/.cargo/env\" 2>/dev/null || true
      cargo run --quiet --locked -p xtask --$quoted"
    if [[ -n $output ]]; then
      clear_output
      "$ssh" -o BatchMode=yes "$host" "tar -c -C '$remote_dir/src' -f - '$output'" | tar -x -f -
    fi
    ;;
esac

if [[ -n $output ]]; then
  echo >&2 "==> packages in $output/"
fi
if (( install )); then
  [[ -n $output ]] || { echo 'install builds packages; --print-version builds none' >&2; exit 1; }
  if command -v dnf >/dev/null; then
    packages=("$output"/rampire-[0-9]*.x86_64.rpm)
    (( ${#packages[@]} == 1 )) && [[ -f ${packages[0]} ]] \
      || { echo "expected one host package in $output/, found: ${packages[*]}" >&2; exit 1; }
    package=${packages[0]}
    # dnf install of a local file installs it, or upgrades the installed one.
    echo >&2 "==> installing $package with dnf"
    sudo dnf install -y "./$package"
  elif command -v apt-get >/dev/null; then
    packages=("$output"/rampire_*_amd64.deb)
    (( ${#packages[@]} == 1 )) && [[ -f ${packages[0]} ]] \
      || { echo "expected one host package in $output/, found: ${packages[*]}" >&2; exit 1; }
    package=${packages[0]}
    echo >&2 "==> installing $package with apt"
    sudo apt-get install -y --allow-downgrades "./$package"
  else
    echo 'install needs dnf or apt-get on this machine' >&2
    exit 1
  fi
  # A development package's version ends in the build rampire reports:
  # ~dev.COUNT.COMMIT[.dirty.DIGEST] is build COMMIT[-dirty.DIGEST].
  if [[ $package == *.rpm ]]; then
    version=$(rpm -qp --queryformat '%{VERSION}' "$package")
  else
    version=$(dpkg-deb --field "$package" Version)
  fi
  reported=$(rampire --version)
  echo >&2 "==> installed: $reported"
  if [[ $version == *~dev.* ]]; then
    build=${version#*~dev.*.}
    build=${build/.dirty./-dirty.}
    if [[ $reported != *"(build $build)"* ]]; then
      echo "the installed rampire is not the build just made ($build): $reported" >&2
      exit 1
    fi
  fi
fi
if (( gate )); then
  # On a Windows workstation the Windows guest builds natively too. This is
  # not the native gate on a Windows VM (cargo xtask windows all), which also
  # runs the release build that gets deployed.
  case $(uname -s) in
    MINGW* | MSYS* | CYGWIN*)
      if command -v cargo >/dev/null; then
        windows=(-p rampire-core -p rampire-guest -p rampire-guest-windows)
        echo >&2 "==> Windows guest: tests and Clippy here"
        cargo test "${windows[@]}" --locked --quiet
        cargo clippy "${windows[@]}" --all-targets --locked --quiet -- -D warnings
      fi
      ;;
  esac
  echo >&2 "==> gate passed for tree ${tree:0:12}"
fi
