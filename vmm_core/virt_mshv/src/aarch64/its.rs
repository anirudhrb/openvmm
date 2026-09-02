// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Data plane for the emulated GICv3 ITS.
//!
//! The ITS control plane in `gic_its` resolves `(DeviceID, EventID)` pairs to
//! `(vCPU, LPI INTID)` translations but delivers nothing. This module turns
//! those decisions into the hypercalls that make the hypervisor deliver the
//! interrupt.
//!
//! Only emulated devices are handled. An LPI belonging to an assigned device
//! must be programmed through the device-interrupt hypercalls instead, because
//! the hypervisor's per-INTID ownership bits for the two paths are mutually
//! exclusive. That path is not implemented yet.

use crate::MshvPartitionInner;
use hvdef::HvArm64RegisterName;
use hvdef::HypercallCode;
use hvdef::hypercall;
use hvdef::hypercall::HvRegisterAssoc;
use mshv_ioctls::MshvError;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;
use vmcore::its::ItsDataPlane;
use vmcore::its::ItsInvalidate;
use vmcore::its::ItsTranslation;

/// Sentinel that turns an `ItsInv` register write from `INV` into `INVALL`.
const GIC_INVALID_INTID: u32 = !0;

/// What the hypervisor currently holds for one interrupt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Binding {
    intid: u32,
    vp_index: u32,
}

/// A single data-plane operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    /// Claim an LPI and bind it to a processor. Required before it can be
    /// asserted, and rejected if the INTID is already claimed.
    Reserve { intid: u32, vp_index: u32 },
    /// Point an already-claimed LPI at a different processor.
    SetTarget { intid: u32, vp_index: u32 },
    /// Give up a claimed LPI.
    Release { intid: u32 },
}

/// Computes the operations that move an interrupt from `old` to `new`.
///
/// Split out from the hypercalls so the ordering rules can be tested directly.
/// The one that matters is release-before-reserve: reserving fails while the
/// INTID is still claimed, so a change of INTID cannot be expressed as anything
/// shorter than a release followed by a reserve.
fn plan(old: Option<Binding>, new: Option<ItsTranslation>) -> impl Iterator<Item = Op> {
    let ops = match (old, new) {
        (None, None) => [None, None],

        (None, Some(new)) => [
            Some(Op::Reserve {
                intid: new.intid,
                vp_index: new.vp_index,
            }),
            None,
        ],

        (Some(old), None) => [Some(Op::Release { intid: old.intid }), None],

        (Some(old), Some(new)) if old.intid == new.intid => {
            if old.vp_index == new.vp_index {
                // Re-resolving a subscribed interrupt reports it even when
                // nothing moved; issuing a hypercall for that would be pure
                // overhead.
                [None, None]
            } else {
                [
                    Some(Op::SetTarget {
                        intid: new.intid,
                        vp_index: new.vp_index,
                    }),
                    None,
                ]
            }
        }

        (Some(old), Some(new)) => [
            Some(Op::Release { intid: old.intid }),
            Some(Op::Reserve {
                intid: new.intid,
                vp_index: new.vp_index,
            }),
        ],
    };

    ops.into_iter().flatten()
}

/// Input for `HvCallSetVpRegisters` with a single register element.
///
/// `HvCallSetVpRegisters` is a rep hypercall: a fixed header followed by one
/// element per register.
#[repr(C)]
struct SetItsInv {
    header: hypercall::GetSetVpRegisters,
    element: HvRegisterAssoc,
}

/// Programs the hypervisor on behalf of the emulated ITS.
pub struct MshvItsDataPlane {
    partition: Arc<MshvPartitionInner>,
    bindings: Mutex<HashMap<(u32, u32), Binding>>,
}

impl MshvItsDataPlane {
    pub(crate) fn new(partition: Arc<MshvPartitionInner>) -> Self {
        Self {
            partition,
            bindings: Mutex::new(HashMap::new()),
        }
    }

    fn reserve(&self, intid: u32, vp_index: u32) -> Result<(), MshvError> {
        let input = hypercall::ReserveVirtualInterrupt {
            partition_id: 0,
            interrupt_id: intid,
            vp_index,
            vtl: 0,
            rsvd0: 0,
            rsvd1: 0,
            rsvd2: 0,
        };
        self.simple_hvcall(HypercallCode::HvCallReserveVirtualInterrupt, &input)
    }

    fn set_target(&self, intid: u32, vp_index: u32) -> Result<(), MshvError> {
        let input = hypercall::SetVirtualInterruptTarget {
            partition_id: 0,
            interrupt_id: intid,
            vp_index,
            vtl: 0,
            rsvd0: 0,
            rsvd1: 0,
            rsvd2: 0,
        };
        self.simple_hvcall(HypercallCode::HvCallSetVirtualInterruptTarget, &input)
    }

    fn release(&self, intid: u32) -> Result<(), MshvError> {
        let input = hypercall::ReleaseVirtualInterrupt {
            partition_id: 0,
            interrupt_id: intid,
            vtl: 0,
            rsvd0: 0,
            rsvd1: 0,
        };
        self.simple_hvcall(HypercallCode::HvCallReleaseVirtualInterrupt, &input)
    }

    /// Issues a fixed-size hypercall with no output.
    ///
    /// `partition_id` is left zero: the kernel overwrites the first `u64` of
    /// the input with the partition the file descriptor refers to, which is
    /// what stops a VMM targeting a partition it does not own.
    fn simple_hvcall<T>(&self, code: HypercallCode, input: &T) -> Result<(), MshvError> {
        let mut args = mshv_bindings::mshv_root_hvcall {
            code: code.0,
            in_sz: size_of::<T>() as u16,
            in_ptr: std::ptr::from_ref(input) as u64,
            ..Default::default()
        };
        self.partition.vmfd.hvcall(&mut args)
    }
}

impl ItsDataPlane for MshvItsDataPlane {
    fn retarget(&self, device_id: u32, event_id: u32, translation: Option<ItsTranslation>) {
        let key = (device_id, event_id);
        let mut bindings = self.bindings.lock();
        let old = bindings.get(&key).copied();

        // What the hypervisor holds after this call, which is not necessarily
        // what was asked for: the guest chooses both the INTID and the target
        // processor, so any of these can legitimately fail.
        let mut bound = old;

        for op in plan(old, translation) {
            match op {
                Op::Reserve { intid, vp_index } => {
                    // A reservation for an INTID that another interrupt still
                    // holds cannot succeed. The usual cause is not a misbehaving
                    // guest but `vits_core` reporting a batch of translation
                    // changes in hash order, so the reserve for a reused INTID
                    // can arrive before the release that frees it. Name both
                    // pairs, because the resulting lost interrupt is otherwise
                    // extremely hard to attribute.
                    let conflict = bindings
                        .iter()
                        .find(|(other, b)| **other != key && b.intid == intid)
                        .map(|(other, _)| *other);
                    if let Some((other_device, other_event)) = conflict {
                        tracelimit::error_ratelimited!(
                            device_id,
                            event_id,
                            other_device,
                            other_event,
                            intid,
                            "its: reserving an LPI still held by another interrupt; \
                             expect this reservation to fail"
                        );
                    }

                    match self.reserve(intid, vp_index) {
                        Ok(()) => bound = Some(Binding { intid, vp_index }),
                        Err(err) => {
                            tracelimit::error_ratelimited!(
                                device_id,
                                event_id,
                                intid,
                                vp_index,
                                error = &err as &dyn std::error::Error,
                                "its: failed to reserve LPI; the interrupt will not be delivered"
                            );
                            bound = None;
                        }
                    }
                }

                Op::SetTarget { intid, vp_index } => match self.set_target(intid, vp_index) {
                    Ok(()) => bound = Some(Binding { intid, vp_index }),
                    Err(err) => {
                        // Leave the recorded binding alone: the interrupt is
                        // still reserved, just aimed at the previous processor.
                        tracelimit::error_ratelimited!(
                            device_id,
                            event_id,
                            intid,
                            vp_index,
                            error = &err as &dyn std::error::Error,
                            "its: failed to retarget LPI; it stays on its old processor"
                        );
                    }
                },

                Op::Release { intid } => {
                    // Drop the binding whether or not the hypercall succeeds.
                    // Holding on to it would leave the INTID permanently
                    // unusable, since every later reserve would be treated as a
                    // conflict with an entry nothing can clear.
                    if let Err(err) = self.release(intid) {
                        tracelimit::warn_ratelimited!(
                            device_id,
                            event_id,
                            intid,
                            error = &err as &dyn std::error::Error,
                            "its: failed to release LPI"
                        );
                    }
                    bound = None;
                }
            }
        }

        match bound {
            Some(binding) => bindings.insert(key, binding),
            None => bindings.remove(&key),
        };
    }

    fn invalidate(&self, vp_index: u32, target: ItsInvalidate) {
        let value = match target {
            ItsInvalidate::Interrupt(intid) => intid,
            ItsInvalidate::Collection(_) => GIC_INVALID_INTID,
        };

        // HvCallSetVpRegisters is a rep hypercall: a fixed header followed by
        // one element per register.
        let input = SetItsInv {
            header: hypercall::GetSetVpRegisters {
                partition_id: 0,
                vp_index,
                target_vtl: hypercall::HvInputVtl::new(),
                rsvd: [0; 3],
            },
            element: (HvArm64RegisterName::ItsInv, value as u64).into(),
        };

        let mut args = mshv_bindings::mshv_root_hvcall {
            code: HypercallCode::HvCallSetVpRegisters.0,
            reps: 1,
            in_sz: size_of::<SetItsInv>() as u16,
            in_ptr: std::ptr::from_ref(&input) as u64,
            ..Default::default()
        };

        if let Err(err) = self.partition.vmfd.hvcall(&mut args) {
            // The hypervisor re-reads the guest's LPI configuration table
            // through this processor's redistributor, so a failure means the
            // guest's change to an enable bit or priority has not taken effect.
            tracelimit::error_ratelimited!(
                vp_index,
                value,
                error = &err as &dyn std::error::Error,
                "its: failed to notify the hypervisor of an LPI config change"
            );
        }
    }

    fn assert(&self, device_id: u32, event_id: u32) {
        let binding = self.bindings.lock().get(&(device_id, event_id)).copied();

        let Some(binding) = binding else {
            // Reachable whenever a device signals an MSI for a vector the guest
            // has not mapped, which the guest controls.
            tracelimit::warn_ratelimited!(
                device_id,
                event_id,
                "its: interrupt signalled before it was mapped"
            );
            return;
        };

        if let Err(err) = self.partition.assert_virtual_interrupt(binding.intid, true) {
            tracelimit::error_ratelimited!(
                device_id,
                event_id,
                intid = binding.intid,
                error = &err as &dyn std::error::Error,
                "its: failed to assert LPI"
            );
        }
    }

    fn reset(&self) {
        // Take the map first so nothing is left claimed if a release fails
        // partway through. A binding that stays in the map after reset would
        // make its INTID permanently unusable, since every later reserve would
        // be reported as conflicting with an entry nothing can clear.
        let bindings = std::mem::take(&mut *self.bindings.lock());

        for ((device_id, event_id), binding) in bindings {
            if let Err(err) = self.release(binding.intid) {
                tracelimit::warn_ratelimited!(
                    device_id,
                    event_id,
                    intid = binding.intid,
                    error = &err as &dyn std::error::Error,
                    "its: failed to release LPI during reset"
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn translation(intid: u32, vp_index: u32) -> ItsTranslation {
        ItsTranslation { intid, vp_index }
    }

    fn binding(intid: u32, vp_index: u32) -> Binding {
        Binding { intid, vp_index }
    }

    fn plan_ops(old: Option<Binding>, new: Option<ItsTranslation>) -> Vec<Op> {
        plan(old, new).collect()
    }

    #[test]
    fn mapping_a_new_interrupt_reserves_it() {
        assert_eq!(
            plan_ops(None, Some(translation(8192, 3))),
            [Op::Reserve {
                intid: 8192,
                vp_index: 3
            }]
        );
    }

    #[test]
    fn unmapping_releases_the_lpi() {
        assert_eq!(
            plan_ops(Some(binding(8192, 3)), None),
            [Op::Release { intid: 8192 }]
        );
    }

    #[test]
    fn moving_to_another_processor_only_retargets() {
        assert_eq!(
            plan_ops(Some(binding(8192, 3)), Some(translation(8192, 5))),
            [Op::SetTarget {
                intid: 8192,
                vp_index: 5
            }]
        );
    }

    /// Reserving fails while the old INTID is still claimed, so the release has
    /// to come first. This ordering is the whole reason `plan` exists.
    #[test]
    fn changing_the_lpi_releases_before_reserving() {
        assert_eq!(
            plan_ops(Some(binding(8192, 3)), Some(translation(8300, 3))),
            [
                Op::Release { intid: 8192 },
                Op::Reserve {
                    intid: 8300,
                    vp_index: 3
                }
            ]
        );
    }

    /// A single MAPC re-resolves every interrupt in the collection, most of
    /// which have not actually moved.
    #[test]
    fn an_unchanged_translation_does_nothing() {
        assert!(plan_ops(Some(binding(8192, 3)), Some(translation(8192, 3))).is_empty());
    }

    #[test]
    fn nothing_to_nothing_does_nothing() {
        assert!(plan_ops(None, None).is_empty());
    }

    /// The hypervisor's input structs are transcribed by hand from its headers,
    /// and a mismatch would surface only as an opaque failure on real hardware.
    #[test]
    fn hypercall_inputs_match_the_hypervisor_layout() {
        assert_eq!(size_of::<hypercall::ReserveVirtualInterrupt>(), 24);
        assert_eq!(size_of::<hypercall::SetVirtualInterruptTarget>(), 24);
        assert_eq!(size_of::<hypercall::ReleaseVirtualInterrupt>(), 16);

        // Header plus exactly one register element.
        assert_eq!(
            size_of::<SetItsInv>(),
            size_of::<hypercall::GetSetVpRegisters>() + size_of::<HvRegisterAssoc>()
        );
    }
}
