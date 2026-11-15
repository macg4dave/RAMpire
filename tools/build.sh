#!/usr/bin/env bash
# Build and test RAMpire the same way on any workstation.
#
#   tools/build.sh [--container | --host HOST | --local] [check | XTASK ARGUMENTS...]
#   tools/build.sh install-check [DIRECTORY]
#   tools/build.sh install
#
# With no arguments it runs the gate, then builds the packages into dist/:
# the gate is a whitespace check here, then format, release build, tests,
# Clippy and shell syntax for every Linux-buildable crate (cargo xtask gate
# checks); the packages are cargo xtask package, by the same builder.
# `check` runs the gate alone. Anything else is passed to cargo xtask,
# for example `tools/build.sh package --static`, and a package's output
# directory (dist/ unless --output says otherwise) is copied back here.
# `tools/build.sh fixtures` writes this release's protocol fixtures
# (fixtures/protocol/MAJOR.MINOR) and copies them back the same way.
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
# and both units, and that the static guest the host package carries runs. RAMPIRE_INSTALL_IMAGES replaces the list of images.
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
  # Images by their full names: podman won't pick a registry for a short
  # one without a terminal to ask on.
  images="docker.io/library/debian:trixie docker.io/rockylinux/rockylinux:10"
  images+=" registry.fedoraproject.org/fedora:45"
  for image in ${RAMPIRE_INSTALL_IMAGES:-$images}; do
    echo >&2 "==> installing the packages on $image"
    # z labels the packages for containers on an SELinux host, which
    # can't read them otherwise; elsewhere it does nothing.
    if "$engine" run --rm -v "$packages:/dist:ro,z" "$image" bash -c '
      set -eu
      if command -v apt-get >/dev/null; then
        apt-get -o Acquire::Retries=5 update -qq >/dev/null
        apt-get -o Acquire::Retries=5 install -y -qq /dist/rampire_*_amd64.deb /dist/rampire-guest_*_amd64.deb >/tmp/install.log 2>&1           || { grep -E "^(dpkg|E|error|Error)[: ]|warning: |scriptlet" /tmp/install.log | head -40; echo ...; tail -20 /tmp/install.log; exit 1; }
      else
        dnf install -y -q --setopt=retries=10 /dist/rampire-[0-9]*.x86_64.rpm /dist/rampire-guest-[0-9]*.x86_64.rpm >/tmp/install.log 2>&1           || { grep -E "^(dpkg|E|error|Error)[: ]|warning: |scriptlet" /tmp/install.log | head -40; echo ...; tail -20 /tmp/install.log; exit 1; }
      fi
      echo "host:  $(rampire --version)"
      echo "guest: $(/usr/libexec/rampire-guest version)"
      # The static guest the host package carries for guests without dnf
      # or apt, with the unit that runs it from /usr/local.
      echo "static guest: $(/usr/share/rampire/guest/rampire-guest version)"
      grep -q "^ExecStart=/usr/local/libexec/rampire-guest " /usr/share/rampire/guest/rampire-guest.service \
        || { echo "no unit for the static guest" >&2; exit 1; }
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
# The packages are built once the gate passes: with no arguments.
then_package=0
if (( $# == 0 )); then
  then_package=1
  set -- check
fi
if [[ $1 == check ]]; then
  (( $# == 1 )) || { echo 'check takes no arguments' >&2; exit 1; }
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

if [[ $1 == fixtures ]]; then
  output=fixtures/protocol
fi

# Temporary files and folders, removed however the script ends.
temporary=()
trap 'rm -rf "${temporary[@]}"' EXIT

if (( gate )); then
  # Snapshot the working tree as a Git tree object without touching the
  # user's index: a temporary index takes every non-ignored file.
  index=$(mktemp)
  temporary+=("$index")
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

# Take a tar of the output directory on stdin into a folder of its own,
# then, once it has all arrived, put it in place of what an earlier build
# left: a build that fails or is stopped leaves the earlier packages.
# It runs on the right of a pipe, so its folder, $received, is made first.
received=
if [[ -n $output ]]; then
  received=$(mktemp -d)
  temporary+=("$received")
fi
receive_output() {
  tar -x -f - -C "$received"
  [[ -d $received/$output ]] || { echo "the build sent no $output/" >&2; return 1; }
  clear_output
  mkdir -p "$output"
  cp -a "$received/$output/." "$output/"
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
      if [[ -n $output ]]; then receive_output; else cat; fi
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
      "$ssh" -o BatchMode=yes "$host" "tar -c -C '$remote_dir/src' -f - '$output'" | receive_output
    fi
    ;;
esac

if [[ -n $output ]]; then
  echo >&2 "==> output in $output/"
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
  # On a Windows workstation the Windows guest builds natively too.
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
if (( then_package )); then
  # The same builder, run again for the packages; it reuses the gate's
  # release build.
  case $builder in
    container) again=(--container) ;;
    local) again=(--local) ;;
    host) again=(--host "$host") ;;
  esac
  rm -rf -- "${temporary[@]}"
  temporary=()
  exec "$0" "${again[@]}" package
fi
