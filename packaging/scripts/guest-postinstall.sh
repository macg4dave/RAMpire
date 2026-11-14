#!/bin/sh
# After installing or upgrading the guest package. It is enabled on first
# install only, so an administrator's `disable` survives upgrades, and
# restarted onto the new binary every time. The request state carries over.
set -e
first=false
# Debian: "configure" with no previous version. RPM: 1 on first install.
if { [ "$1" = configure ] && [ -z "$2" ]; } || [ "$1" = 1 ]; then
    first=true
fi
# The service needs pressure stall information to return memory. RHEL's
# kernels build it in but leave it off unless booted with psi=1
# (CONFIG_PSI_DEFAULT_DISABLED). Turn it on for every installed kernel; it
# takes effect when the VM next starts, which `rampire vm add` does when it
# changes the VM's definition. An explicit psi=0 is the administrator's.
if [ ! -e /proc/pressure/memory ] \
    && grep -qs '^CONFIG_PSI=y' "/boot/config-$(uname -r)" \
    && ! grep -qw 'psi=0' /proc/cmdline \
    && command -v grubby >/dev/null 2>&1; then
    grubby --update-kernel=ALL --args=psi=1
    echo "rampire-guest: turned on pressure stall information (psi=1) for every kernel;" \
        "it takes effect when the VM next starts"
fi
if [ -d /run/systemd/system ]; then
    systemctl daemon-reload
    if $first; then
        systemctl enable rampire-guest.service
    fi
    if systemctl is-enabled --quiet rampire-guest.service; then
        systemctl restart rampire-guest.service
    fi
fi
