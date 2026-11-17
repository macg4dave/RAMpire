#!/bin/sh
# After installing or upgrading the host package. The controller isn't
# enabled here: it needs `rampire init` (or a configuration and
# `rampire apply`) first. An upgrade records each running VM's setup again
# in this build's form (`apply --records-only`), so a VM whose record an
# earlier build wrote isn't refused every resize, then restarts a running
# controller onto the new binary. Recording first means the new
# controller never starts on records it can't read. Nothing else in the
# draft is applied.
set -e
if command -v systemd-sysusers >/dev/null 2>&1; then
    systemd-sysusers /usr/lib/sysusers.d/rampire.conf
fi
if [ -d /run/systemd/system ]; then
    systemctl daemon-reload
    if systemctl is-active --quiet rampire.service; then
        rampire --yes apply --records-only \
            || echo "rampire: some VMs' setups weren't recorded again: run sudo rampire doctor" >&2
        systemctl restart rampire.service
    fi
fi
