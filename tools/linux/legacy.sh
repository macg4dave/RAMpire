#!/bin/bash
# Finds what an install from before the RAMpire clean break left behind on a
# host or a Linux guest: the virtio-mem-pool names in docs/names.md, the
# parked VM files, and libvirt domains whose definition still names the old
# control channel port. RAMpire itself has no code for the old names
# (decision D9), so this development tool does the one-time cleanup.
#
# Usage: legacy.sh [list|remove] [--apply] [--clear-scratch]
#   list             print each leftover and what it is (default; changes nothing)
#   remove           print what would be removed; with --apply, remove it
#   --clear-scratch  also take everything else in ~/scratch
#
# remove covers files, directories, units, the polkit rule, the old service
# account, orphaned parked files and old-name working folders, and, once
# the package is installed, a development install's unit, polkit rule and
# backups (restarting the controller onto the package's unit). A checkout of
# this repository still in a folder under the old name is cloned again as
# rampire/, keeping its untracked site/, and the old folder deleted. It never
# undefines a libvirt domain or deletes a disk: list reports those, and the
# owner decides about each VM.
#
# Run on the machine itself, or over SSH from a checkout:
#   ssh HOST 'bash -s' -- list < tools/linux/legacy.sh
# Reading /etc and /var/lib needs root; the script uses sudo -n when it is
# not root and says so if that fails.
set -u

MODE=list
APPLY=0
CLEAR_SCRATCH=0
while [ $# -gt 0 ]; do
  case "$1" in
    list|remove) MODE=$1; shift ;;
    --apply) APPLY=1; shift ;;
    --clear-scratch) CLEAR_SCRATCH=1; shift ;;
    -h|--help) sed -n '2,23p' "$0" 2>/dev/null | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done
[ "$MODE" = remove ] || [ "$APPLY" = 0 ] || { echo "--apply only goes with remove" >&2; exit 2; }

if [ "$(id -u)" = 0 ]; then
  SUDO=()
elif sudo -n true 2>/dev/null; then
  SUDO=(sudo -n)
else
  echo "note: no passwordless sudo; root-only paths may be missed" >&2
  SUDO=()
fi
run() { "${SUDO[@]}" "$@"; }

FOUND=0
REMOVABLE=()
found() { # KIND PATH-OR-NAME NOTE [removable]
  FOUND=$((FOUND + 1))
  printf '%-8s %-58s %s\n' "$1" "$2" "$3"
  [ "${4:-}" = removable ] && REMOVABLE+=("$1|$2")
}

# Paths from the "Now" column of docs/names.md, host and guest.
PATHS=(
  /usr/local/libexec/virtio-mem-host
  /usr/local/libexec/virtio-mem-guest
  /usr/libexec/virtio-mem-guest
  /usr/local/bin/virtio-mem-workload
  /usr/local/bin/virtio-mem-watch-ram
  /usr/bin/virtio-mem-watch-ram
  /etc/virtio-mem-host
  /var/lib/virtio-mem-host
  /var/lib/virtio-mem-guest
  /run/virtio-mem-host
  /etc/polkit-1/rules.d/50-virtio-mem-host.rules
)
for p in "${PATHS[@]}"; do
  run test -e "$p" && found path "$p" "old name" removable
done

# Units under the old names, in every unit directory.
for dir in /etc/systemd/system /usr/lib/systemd/system /lib/systemd/system; do
  while IFS= read -r unit; do
    [ -n "$unit" ] && found unit "$unit" "old unit" removable
  done < <(run find "$dir" -maxdepth 2 -name 'virtio-mem-*' 2>/dev/null)
done
while IFS= read -r line; do
  [ -n "$line" ] && found loaded "${line%% *}" "loaded in systemd"
done < <(systemctl list-units --all --no-legend --plain 'virtio-mem-*' 2>/dev/null)

# A development install from before packages (the removed xtask
# host-deploy): once the package is installed, its unit and polkit rule in
# /etc hide the package's own, and its backups are of nothing in use.
if rpm -q rampire >/dev/null 2>&1 ||
   dpkg-query -W -f '${Status}' rampire 2>/dev/null | grep -q installed; then
  run test -f /etc/systemd/system/rampire.service && found devinstall     /etc/systemd/system/rampire.service "hides the package's unit" removable
  run test -f /etc/polkit-1/rules.d/50-rampire.rules && found devinstall     /etc/polkit-1/rules.d/50-rampire.rules "duplicates the package's rule" removable
fi
run test -d /var/lib/rampire/deployment-backups && found devinstall   /var/lib/rampire/deployment-backups "host-deploy's backups" removable

# The old service account.
getent passwd virtio-mem-host >/dev/null && found account virtio-mem-host "old user" removable
getent group virtio-mem-host >/dev/null && found group virtio-mem-host "old group" removable

# Old guest package, on a Debian-family guest.
if command -v dpkg-query >/dev/null &&
   dpkg-query -W -f '${Status}' virtio-mem-guest 2>/dev/null | grep -q installed; then
  found package virtio-mem-guest "old guest .deb (remove with apt)"
fi

# Parked VM files from O1.11, and the backup of the old install.
# A parked file whose domain still exists is what `rampire vm add` takes
# back, so it stays; one whose domain is gone is an orphan.
if run test -d /etc/rampire/parked; then
  for f in $(run ls /etc/rampire/parked); do
    p=/etc/rampire/parked/$f
    case "$f" in
      *.toml)
        vm=${f%.toml}
        if command -v virsh >/dev/null && run virsh -c qemu:///system dominfo "$vm" >/dev/null 2>&1; then
          found parked "$p" "kept: domain exists, restore with rampire vm add $vm"
        else
          found parked "$p" "orphan: no domain $vm" removable
        fi ;;
      README.md) found parked "$p" "hand procedure, replaced by rampire vm add" removable ;;
      *) found parked "$p" "unknown file" ;;
    esac
  done
fi
for home in /root /home/*; do
  for p in "$home"/scratch/old-install-*; do
    run test -e "$p" && found path "$p" "backup of the old install" removable
  done
  for p in "$home"/scratch/virtio-mem-*; do
    run test -e "$p" && found path "$p" "old-name working folder, $(run du -sh "$p" | cut -f1)" removable
  done
  # Anything else in ~/scratch is hand work from earlier sessions.
  if run test -d "$home/scratch"; then
    others=()
    for p in "$home"/scratch/*; do
      case "$(basename "$p")" in old-install-*|virtio-mem-*|'*') continue ;; esac
      others+=("$p")
    done
    if [ "$CLEAR_SCRATCH" = 1 ]; then
      for p in "${others[@]}"; do found scratch "$p" "$(run du -sh "$p" | cut -f1)" removable; done
    elif [ "${#others[@]}" -gt 0 ]; then
      found scratch "$home/scratch" "${#others[@]} other entries (--clear-scratch removes them)"
    fi
  fi
  for p in "$home"/virtio-mem-pool "$home"/github/virtio-mem-pool; do
    run test -d "$p" || continue
    origin=$(git -C "$p" remote get-url origin 2>/dev/null)
    head=$(git -C "$p" log -1 --format='%h %cs' 2>/dev/null)
    dirty=$(git -C "$p" status --porcelain 2>/dev/null | wc -l)
    case "$origin" in
      *[Rr][Aa][Mm]pire*)
        found reclone "$p" "this repo under the old folder name ($head, $dirty changed): clone again as $(dirname "$p")/rampire, keeping site/" removable ;;
      *) found path "$p" "old checkout: ${origin:-no origin}, $head, $dirty changed files" removable ;;
    esac
  done
done

# libvirt domains: report every one, flag the old port. Never removed here.
if command -v virsh >/dev/null; then
  while IFS= read -r dom; do
    [ -n "$dom" ] || continue
    state=$(run virsh -c qemu:///system domstate "$dom" 2>/dev/null | head -1)
    xml=$(run virsh -c qemu:///system dumpxml --inactive "$dom" 2>/dev/null)
    note="domain, $state"
    grep -q 'org\.virtio-mem\.control' <<<"$xml" && note="$note, old control port"
    grep -q 'org\.rampire\.control' <<<"$xml" && note="$note, rampire port"
    grep -q 'rampire-testvm\|testvm' <<<"$xml" && note="$note, made by testvm"
    run test -e "/etc/rampire/vms.d/$dom.toml" && note="$note, managed"
    found domain "$dom" "$note"
  done < <(run virsh -c qemu:///system list --all --name 2>/dev/null)
fi

[ "$FOUND" = 0 ] && echo "nothing left from before the clean break"

if [ "$MODE" = remove ]; then
  echo
  [ "${#REMOVABLE[@]}" = 0 ] && { echo "nothing to remove"; exit 0; }
  for item in "${REMOVABLE[@]}"; do
    kind=${item%%|*}; target=${item#*|}
    verb=remove; [ "$kind" = reclone ] && verb=reclone
    if [ "$APPLY" = 0 ]; then
      echo "would $verb $kind $target"; continue
    fi
    case "$kind" in
      reclone)
        # Clone first, as the checkout's owner, then carry the untracked
        # site/ configuration across, then delete the old folder.
        dest=$(dirname "$target")/rampire
        owner=$(stat -c %U "$target")
        as_owner=(); [ "$(id -un)" = "$owner" ] || as_owner=("${SUDO[@]}" -u "$owner")
        url=$(git -C "$target" remote get-url origin)
        if run test -e "$dest"; then echo "failed: $dest already exists" >&2; false
        elif "${as_owner[@]}" git clone --quiet "$url" "$dest"; then
          { ! test -d "$target/site" || "${as_owner[@]}" cp -a "$target/site" "$dest/site"; } &&
            run rm -rf -- "$target"
        else false; fi ;;
      unit)
        name=$(basename "$target")
        run systemctl disable --now "$name" 2>/dev/null
        run rm -rf -- "$target" ;;
      path|parked|scratch) run rm -rf -- "$target" ;;
      devinstall) run rm -rf -- "$target" && restart=1 ;;
      account) run userdel virtio-mem-host ;;
      group) getent group virtio-mem-host >/dev/null && run groupdel virtio-mem-host ;;
    esac && echo "${verb}d $kind $target" || echo "failed: $kind $target" >&2
  done
  [ "$APPLY" = 1 ] && run systemctl daemon-reload
  # A running controller moves onto the package's unit.
  if [ "${restart:-0}" = 1 ] && systemctl is-active --quiet rampire.service; then
    run systemctl restart rampire.service && echo "restarted rampire.service on the package's unit"
  fi
  [ "$APPLY" = 0 ] && echo "(dry run: add --apply to remove)"
fi
exit 0
