#!/bin/bash
# CI ONLY: real-Snapper end-to-end run of contrib/ghostvolumes-snapshot on
# the GitHub Ubuntu runner (sudo, real systemd + D-Bus activating
# snapperd), on a loop-mounted throwaway BTRFS image. Snapper comes from
# the openSUSE Build Service repo for Ubuntu (key-verified), since Ubuntu
# itself ships 0.10.6 and the snbk generation (>= 0.12) is required.
# Refuses to run anywhere else. Expects ./target/release/ghostvolumes.
set -euo pipefail
[ "${GITHUB_ACTIONS:-}" = true ] || { echo "refusing: CI only" >&2; exit 1; }

repo=https://download.opensuse.org/repositories/filesystems:/snapper/xUbuntu_$(. /etc/os-release && echo "$VERSION_ID")
# The OBS filesystems:snapper project key, pinned as first seen (2026-10-08;
# the same key signs the project's xUbuntu_22.04 and _24.04 repos).
OBS_FPR=2A658CFA6380FA92EAE4A854248BF9082524EEFD
curl -fsSL -o "$RUNNER_TEMP/obs.key" "$repo/Release.key"
[ "$(gpg --show-keys --with-colons "$RUNNER_TEMP/obs.key" | awk -F: '$1 == "fpr" {print $10; exit}')" = "$OBS_FPR" ] ||
  { echo "OBS key fingerprint mismatch" >&2; exit 1; }
gpg --dearmor <"$RUNNER_TEMP/obs.key" | sudo tee /usr/share/keyrings/snapper-obs.gpg >/dev/null
echo "deb [signed-by=/usr/share/keyrings/snapper-obs.gpg] $repo/ /" | sudo tee /etc/apt/sources.list.d/snapper-obs.list >/dev/null
sudo apt-get update -qq
sudo apt-get install -y -qq snapper btrfs-progs >/dev/null
ver=$(snapper --version | awk '{print $2; exit}')
echo "snapper $ver"
printf '%s\n' 0.12 "$ver" | sort -V -C || { echo "snapper $ver < 0.12" >&2; exit 1; }

sudo useradd -m dev
truncate -s 1G "$RUNNER_TEMP/disk.img"
loop=$(sudo losetup -f --show "$RUNNER_TEMP/disk.img")
trap 'sudo umount /home/dev/src 2>/dev/null || true; sudo losetup -d "$loop" || true' EXIT
sudo mkfs.btrfs -q "$loop"
sudo mkdir -p /home/dev/src && sudo mount "$loop" /home/dev/src && sudo chown dev: /home/dev/src
sudo snapper -c src create-config /home/dev/src
sudo snapper -c src set-config TIMELINE_CREATE=no ALLOW_USERS=dev SYNC_ACL=yes NUMBER_CLEANUP=no
sudo install -m755 target/release/ghostvolumes /usr/local/bin/ghostvolumes
sudo install -m755 contrib/ghostvolumes-snapshot /usr/local/bin/ghostvolumes-snapshot

# A clean environment: sudo keeps the runner's XDG_CONFIG_HOME (and more),
# which would point git and ghostvolumes at /home/runner/.config.
sudo -u dev env -i HOME=/home/dev USER=dev LOGNAME=dev LANG=C.UTF-8 \
  PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin \
  bash -euo pipefail -c '
  cd ~/src && mkdir p && cd p
  git init -q -b main && git config user.name t && git config user.email t@t
  printf "target/\n" > .gitignore
  printf "+ target\n" > .ghostvolumes-decisions
  mkdir -p src target && echo "fn main() {}" > src/main.rs && echo built > target/big.o
  git add . && git commit -qm init

  # The userdata column, unquoted (snapper may CSV-quote it).
  tag() { snapper --csvout -c src list --columns number,userdata | awk -F, -v n="$1" "\$1==n{print \$2}" | tr -d "\""; }
  run() { ghostvolumes-snapshot src ~/src | tee /tmp/out; }
  S=~/src/.snapshots

  run;                                     [ "$(tag 1)" = verify=rules-changed ]   # first run: all rules new
  grep -q "p/target" ~/.local/state/ghostvolumes/events.log
  rm ~/.local/state/ghostvolumes/events.log
  # (separate tests: set -e ignores failures inside && lists and after !)
  test ! -e $S/1/snapshot/p/target
  test -e $S/1/snapshot/p/src/main.rs
  test -e ~/src/p/target/big.o
  if touch $S/1/snapshot/p/x 2>/dev/null; then echo "snapshot 1 not read-only" >&2; exit 1; fi

  run; grep -q "unchanged: " /tmp/out;     [ "$(tag 2)" = verify=ok ]

  echo "//" >> src/main.rs && git commit -qam two
  run; grep -q "ok: " /tmp/out;            [ "$(tag 3)" = verify=ok ]

  # A decision change: pruned anyway, tagged and logged; the earlier snapshot keeps it.
  mkdir notes && echo mine > notes/n.md && printf "target/\nnotes/\n" > .gitignore
  printf "+ target\n+ /notes\n" > .ghostvolumes-decisions && git commit -qam notes
  run; grep -q "will prune (decisions changed): p/notes" /tmp/out
  [ "$(tag 4)" = verify=rules-changed ]
  test ! -e $S/4/snapshot/p/notes
  test -s ~/.local/state/ghostvolumes/events.log
  # Real snapper JSON was parsed and the config matched: a baseline was found.
  if grep -q "baseline unavailable" ~/.local/state/ghostvolumes/events.log; then
    cat ~/.local/state/ghostvolumes/events.log >&2; exit 1
  fi

  # A run that died after create (still writable) ...
  n=$(snapper -c src create --read-write --cleanup-algorithm timeline --description pruned --userdata verify=pending --print-number)
  run;                                     [ "$(tag "$n")" = verify=recovered ]
  test ! -e $S/$n/snapshot/p/target
  # ... and one that died after locking: kept, just tagged.
  m=$(snapper -c src create --read-only --cleanup-algorithm timeline --description pruned --userdata verify=pending --print-number)
  run;                                     [ "$(tag "$m")" = verify=recovered ]
  grep -q "note: snapshot $m was already locked" /tmp/out
  echo "snapshot-prune e2e: all checks passed"
'
