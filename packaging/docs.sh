#!/usr/bin/env bash
# The man pages and shell completions the packages install, made from the
# binaries' own command definitions, so they say what --help says:
#
#   packaging/docs.sh OUTPUT_DIRECTORY
#
# Run from the repository root after the release build. Writes
#   man8/    rampire(8) and a page per subcommand (rampire-vm-add(8), ...)
#   man5/    rampire.toml(5), from the settings' definitions in rampire-core
#   guest/   rampire-guest(8) and rampire-watch-ram(1), from their --help
#   completions/  rampire for bash, _rampire for zsh, rampire.fish for fish
set -euo pipefail
if (( $# != 1 )); then
  echo 'usage: packaging/docs.sh OUTPUT_DIRECTORY' >&2
  exit 1
fi
out=$1
bin=target/release
rm -rf "$out"
mkdir -p "$out/man8" "$out/man5" "$out/guest" "$out/completions"

"$bin/rampire" generate man "$out/man8"
mv "$out/man8/rampire.toml.5" "$out/man5/"

"$bin/rampire" generate completions "$out/completions"
mv "$out/completions/rampire.bash" "$out/completions/rampire"

# A page from a program's own help text: NAME, then the help as it prints.
help_page() {
  local name=$1 section=$2 summary=$3
  printf '.TH %s %s "" "RAMpire" "RAMpire"\n' "${name^^}" "$section"
  printf '.SH NAME\n%s \\- %s\n.SH DESCRIPTION\n.nf\n' "$name" "$summary"
  # Backslashes, and lines that roff would take for its own requests.
  sed -e 's/\\/\\e/g' -e "s/^[.']/\\\\\&&/"
  printf '.fi\n'
}

"$bin/rampire-guest" help |
  help_page rampire-guest 8 'the guest side of RAMpire, run by rampire-guest.service' \
  > "$out/guest/rampire-guest.8"
bash tools/linux/watch-ram.sh --help |
  help_page rampire-watch-ram 1 'show the RAM a Linux guest sees, live' \
  > "$out/guest/rampire-watch-ram.1"

# Compressed as distributions ship them; -n keeps the packages reproducible.
gzip -n -9 "$out"/man8/* "$out"/man5/* "$out"/guest/*
