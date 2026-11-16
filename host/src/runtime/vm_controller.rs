//! One VM's side of the host control loop. Each round it resolves any
//! recorded command against the device's live state, reads the guest's one
//! unresolved request, and, once the pool has granted it a ceiling, admits,
//! records and executes that request, then publishes the actual result.
//! Authority to resize comes only from the durable control state.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use rampire_core::{
    bounded_reason, CommandDirection, CommandOwnership, HostMemoryStatus, HostStatusSchema,
    MemoryOperation, MemoryOutcome, MemoryRequest, MemoryResourceNotificationState, MemoryResult,
    PoolMember, RequestAdmission, VirtioMemState, MEMORY_REQUEST_VERSION, MIN_BLOCK_SIZE_BYTES,
};

use super::{HostRuntimeError, Step};
use crate::checkpoint::{
    command_ownership, movement_result, Checkpoint, CommandIntent, DurableControlState,
};
use crate::config::HostConfig;
use crate::guest_channel::{GuestLink, SystemUnixClock, UnixClock};
use crate::host_memory::{validate_grow_headroom, HostMemorySource, GROW_ADMISSION_MARGIN_BYTES};
use crate::resize_sink::{ResizeSink, ResizeSinkError};
use crate::xml_source::MemoryStateSource;

/// One VM's observation for this round. Only `Ready` may act after the pool
/// has allocated the round.
pub(super) enum Observed {
    /// Nothing to execute this round. The pool charge and block size, from
    /// live state or, when that is unreadable, the last known charge; `None`
    /// before the VM was ever read.
    Held {
        charge: Option<(u64, u64)>,
    },
    Ready(Ready),
    Stop(HostRuntimeError),
}

/// A converged device and a guest request that may be executed.
pub(super) struct Ready {
    base_ram: u64,
    pub(super) state: VirtioMemState,
    request: MemoryRequest,
    admission: RequestAdmission,
}

/// Host memory granted to growth within one round, so two VMs growing in the
/// same round cannot both count the same available host RAM.
pub(super) struct HostBudget {
    pub(super) committed: u64,
}

/// One VM: its adapters, durable control state and process-local bookkeeping.
/// Nothing process-local grants authority to actuate; that comes only from
/// the durable control state.
pub struct VmController<G, S, R> {
    guest: G,
    state_source: S,
    resize_sink: R,
    pub(super) config: HostConfig,
    checkpoint: Checkpoint,
    pub(super) control: DurableControlState,
    /// Monotonic bound within this process, combined with the durable
    /// original start time so restarts cannot extend an operation.
    pending_since: Option<Instant>,
    /// The owned resize's last observed allocation and when it last moved.
    pending_progress: Option<(u64, Instant)>,
    operation_counter: u64,
    last_charge: Option<(u64, u64)>,
    pub(super) outstanding_growth: Option<u64>,
    /// This VM failed and is latched; it is only watched for its pool charge
    /// until the daemon restarts.
    pub(super) stopped: bool,
    unowned_divergence_reported: bool,
    /// A settle libvirt couldn't be asked for was reported; it is tried
    /// again every poll and reported once until one goes through.
    settle_unavailable_reported: bool,
    state_unavailable_reported: bool,
    /// The VM was last seen shut off, holding nothing.
    shut_off: bool,
    guest_unavailable_reported: bool,
    /// Separate from `guest_unavailable_reported`: a successful status write
    /// every poll must not re-arm the read-failure report.
    status_unpublished_reported: bool,
    health_report: Option<(Instant, u64, u64)>,
    last_observed_request: Option<rampire_core::RequestIdentity>,
}

impl<G, S, R> VmController<G, S, R> {
    /// Opens and locks the VM's checkpoint. The caller holds its device owner.
    pub fn new(
        guest: G,
        state_source: S,
        resize_sink: R,
        config: HostConfig,
        compatibility_fingerprint: String,
    ) -> Result<Self, String> {
        let (checkpoint, control) = Checkpoint::open(&config, compatibility_fingerprint)?;
        Ok(Self {
            guest,
            state_source,
            resize_sink,
            config,
            checkpoint,
            control,
            pending_since: None,
            pending_progress: None,
            operation_counter: 0,
            last_charge: None,
            outstanding_growth: None,
            stopped: false,
            unowned_divergence_reported: false,
            settle_unavailable_reported: false,
            state_unavailable_reported: false,
            shut_off: false,
            guest_unavailable_reported: false,
            status_unpublished_reported: false,
            health_report: None,
            last_observed_request: None,
        })
    }
}

impl<G, S, R> VmController<G, S, R>
where
    G: GuestLink,
    S: MemoryStateSource,
    R: ResizeSink,
{
    /// Phase one: resolve recorded state and, on a converged device, read the
    /// guest's request. Pending commands are handled here in full; they never
    /// need pool bytes because their charge already includes their target.
    pub(super) fn observe(&mut self, stop: &AtomicBool) -> Result<Observed, HostRuntimeError> {
        let state = match self.live_state() {
            Ok(state) => state,
            Err(error) if self.control.intent.is_some() => {
                self.latch(&format!(
                    "live state unavailable during an owned operation: {error}"
                ))?;
                return Ok(Observed::Held {
                    charge: self.last_charge,
                });
            }
            Err(_) if self.state_source.is_shut_off() => return Ok(self.observe_shut_off()),
            Err(error) => {
                // Without an owned operation there is nothing to protect but
                // the pool: hold at the last known charge. A VM that was off
                // and has just started holds what its device plugs as it
                // boots, which isn't known yet.
                if std::mem::take(&mut self.shut_off) {
                    self.outstanding_growth = None;
                    self.last_charge = None;
                }
                if !self.state_unavailable_reported {
                    self.log(&format!("event=live_state_unavailable reason={error}"));
                    self.state_unavailable_reported = true;
                }
                return Ok(Observed::Held {
                    charge: self.last_charge,
                });
            }
        };
        self.state_unavailable_reported = false;
        self.shut_off = false;
        let intent_target = self.intent_target();
        self.outstanding_growth = Some(state.outstanding_growth(intent_target));
        let charge = (state.pool_charge(intent_target), state.block_size_bytes);
        self.last_charge = Some(charge);
        let held = Observed::Held {
            charge: Some(charge),
        };
        self.resolve_intent(state)?;
        if state.requested_bytes != state.current_bytes {
            let step = self.observe_pending(state, stop)?;
            self.publish(state, None);
            return Ok(match step {
                Step::Wait => held,
                Step::Stop(error) => Observed::Stop(error),
            });
        }
        self.pending_since = None;
        self.pending_progress = None;
        self.unowned_divergence_reported = false;
        let (request, base_ram) = match self.guest.read() {
            Ok(envelope) => {
                self.guest_unavailable_reported = false;
                if let Some(path) = &self.config.telemetry_path {
                    let result = serde_json::to_vec(&envelope)
                        .map_err(|error| error.to_string())
                        .and_then(|bytes| {
                            crate::files::replace_file(
                                std::path::Path::new(path),
                                &bytes,
                                None,
                                false,
                            )
                            .map_err(|error| format!("write {path}: {error}"))
                        });
                    if let Err(error) = result {
                        self.log(&format!("event=optional_telemetry_failed reason={error}"));
                    }
                }
                let Some(base) = envelope
                    .memory
                    .physical_total_bytes
                    .checked_sub(state.current_bytes)
                else {
                    self.publish(state, None);
                    return Ok(held);
                };
                (envelope.request, base)
            }
            Err(error) => {
                if error != crate::guest_channel::AWAITING_GUEST && !self.guest_unavailable_reported
                {
                    self.log(&format!("event=guest_input_invalid reason={error}"));
                    self.guest_unavailable_reported = true;
                }
                (None, 0)
            }
        };
        let Some(request) = request else {
            self.publish(state, None);
            return Ok(held);
        };
        if self.last_observed_request.as_ref() != Some(&request.identity) {
            self.log(&format!(
                "event=guest_request_observed {} issued_unix_millis={}",
                describe(&request),
                request.issued_unix_millis
            ));
            self.last_observed_request = Some(request.identity.clone());
        }
        let now = self.now()?;
        let mut journal = self.control.journal.clone();
        let refusal = match journal.admit(
            request.clone(),
            &self.config.vm_name,
            &self.config.raw_telemetry_service_name,
            now,
            self.future_tolerance_millis()?,
        ) {
            // A retransmission of a resolved request: republish its result.
            Ok(RequestAdmission::Replay) if self.control.journal.result.is_some() => {
                self.publish(state, None);
                return Ok(held);
            }
            Ok(RequestAdmission::New) if self.control.latched() => format!(
                "automatic actuation is latched: {}",
                self.control.latch_reason.as_deref().unwrap_or_default()
            ),
            Ok(admission) => {
                return Ok(Observed::Ready(Ready {
                    base_ram,
                    state,
                    request,
                    admission,
                }))
            }
            Err(reason) => reason,
        };
        self.log(&format!(
            "event=controller_decision action=refuse {} reason={refusal}",
            describe(&request)
        ));
        self.publish(state, Some(MemoryResult::rejected(request, now, refusal)));
        Ok(held)
    }

    pub(super) fn stop(&mut self, error: &HostRuntimeError) {
        self.stopped = true;
        self.log(&format!(
            "event=vm_stopped reason={error}; other VMs continue, restart the daemon after recovery"
        ));
    }

    /// A stopped VM still holds memory, so the pool keeps charging it.
    pub(super) fn observe_stopped(&mut self) -> Observed {
        match self.live_state() {
            Ok(state) => {
                self.shut_off = false;
                let intent_target = self.intent_target();
                self.outstanding_growth = Some(state.outstanding_growth(intent_target));
                self.last_charge = Some((state.pool_charge(intent_target), state.block_size_bytes));
            }
            Err(_) if self.state_source.is_shut_off() => return self.observe_shut_off(),
            Err(_) => {
                if std::mem::take(&mut self.shut_off) {
                    self.outstanding_growth = None;
                    self.last_charge = None;
                }
            }
        }
        Observed::Held {
            charge: self.last_charge,
        }
    }

    /// A VM that is shut off holds no memory: nothing of the pool or the
    /// host is charged for it until it starts and is read again.
    fn observe_shut_off(&mut self) -> Observed {
        if !self.shut_off {
            self.log("event=vm_shut_off charge_bytes=0");
            self.shut_off = true;
        }
        self.state_unavailable_reported = false;
        self.outstanding_growth = Some(0);
        self.last_charge = Some((0, MIN_BLOCK_SIZE_BYTES));
        Observed::Held {
            charge: self.last_charge,
        }
    }

    /// This VM's input to the pool. Anything not executing a request asks
    /// for nothing beyond what it holds. A VM that is shut off holds
    /// nothing. An otherwise unreadable VM (crashed, or a failing virsh)
    /// keeps the memory it last held. A VM never read since startup, or
    /// started since it was last read, could hold anything, so it is
    /// charged the whole pool.
    pub(super) fn pool_member(&self, observed: &Observed, pool_bytes: u64) -> PoolMember {
        let (charged_bytes, block_bytes, wanted) = match observed {
            Observed::Held { charge } => {
                let (charged, block) = charge.unwrap_or((pool_bytes, MIN_BLOCK_SIZE_BYTES));
                (charged, block, 0)
            }
            // A stopping round never reaches allocation.
            Observed::Stop(_) => (pool_bytes, MIN_BLOCK_SIZE_BYTES, 0),
            Observed::Ready(ready) => (
                ready.state.current_bytes,
                ready.state.block_size_bytes,
                match ready.request.operation {
                    MemoryOperation::Add => ready.request.bytes.min(
                        self.config
                            .max_ram
                            .saturating_sub(ready.base_ram)
                            .saturating_sub(ready.state.current_bytes),
                    ),
                    MemoryOperation::Remove => 0,
                },
            ),
        };
        PoolMember {
            priority: self.config.priority,
            arrival_unix_millis: match observed {
                Observed::Ready(ready) => ready.request.issued_unix_millis,
                Observed::Held { .. } | Observed::Stop(_) => u64::MAX,
            },
            charged_bytes,
            desired_bytes: charged_bytes.saturating_add(wanted),
            block_bytes,
        }
    }

    fn live_state(&self) -> Result<VirtioMemState, String> {
        let state = self.state_source.memory_state()?;
        state.validate().map_err(|error| error.to_string())?;
        Ok(state)
    }

    /// Finish an intent the device has converged to and publish its actual
    /// result; latch on an intent the device never applied or contradicts.
    fn resolve_intent(&mut self, live: VirtioMemState) -> Result<(), HostRuntimeError> {
        match command_ownership(self.control.intent.as_ref(), live) {
            CommandOwnership::None
            | CommandOwnership::UnownedPending
            | CommandOwnership::OwnedPending => Ok(()),
            CommandOwnership::OwnedConvergedUnresolved => {
                let Some(intent) = self.control.intent.take() else {
                    return Ok(());
                };
                let now = self.now()?;
                let reason = match intent.direction {
                    CommandDirection::SettleShrink => {
                        Some("the guest returned only part of the removal".to_owned())
                    }
                    CommandDirection::SettleGrow => {
                        Some("the guest plugged only part of the addition".to_owned())
                    }
                    CommandDirection::Grow | CommandDirection::Shrink => None,
                };
                if let Some(result) =
                    movement_result(&self.control.journal, &intent, live, now, reason)
                {
                    self.log(&format!(
                        "event=request_completed operation_id={} {} outcome={:?} actual_bytes={}",
                        intent.operation_id,
                        describe(&result.request),
                        result.outcome,
                        result.actual_bytes
                    ));
                    self.control
                        .journal
                        .record_result(result)
                        .map_err(HostRuntimeError::Controller)?;
                }
                self.save()
            }
            CommandOwnership::RecordedNotApplied => self.latch("recorded command was not applied"),
            CommandOwnership::Conflict => self.latch("recorded command result is unknown"),
        }
    }

    fn observe_pending(
        &mut self,
        state: VirtioMemState,
        stop: &AtomicBool,
    ) -> Result<Step, HostRuntimeError> {
        let owned = self
            .control
            .intent
            .as_ref()
            .filter(|intent| intent.target_bytes == state.requested_bytes)
            .map(|intent| intent.direction);
        let Some(direction) = owned else {
            if !self.unowned_divergence_reported {
                self.log(&format!(
                    "event=ownership_conflict requested_bytes={} current_bytes={} reason=unowned_divergence_replay_suppressed",
                    state.requested_bytes, state.current_bytes
                ));
                self.unowned_divergence_reported = true;
            }
            return Ok(Step::Wait);
        };
        // A grow or shrink holds its target until the guest reaches it: no
        // general stall settle or convergence timeout. Exceptions settle it at
        // its current allocation (`partial`), when commands are allowed:
        // - a shrink while the guest reports Low memory: the held shrink
        //   occupies the guest's only request slot, so release it and let
        //   the guest ask for memory again;
        // - a shrink that has not moved for the removal settle time: the
        //   guest driver retries by itself, so settle at what it returned
        //   once the retries it was given have passed.
        // PAUSED (viomem): the Windows-only widening and shrink extension are
        // archived in archive/viomem/host-workaround/ until the viomem driver
        // bug is repaired; every guest now takes the paths above.
        let can_settle = !self.control.latched() && !self.config.observe_only;
        let settle = match direction {
            CommandDirection::Grow => false,
            CommandDirection::Shrink if can_settle => {
                if self.guest.read().ok().is_some_and(|envelope| {
                    envelope.native_pressure == Some(MemoryResourceNotificationState::Low)
                }) {
                    // Logged once while a settle waits for libvirt.
                    if !self.settle_unavailable_reported {
                        self.log("event=held_shrink_released reason=guest_low_memory");
                    }
                    true
                } else {
                    let stalled = self.command_stalled(state)
                        && self.pending_progress.is_some_and(|(_, since)| {
                            since.elapsed() >= self.config.removal_settle
                        });
                    if stalled && !self.settle_unavailable_reported {
                        self.log("event=held_shrink_released reason=removal_stalled");
                    }
                    stalled
                }
            }
            CommandDirection::Shrink => false,
            CommandDirection::SettleGrow | CommandDirection::SettleShrink => self.pending_elapsed(),
        };
        if !settle {
            self.log_pending_health(direction, state, None);
            return Ok(Step::Wait);
        }
        match direction {
            CommandDirection::Grow | CommandDirection::Shrink => {
                self.settle_short(direction, state, stop)
            }
            CommandDirection::SettleGrow => Ok(Step::Stop(self.convergence_timeout("growth")?)),
            CommandDirection::SettleShrink => Ok(Step::Stop(self.convergence_timeout("shrink")?)),
        }
    }

    // PAUSED (viomem): the Windows shrink extension that worked around the
    // virtio-win viomem removal bug is archived in
    // archive/viomem/host-workaround/ and is on pause until the driver bug is
    // repaired. Windows guests are moving to Hyper-V Dynamic Memory instead
    // (docs/roadmap.md, milestone W).

    /// Request the resize's current allocation, which completes it. The
    /// settlement gets its own deadline; failing it latches.
    fn settle_short(
        &mut self,
        direction: CommandDirection,
        state: VirtioMemState,
        stop: &AtomicBool,
    ) -> Result<Step, HostRuntimeError> {
        if stop.load(Ordering::Acquire) {
            return Ok(Step::Wait);
        }
        // What to go back to if the settle never reaches libvirt.
        let earlier = (self.control.intent.clone(), self.pending_since);
        let settle = CommandIntent {
            operation_id: self.next_operation_id(),
            started_unix_millis: self.now()?,
            direction: if direction == CommandDirection::Grow {
                CommandDirection::SettleGrow
            } else {
                CommandDirection::SettleShrink
            },
            prior_requested_bytes: state.requested_bytes,
            // The request's result measures movement from the original
            // command, so keep its starting allocation.
            prior_current_bytes: self
                .control
                .intent
                .as_ref()
                .map_or(state.current_bytes, |intent| intent.prior_current_bytes),
            target_bytes: state.current_bytes,
        };
        let operation_id = settle.operation_id.clone();
        self.control.intent = Some(settle);
        self.save()?;
        self.pending_since = None;
        let command = self
            .resize_sink
            .retarget_pending_guarded(state, state.current_bytes, stop);
        if let Err(ResizeSinkError::Unavailable(reason)) = &command {
            // Nothing ran: the device still holds the original target, which
            // the earlier intent owns. Keep it, and settle again next poll.
            // Latching here stopped a VM for hours over polkit refusing
            // connections, which says nothing about the VM.
            (self.control.intent, self.pending_since) = earlier;
            self.save()?;
            if !self.settle_unavailable_reported {
                self.settle_unavailable_reported = true;
                self.log(&format!(
                    "event=settle_deferred direction={direction:?} target_bytes={} kept_bytes={} reason={reason}",
                    state.requested_bytes, state.current_bytes
                ));
            }
            return Ok(Step::Wait);
        }
        self.settle_unavailable_reported = false;
        self.log(&format!(
            "event=resize_settled_short operation_id={operation_id} direction={direction:?} target_bytes={} kept_bytes={} command_result={command:?}",
            state.requested_bytes, state.current_bytes
        ));
        if let Err(error) = command {
            self.latch(&format!("settling a short {direction:?} failed: {error}"))?;
        }
        Ok(Step::Wait)
    }

    /// Phase two on a converged device: recheck the request at the effect
    /// boundary, grant what is free and execute it.
    pub(super) fn act<H: HostMemorySource>(
        &mut self,
        ready: Ready,
        ceiling: Option<u64>,
        host_memory: &H,
        host: &mut HostBudget,
        stop: &AtomicBool,
    ) -> Result<Step, HostRuntimeError> {
        let Ready {
            base_ram,
            state,
            request,
            admission,
        } = ready;
        let now = self.now()?;
        let admission_started = Instant::now();
        if let Err(reason) = request.validate_at(
            &self.config.vm_name,
            &self.config.raw_telemetry_service_name,
            now,
            self.future_tolerance_millis()?,
        ) {
            return self.refuse(state, request, admission, reason);
        }
        if !request.bytes.is_multiple_of(state.block_size_bytes) {
            return self.refuse(
                state,
                request,
                admission,
                "request is not a multiple of the device block size".to_owned(),
            );
        }
        let grant = match request.operation {
            MemoryOperation::Add => {
                self.growth_grant(&request, state, base_ram, ceiling, host_memory, host)?
            }
            MemoryOperation::Remove => self.removal_grant(&request, state, base_ram),
        };
        self.log(&format!(
            "event=controller_decision action=grant {} grant_bytes={grant} pool_ceiling_bytes={ceiling:?} requested_bytes={} current_bytes={}",
            describe(&request),
            state.requested_bytes,
            state.current_bytes
        ));
        if self.config.observe_only {
            return self.refuse(
                state,
                request,
                admission,
                format!("observe_only: would grant {grant} bytes"),
            );
        }
        if grant == 0 {
            let result = match request.operation {
                MemoryOperation::Add => MemoryResult {
                    outcome: MemoryOutcome::PoolEmpty,
                    actual_bytes: 0,
                    observed_unix_millis: now,
                    reason: Some("no free pool, device or host-reserve bytes remain".to_owned()),
                    request,
                },
                MemoryOperation::Remove => MemoryResult::rejected(
                    request,
                    now,
                    "the device has no plugged memory to remove".to_owned(),
                ),
            };
            return self.record_unexecuted(state, admission, result);
        }
        let (direction, target_bytes) = match request.operation {
            MemoryOperation::Add => (CommandDirection::Grow, state.current_bytes + grant),
            MemoryOperation::Remove => (CommandDirection::Shrink, state.current_bytes - grant),
        };
        if stop.load(Ordering::Acquire) {
            return Ok(Step::Wait);
        }
        let intent =
            self.record_intent(&request, admission, state, direction, target_bytes, now)?;
        let command = self.resize(state, &intent, host_memory, host, stop, admission_started);
        self.settle_command(state, request, now, direction, command)
    }

    /// What a growth may have: the least of what was asked, the guest's
    /// maximum, the device's capacity, the pool ceiling and the host's
    /// headroom, in whole device blocks. Every limit is logged.
    fn growth_grant<H: HostMemorySource>(
        &self,
        request: &MemoryRequest,
        state: VirtioMemState,
        base_ram: u64,
        ceiling: Option<u64>,
        host_memory: &H,
        host: &HostBudget,
    ) -> Result<u64, HostRuntimeError> {
        let available = host_memory
            .available_bytes()
            .map_err(HostRuntimeError::HostMemory)?;
        let caps = [
            ("request", request.bytes),
            (
                "guest_maximum",
                self.config
                    .max_ram
                    .saturating_sub(base_ram)
                    .saturating_sub(state.current_bytes),
            ),
            (
                "device_capacity",
                state
                    .maximum_target_bytes()
                    .saturating_sub(state.current_bytes),
            ),
            (
                "pool",
                ceiling.map_or(u64::MAX, |ceiling| {
                    ceiling.saturating_sub(state.current_bytes)
                }),
            ),
            (
                "host_headroom",
                available
                    .saturating_sub(host.committed)
                    .saturating_sub(self.config.host_min_headroom_bytes)
                    .saturating_sub(GROW_ADMISSION_MARGIN_BYTES),
            ),
        ];
        let free = caps.iter().map(|(_, bytes)| *bytes).min().unwrap_or(0);
        let limiting = caps
            .iter()
            .filter(|(_, bytes)| *bytes == free)
            .map(|(name, _)| *name)
            .collect::<Vec<_>>()
            .join(",");
        self.log(&format!("event=grant_limits {} available_bytes={available} committed_bytes={} reserve_bytes={} margin_bytes={} guest_maximum_bytes={} device_capacity_bytes={} pool_bytes={} host_room_bytes={} limiting_factors={limiting} alignment_remainder_bytes={}",
            describe(request), host.committed, self.config.host_min_headroom_bytes, GROW_ADMISSION_MARGIN_BYTES,
            caps[1].1, caps[2].1, caps[3].1, caps[4].1, free % state.block_size_bytes));
        Ok(free - free % state.block_size_bytes)
    }

    /// What a removal may take: what was asked, down to the guest's minimum,
    /// in whole device blocks.
    fn removal_grant(&self, request: &MemoryRequest, state: VirtioMemState, base_ram: u64) -> u64 {
        let minimum = self.config.min_ram.saturating_sub(base_ram);
        let removable = state.current_bytes.saturating_sub(minimum);
        let grant = request.bytes.min(removable);
        self.log(&format!("event=grant_limits {} minimum_device_bytes={minimum} removable_bytes={removable} limiting_factors={}", describe(request), if removable < request.bytes { "guest_minimum" } else { "request" }));
        grant - grant % state.block_size_bytes
    }

    /// Make the admission and the command intent durable together, before
    /// the command runs.
    fn record_intent(
        &mut self,
        request: &MemoryRequest,
        admission: RequestAdmission,
        state: VirtioMemState,
        direction: CommandDirection,
        target_bytes: u64,
        now: u64,
    ) -> Result<CommandIntent, HostRuntimeError> {
        let intent = CommandIntent {
            operation_id: self.next_operation_id(),
            started_unix_millis: now,
            direction,
            prior_requested_bytes: state.requested_bytes,
            prior_current_bytes: state.current_bytes,
            target_bytes,
        };
        if admission == RequestAdmission::New {
            self.control
                .journal
                .admit(
                    request.clone(),
                    &self.config.vm_name,
                    &self.config.raw_telemetry_service_name,
                    now,
                    self.future_tolerance_millis()?,
                )
                .map_err(HostRuntimeError::Controller)?;
        }
        self.control.intent = Some(intent.clone());
        self.save()?;
        Ok(intent)
    }

    /// Reserve the growth in this round's host budget, then run the recorded
    /// resize, guarded by a fresh host headroom check.
    fn resize<H: HostMemorySource>(
        &mut self,
        state: VirtioMemState,
        intent: &CommandIntent,
        host_memory: &H,
        host: &mut HostBudget,
        stop: &AtomicBool,
        admission_started: Instant,
    ) -> Result<(), ResizeSinkError> {
        let target_bytes = intent.target_bytes;
        let previously_committed = host.committed;
        let growth = target_bytes.saturating_sub(state.current_bytes);
        // Reserve before invoking a command: even a lost/error response may
        // have started growth and must constrain later members in this round.
        host.committed = host.committed.saturating_add(growth);
        self.outstanding_growth = Some(growth);
        let headroom = self.config.host_min_headroom_bytes;
        let authorize = || {
            validate_grow_headroom(
                state.current_bytes,
                target_bytes,
                host_memory
                    .available_bytes()?
                    .saturating_sub(previously_committed),
                headroom,
            )
        };
        let command_started_unix_millis = intent.started_unix_millis.saturating_add(
            u64::try_from(admission_started.elapsed().as_millis()).unwrap_or(u64::MAX),
        );
        let command_started = Instant::now();
        let command =
            self.resize_sink
                .request_resize_guarded(state, target_bytes, stop, &authorize);
        self.log(&format!(
            "event=resize_requested operation_id={} direction={:?} target_bytes={target_bytes} requested_bytes={} current_bytes={} command_started_unix_millis={command_started_unix_millis} command_elapsed_ms={} command_result={command:?}",
            intent.operation_id, intent.direction, state.requested_bytes, state.current_bytes, command_started.elapsed().as_millis()
        ));
        command
    }

    /// What the resize command's outcome leaves: a pending resize to watch,
    /// a refused request when nothing ran, or a latch when its outcome is
    /// unknown.
    fn settle_command(
        &mut self,
        state: VirtioMemState,
        request: MemoryRequest,
        now: u64,
        direction: CommandDirection,
        command: Result<(), ResizeSinkError>,
    ) -> Result<Step, HostRuntimeError> {
        match command {
            Ok(()) => {
                self.pending_since = Some(Instant::now());
                self.publish(state, None);
                Ok(Step::Wait)
            }
            // Nothing ran: release the intent and refuse the request. The
            // guest asks again, and libvirt may answer by then.
            Err(ResizeSinkError::Rejected(reason) | ResizeSinkError::Unavailable(reason)) => {
                self.control.intent = None;
                self.outstanding_growth = Some(0);
                self.record_unexecuted(
                    state,
                    RequestAdmission::Replay,
                    MemoryResult::rejected(request, now, reason),
                )
            }
            Err(error @ ResizeSinkError::CommandUnknown(_)) => {
                self.latch(&format!("resize command failed: {error}"))?;
                self.publish(state, None);
                // An unknown growth stops this VM; an unknown shrink keeps
                // observing, since it can only free memory.
                Ok(if direction == CommandDirection::Grow {
                    Step::Stop(HostRuntimeError::Resize(error.to_string()))
                } else {
                    Step::Wait
                })
            }
        }
    }

    /// A refusal at the effect boundary. Only an earlier admission needs a
    /// durable result; a new request's refusal is simply published.
    fn refuse(
        &mut self,
        state: VirtioMemState,
        request: MemoryRequest,
        admission: RequestAdmission,
        reason: String,
    ) -> Result<Step, HostRuntimeError> {
        self.log(&format!(
            "event=controller_decision action=refuse {} reason={reason}",
            describe(&request)
        ));
        let result = MemoryResult::rejected(request, self.now()?, reason);
        if admission == RequestAdmission::New {
            self.publish(state, Some(result));
            return Ok(Step::Wait);
        }
        self.record_unexecuted(state, admission, result)
    }

    /// Journal a request that ends without a device command.
    fn record_unexecuted(
        &mut self,
        state: VirtioMemState,
        admission: RequestAdmission,
        result: MemoryResult,
    ) -> Result<Step, HostRuntimeError> {
        let event =
            format!(
            "event=request_completed operation_id=none {} outcome={:?} actual_bytes={} reason={}",
            describe(&result.request), result.outcome, result.actual_bytes,
            result.reason.as_deref().unwrap_or("unspecified")
        );
        if admission == RequestAdmission::New {
            self.control
                .journal
                .admit(
                    result.request.clone(),
                    &self.config.vm_name,
                    &self.config.raw_telemetry_service_name,
                    result.observed_unix_millis,
                    self.future_tolerance_millis()?,
                )
                .map_err(HostRuntimeError::Controller)?;
        }
        self.control
            .journal
            .record_result(result)
            .map_err(HostRuntimeError::Controller)?;
        self.save()?;
        self.log(&event);
        self.publish(state, None);
        Ok(Step::Wait)
    }

    /// Latch durably and give any unresolved request an uncertain result.
    /// An existing latch keeps its original reason.
    fn latch(&mut self, reason: &str) -> Result<(), HostRuntimeError> {
        if self.control.latched() {
            return Ok(());
        }
        let reason = &bounded_reason(reason);
        self.control.latch_reason = Some(reason.to_owned());
        if let (Some(request), None) = (&self.control.journal.request, &self.control.journal.result)
        {
            let uncertain = MemoryResult {
                request: request.clone(),
                outcome: MemoryOutcome::Uncertain,
                actual_bytes: 0,
                observed_unix_millis: self.now()?,
                reason: Some(reason.to_owned()),
            };
            self.control
                .journal
                .record_result(uncertain)
                .map_err(HostRuntimeError::Controller)?;
        }
        self.save()?;
        self.log(&format!("event=actuation_latched reason={reason}"));
        Ok(())
    }

    fn save(&self) -> Result<(), HostRuntimeError> {
        self.checkpoint
            .save(&self.control)
            .map_err(HostRuntimeError::Controller)
    }

    /// Every owned operation that misses its deadline ends the same way:
    /// latch durably so a restart cannot resume actuation, then stop.
    fn convergence_timeout(
        &mut self,
        operation: &str,
    ) -> Result<HostRuntimeError, HostRuntimeError> {
        self.latch(&format!(
            "owned {operation} exceeded the convergence deadline"
        ))?;
        Ok(HostRuntimeError::ConvergenceTimeout(
            self.config.convergence_timeout,
        ))
    }

    /// Whether the owned resize has not moved for one poll, bounded by the
    /// convergence timeout.
    fn command_stalled(&mut self, state: VirtioMemState) -> bool {
        let bound = self
            .config
            .poll_interval
            .min(self.config.convergence_timeout);
        match self.pending_progress {
            Some((current, since)) if current == state.current_bytes => since.elapsed() >= bound,
            _ => {
                self.pending_progress = Some((state.current_bytes, Instant::now()));
                false
            }
        }
    }

    fn pending_elapsed(&mut self) -> bool {
        let timeout = self.config.convergence_timeout;
        let Some(started) = self
            .control
            .intent
            .as_ref()
            .map(|intent| intent.started_unix_millis)
        else {
            return true;
        };
        let durable_elapsed = SystemUnixClock.now_unix_millis().map_or(true, |now| {
            now < started || u128::from(now - started) >= timeout.as_millis()
        });
        durable_elapsed
            || self
                .pending_since
                .get_or_insert_with(Instant::now)
                .elapsed()
                >= timeout
    }

    /// Tell the guest the device state and the result for its request.
    /// Failure is logged: the guest keeps its pending request and retries.
    fn publish(&mut self, state: VirtioMemState, refusal: Option<MemoryResult>) {
        let published = SystemUnixClock.now_unix_millis().and_then(|now| {
            self.guest.publish_status(&HostMemoryStatus {
                schema: HostStatusSchema,
                version: MEMORY_REQUEST_VERSION,
                vm_name: self.config.vm_name.clone(),
                service_name: self.config.raw_telemetry_service_name.clone(),
                device_alias: self.config.alias.clone(),
                observed_unix_millis: now,
                device_size_bytes: state.size_bytes,
                block_size_bytes: state.block_size_bytes,
                requested_bytes: state.requested_bytes,
                current_bytes: state.current_bytes,
                result: refusal.or_else(|| self.control.journal.result.clone()),
                guest_settings: self.config.guest_settings,
            })
        });
        match published {
            Ok(()) => self.status_unpublished_reported = false,
            Err(error) if !self.status_unpublished_reported => {
                self.log(&format!("event=guest_status_unpublished reason={error}"));
                self.status_unpublished_reported = true;
            }
            Err(_) => {}
        }
    }

    fn log_pending_health(
        &mut self,
        direction: CommandDirection,
        state: VirtioMemState,
        floor: Option<u64>,
    ) {
        if self
            .health_report
            .is_some_and(|(when, requested, current)| {
                requested == state.requested_bytes
                    && current == state.current_bytes
                    && when.elapsed() < Duration::from_secs(30)
            })
        {
            return;
        }
        self.health_report = Some((Instant::now(), state.requested_bytes, state.current_bytes));
        let now = match SystemUnixClock.now_unix_millis() {
            Ok(now) => now,
            Err(error) => {
                self.log(&format!("event=diagnostic_clock_failed reason={error}"));
                return;
            }
        };
        let age = self
            .control
            .intent
            .as_ref()
            .map_or(0, |intent| now.saturating_sub(intent.started_unix_millis));
        let idle = self
            .pending_progress
            .map_or(0, |(_, since)| since.elapsed().as_millis());
        let timeout = match direction {
            CommandDirection::Shrink => self.config.removal_settle,
            _ => self.config.convergence_timeout,
        }
        .as_millis();
        let identity = self
            .control
            .journal
            .request
            .as_ref()
            .map_or_else(String::new, describe);
        self.log(&format!("event=control_health direction={direction:?} {identity} requested_bytes={} current_bytes={} remaining_to_device_target_bytes={} pending_age_ms={age} idle_ms={idle} idle_timeout_remaining_ms={} floor_bytes={floor:?}",
            state.requested_bytes, state.current_bytes, state.current_bytes.abs_diff(state.requested_bytes), timeout.saturating_sub(idle)));
    }

    /// The recorded command's target; zero with none.
    fn intent_target(&self) -> u64 {
        self.control
            .intent
            .as_ref()
            .map_or(0, |intent| intent.target_bytes)
    }

    fn next_operation_id(&mut self) -> String {
        self.operation_counter = self.operation_counter.saturating_add(1);
        format!("{}-{}", std::process::id(), self.operation_counter)
    }

    fn now(&self) -> Result<u64, HostRuntimeError> {
        SystemUnixClock
            .now_unix_millis()
            .map_err(HostRuntimeError::Controller)
    }

    fn future_tolerance_millis(&self) -> Result<u64, HostRuntimeError> {
        u64::try_from(self.config.raw_telemetry_future_tolerance.as_millis())
            .map_err(|_| HostRuntimeError::Controller("future tolerance is too large".to_owned()))
    }

    fn log(&self, message: &str) {
        eprintln!(
            "rampire: vm={} alias={} {message}",
            self.config.vm_name, self.config.alias
        );
    }
}

fn describe(request: &MemoryRequest) -> String {
    format!(
        "session_id={} sequence={} operation={:?} bytes={}",
        request.identity.session_id, request.identity.sequence, request.operation, request.bytes
    )
}
