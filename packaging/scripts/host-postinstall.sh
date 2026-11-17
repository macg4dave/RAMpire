#!/bin/sh
# After installing or upgrading the host package. The controller isn't
# enabled here: it needs `rampire init` (or a configuration and
# `rampire apply`) first. On a host whose controller is enabled, an
# upgrade records each VM's setup again in this build's form
# (`apply --records-only`; a VM that is shut off is recorded from its
# definition when an earlier build wrote its record), so no VM is refused
# every resize, then restarts the controller onto the new binary.
# Recording first means the new controller never starts on records it
# can't read. "Enabled", not "active": a controller that couldn't read
# the old records is waiting to start again, not active, and that is the
# host that most needs them recorded. Nothing else in the draft is
# applied.
set -e
if command -v systemd-sysusers >/dev/null 2>&1; then
    systemd-sysusers /usr/lib/sysusers.d/rampire.conf
fi
if [ -d /run/systemd/system ]; then
    systemctl daemon-reload
    if systemctl is-enabled --quiet rampire.service \
        || systemctl is-active --quiet rampire.service; then
        rampire --yes apply --records-only \
            || echo "rampire: some VMs' setups weren't recorded again: run sudo rampire doctor" >&2
        systemctl restart rampire.service
    fi
fi
