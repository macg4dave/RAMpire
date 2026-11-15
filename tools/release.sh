#!/usr/bin/env bash
# Make a RAMpire release from this checkout (docs/releasing.md).
#
#   tools/release.sh prepare VERSION [--name NAME]
#   tools/release.sh publish [--dry-run]
#   tools/release.sh notes [VERSION]
#
# prepare  sets every crate to VERSION, turns CHANGELOG.md's "## Unreleased"
#          section into VERSION's entry, dated today and named NAME if
#          given (for example "Linux only"), writes a new minor release's
#          protocol fixtures, and commits "Release VERSION". Nothing leaves
#          this machine.
# publish  releases the commit checked out: the clean head of main, with
#          its version's CHANGELOG entry. It runs the gate, tags vVERSION,
#          builds the packages and static tarballs from the tag, installs
#          them in clean containers (tools/build.sh install-check), then
#          pushes main and the tag and makes the GitHub release, with the
#          entry as its notes and the packages, tarballs and SHA256SUMS
#          attached. A 0.x release is marked a pre-release.
#          --dry-run stops before the push and shows what would be
#          published. The tag stays, on this machine only; a later publish
#          uses it.
# notes    prints the GitHub release notes for VERSION (default: the
#          version the crates have).
#
# Needs git, and Docker or Podman as tools/build.sh does; publish also
# needs the GitHub CLI (gh), logged in with push access.
set -euo pipefail

repo=$(cd -- "$(dirname -- "$0")/.." && pwd)
cd "$repo"

die() {
  echo "release: $*" >&2
  exit 1
}

usage() {
  sed -n '3,6p' "$0" | sed 's/^# *//' >&2
  exit 1
}

# The version every crate has: the rampire crate's.
crate_version() {
  sed -n 's/^version = "\(.*\)"$/\1/p' rampire/Cargo.toml | head -1
}

# The workspace members' directories, from the root manifest.
members() {
  sed -n 's/^members = \[\(.*\)\]$/\1/p' Cargo.toml | tr -d '" ' | tr ',' '\n'
}

# The CHANGELOG entry for $1 without its heading: from "## $1" to the next
# "## ". Empty if there is none.
entry() {
  awk -v version="$1" '
    /^## / {
      if (found) exit
      heading = substr($0, 4)
      if (heading == version || index(heading, version " ") == 1 || index(heading, version ":") == 1) {
        found = 1
        next
      }
    }
    found { print }
  ' CHANGELOG.md
}

# The NAME in the "## VERSION (DATE): NAME" heading, if there is one.
entry_name() {
  awk -v version="$1" '
    index($0, "## " version " ") == 1 || index($0, "## " version ":") == 1 {
      if (split($0, parts, "): ") > 1) print parts[2]
      exit
    }
  ' CHANGELOG.md
}

has_text() {
  grep -q '[^[:space:]]' <<< "$1"
}

on_clean_main() {
  [[ $(git rev-parse --abbrev-ref HEAD) == main ]] || die 'releases are made from main'
  [[ -z $(git status --porcelain) ]] || die 'the tree has uncommitted changes'
}

# owner/name of the GitHub repository origin points at.
github_repository() {
  git remote get-url origin | sed -E 's#^(https://github.com/|git@github.com:)##; s#\.git$##'
}

# The files a release of $1 attaches, as cargo xtask package names them.
assets() {
  local version=$1
  printf '%s\n' \
    "dist/rampire-$version-1.x86_64.rpm" \
    "dist/rampire-guest-$version-1.x86_64.rpm" \
    "dist/rampire_${version}_amd64.deb" \
    "dist/rampire-guest_${version}_amd64.deb" \
    "dist/rampire-$version-x86_64-linux.tar.gz" \
    "dist/rampire-guest-$version-x86_64-linux.tar.gz" \
    dist/SHA256SUMS
}

notes() {
  local version=${1:-$(crate_version)}
  local text
  text=$(entry "$version")
  has_text "$text" || die "CHANGELOG.md has no entry for $version"
  local repository
  repository=$(github_repository)
  # The entry, its blank lines at either end trimmed and its links to files
  # in the repository pointed at them as tagged, then how to install.
  sed -e '/[^[:space:]]/,$!d' <<< "$text" | sed -e ':a' -e '/^\n*$/{$d;N;ba' -e '}' |
    sed -E "s#\]\(([^)#:][^):]*)\)#](https://github.com/$repository/blob/v$version/\1)#g"
  cat <<EOF

## Install

For Linux on x86-64. On the KVM host, with the host package from this
release:

\`\`\`bash
sudo dnf install ./rampire-$version-1.x86_64.rpm   # RHEL, AlmaLinux, Rocky Linux, Fedora
sudo apt install ./rampire_${version}_amd64.deb    # Debian
sudo rampire init
\`\`\`

The host package carries the guest package: \`rampire vm add\` installs
it in each VM. The \`rampire-guest\` packages are for installing a guest
by hand, and the static tarballs for a host or guest without a package
(\`INSTALL.md\` inside). Check what you downloaded with
\`sha256sum -c --ignore-missing SHA256SUMS\`.

[Quick start](https://github.com/$repository/blob/v$version/docs/quick-start.md) ·
[Upgrading](https://github.com/$repository/blob/v$version/docs/administration.md#upgrading) ·
[Changelog](https://github.com/$repository/blob/v$version/CHANGELOG.md)
EOF
}

prepare() {
  local version='' name=''
  while (( $# )); do
    case $1 in
      --name) name=${2:?--name needs a name}; shift 2 ;;
      -*) usage ;;
      *) [[ -z $version ]] || usage; version=$1; shift ;;
    esac
  done
  [[ $version =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || die "VERSION is MAJOR.MINOR.PATCH, got '$version'"
  on_clean_main
  local old
  old=$(crate_version)
  [[ $version != "$old" && $(printf '%s\n%s\n' "$old" "$version" | sort -V | tail -1) == "$version" ]] \
    || die "$version is not after the crates' version, $old"
  git rev-parse -q --verify "refs/tags/v$version" >/dev/null && die "v$version is already tagged"
  grep -q '^## Unreleased\(:.*\)\?$' CHANGELOG.md || die 'CHANGELOG.md has no "## Unreleased" section'
  has_text "$(entry Unreleased)" || die 'nothing is listed under "## Unreleased" in CHANGELOG.md'

  echo >&2 "==> crates $old -> $version"
  local member names=()
  for member in $(members); do
    sed -i '0,/^version = ".*"$/s//version = "'"$version"'"/' "$member/Cargo.toml"
    names+=("$(sed -n 's/^name = "\(.*\)"$/\1/p' "$member/Cargo.toml" | head -1)")
  done
  awk -v names=" ${names[*]} " -v version="$version" '
    /^name = "/ { name = $3; gsub(/"/, "", name); member = index(names, " " name " ") > 0 }
    member && /^version = "/ { $0 = "version = \"" version "\""; member = 0 }
    { print }
  ' Cargo.lock > Cargo.lock.new
  mv Cargo.lock.new Cargo.lock

  echo >&2 "==> CHANGELOG.md: Unreleased -> $version"
  local heading today
  today=$(date +%F)
  heading="## $version ($today)"
  if [[ -z $name ]]; then
    name=$(sed -n 's/^## Unreleased: \(.*\)$/\1/p' CHANGELOG.md | head -1)
  fi
  [[ -n $name ]] && heading+=": $name"
  awk -v heading="$heading" '
    !done && /^## Unreleased(:.*)?$/ { print heading; done = 1; next }
    { print }
  ' CHANGELOG.md > CHANGELOG.md.new
  mv CHANGELOG.md.new CHANGELOG.md

  # A controller serves guests of its own minor release and the one
  # before, each checked against the documents that release kept.
  local minor=${version%.*}
  if [[ $minor != "${old%.*}" && ! -d fixtures/protocol/$minor ]]; then
    tools/build.sh fixtures
    [[ -d fixtures/protocol/$minor ]] || die "no fixtures/protocol/$minor was written"
  fi

  git add Cargo.lock CHANGELOG.md fixtures/protocol
  for member in $(members); do git add "$member/Cargo.toml"; done
  git commit -q -m "Release $version"
  echo >&2 "==> committed: $(git log -1 --format='%h %s')"
  echo >&2 "next: tools/release.sh publish --dry-run, then tools/release.sh publish"
}

publish() {
  local dry_run=0
  case ${1:-} in
    --dry-run) dry_run=1; shift ;;
  esac
  (( $# == 0 )) || usage
  on_clean_main
  command -v gh >/dev/null || die 'publish needs the GitHub CLI, gh'
  local version tag notes_file
  version=$(crate_version)
  tag=v$version
  has_text "$(entry Unreleased)" \
    && die "CHANGELOG.md lists unreleased changes: run tools/release.sh prepare first"
  notes_file=$(mktemp)
  trap 'rm -f "$notes_file"' EXIT
  notes "$version" > "$notes_file"

  echo >&2 "==> checking $tag against origin and GitHub"
  git fetch -q origin main
  git merge-base --is-ancestor origin/main HEAD || die 'main is behind origin/main: pull first'
  local head remote
  head=$(git rev-parse HEAD)
  # The tag's commit on origin: an annotated tag lists its commit last.
  remote=$(git ls-remote origin "refs/tags/$tag" "refs/tags/$tag^{}" | awk '{ commit = $1 } END { print commit }')
  [[ -z $remote || $remote == "$head" ]] || die "$tag is already on origin, at another commit"
  gh release view "$tag" >/dev/null 2>&1 && die "GitHub already has the release $tag"

  tools/build.sh
  if git rev-parse -q --verify "refs/tags/$tag" >/dev/null; then
    [[ $(git rev-parse "$tag^{commit}") == "$head" ]] || die "$tag is tagged at another commit"
  else
    git tag -a "$tag" -m "RAMpire $version"
    echo >&2 "==> tagged $tag"
  fi
  local built
  built=$(tools/build.sh package --print-version | tail -1)
  [[ $built == "$version" ]] || die "the build would be version $built, not $version"
  tools/build.sh package --static
  tools/build.sh install-check

  local files=() file
  mapfile -t files < <(assets "$version")
  for file in "${files[@]}"; do
    [[ -f $file ]] || die "the build made no $file"
  done
  (cd dist && sha256sum --quiet -c SHA256SUMS) || die 'dist/SHA256SUMS does not match the files'

  local title="RAMpire $version" name
  name=$(entry_name "$version")
  [[ -n $name ]] && title+=" ($name)"
  local flags=(--verify-tag --title "$title" --notes-file "$notes_file")
  [[ $version == 0.* ]] && flags+=(--prerelease)

  if (( dry_run )); then
    echo >&2 "==> dry run: would push main and $tag to origin, then publish:"
    echo >&2 "    $title$([[ $version == 0.* ]] && echo ', a pre-release')"
    printf >&2 '    %s\n' "${files[@]}"
    echo >&2 "==> its notes:"
    cat "$notes_file" >&2
    return
  fi
  echo >&2 "==> pushing main and $tag"
  git push -q origin main
  git push -q origin "$tag"
  echo >&2 "==> publishing $title"
  gh release create "$tag" "${files[@]}" "${flags[@]}"
}

case ${1:-} in
  prepare) shift; prepare "$@" ;;
  publish) shift; publish "$@" ;;
  notes) shift; (( $# <= 1 )) || usage; notes "$@" ;;
  *) usage ;;
esac
