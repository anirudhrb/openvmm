// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! The virtual ITS chipset device: MMIO dispatch, lifecycle, and diagnostics.

use crate::data_plane::ItsDataPlane;
use crate::env::GicItsEnv;
use chipset_device::ChipsetDevice;
use chipset_device::io::IoError;
use chipset_device::io::IoResult;
use chipset_device::mmio::MmioIntercept;
use guestmem::GuestMemory;
use inspect::InspectMut;
use std::ops::RangeInclusive;
use std::sync::Arc;
use vits_core::ItsCore;
use vmcore::device_state::ChangeDeviceState;
use vmcore::save_restore::RestoreError;
use vmcore::save_restore::SaveError;
use vmcore::save_restore::SaveRestore;
use vmcore::save_restore::SavedStateNotSupported;

/// Size of the ITS MMIO window: two 64 KiB frames.
///
/// The first frame holds the `GITS_*` control registers; the second holds
/// `GITS_TRANSLATER`, the MSI doorbell.
///
/// This must agree with the value the topology uses to place the window
/// (`openvmm_defs::config::GIC_ITS_SIZE`). It is defined here rather than
/// imported so that this crate stays independent of the OpenVMM configuration
/// crates, matching how `smmu` owns its own region size.
pub const MMIO_REGION_SIZE: u64 = 0x2_0000;

/// Size of the control register frame, i.e. everything [`vits_core`] emulates.
const CONTROL_FRAME_SIZE: u32 = 0x1_0000;

/// Offset of `GITS_TRANSLATER` — the second 64 KiB frame, offset `0x40`.
const GITS_TRANSLATER_OFFSET: u32 = 0x1_0040;

// Control frame offsets, needed here only to project register state into
// `Inspect` and to recognize the command queue doorbell. `vits_core` owns the
// register semantics.
const GITS_CTLR_OFFSET: u32 = 0x0000;
const GITS_IIDR_OFFSET: u32 = 0x0004;
const GITS_TYPER_OFFSET: u32 = 0x0008;
const GITS_CBASER_OFFSET: u32 = 0x0080;
const GITS_CWRITER_OFFSET: u32 = 0x0088;
const GITS_CREADR_OFFSET: u32 = 0x0090;
const GITS_BASER0_OFFSET: u32 = 0x0100;
const GITS_BASER1_OFFSET: u32 = 0x0108;

const GITS_CTLR_ENABLED: u64 = 1 << 0;
const GITS_CTLR_QUIESCENT: u64 = 1 << 31;

/// A virtual GICv3 ITS.
///
/// Owns the guest's ITS MMIO window and drives [`vits_core::ItsCore`], which
/// holds all the actual ITS state: the register file, the command queue, and
/// the device / collection / interrupt-translation tables.
//
// DEVNOTE: `smmu` splits itself into a lock-held device and an
// `Arc<SharedState>` reachable without the chipset lock, because its per-device
// DMA and MSI wrappers must reach it on the interrupt hot path. This device
// deliberately does not, because every entry point here is guest MMIO, which
// already runs under the chipset device lock.
//
// That must be revisited when the MSI signalling path is wired up. If a
// `SignalMsi` fast path calls `ItsCore::translate_interrupt` per interrupt, it
// would take the chipset lock on every interrupt, and the translation state
// should move behind a shared-state type instead.
pub struct GicItsDevice {
    mmio_region: (&'static str, RangeInclusive<u64>),
    mmio_base: u64,
    /// Retained so that `reset` can rebuild the core from scratch.
    guest_memory: GuestMemory,
    /// Retained so that `reset` can rebuild the core from scratch, and for the
    /// MSI signalling path to share.
    programmer: Arc<dyn ItsDataPlane>,
    core: ItsCore<GicItsEnv>,
}

impl GicItsDevice {
    /// Creates a new virtual ITS with its register window at `mmio_base`.
    ///
    /// `mmio_base` must be 64 KiB aligned, and the window extends for
    /// [`MMIO_REGION_SIZE`] bytes.
    ///
    /// `guest_memory` is used to read the ITS command queue. `programmer`
    /// receives the translations the guest programs, and is responsible for
    /// making the interrupts actually arrive.
    pub fn new(
        mmio_base: u64,
        guest_memory: GuestMemory,
        programmer: Arc<dyn ItsDataPlane>,
    ) -> Self {
        Self {
            mmio_region: ("gic_its", mmio_base..=mmio_base + MMIO_REGION_SIZE - 1),
            mmio_base,
            core: ItsCore::new(GicItsEnv::new(guest_memory.clone(), programmer.clone())),
            guest_memory,
            programmer,
        }
    }

    /// Converts an absolute guest address into a window offset.
    ///
    /// The chipset only routes addresses inside the registered region, so this
    /// should never fail; it is fallible rather than a subtraction so that a
    /// routing bug cannot become an arithmetic panic.
    fn offset_of(&self, addr: u64) -> Option<u32> {
        let offset = addr.checked_sub(self.mmio_base)?;
        (offset < MMIO_REGION_SIZE).then_some(offset as u32)
    }

    /// Reads a register, resolving anything the ITS does not implement to zero.
    fn read_offset(&self, offset: u32, len: u32) -> u64 {
        if offset >= CONTROL_FRAME_SIZE {
            // `GITS_TRANSLATER` is write-only, and the rest of the second frame
            // is unimplemented. Both read as zero.
            tracelimit::warn_ratelimited!(offset, len, "its: read outside the control frame");
            return 0;
        }

        self.core.read_mmio(offset, len).unwrap_or_else(|err| {
            // Unimplemented registers must read as zero rather than fail. This
            // is what makes `GITS_BASER2`..`GITS_BASER7` report
            // `GITS_BASER_TYPE_NONE`, as the guest's probe loop expects.
            tracelimit::warn_ratelimited!(offset, len, ?err, "its: unhandled register read");
            0
        })
    }

    /// Writes a register, ignoring anything the ITS does not implement.
    fn write_offset(&mut self, offset: u32, len: u32, value: u64) {
        if offset == GITS_TRANSLATER_OFFSET {
            // `GITS_TRANSLATER` is architecturally written by devices, not by
            // guest software: emulated devices signal MSIs out of band, and
            // assigned devices are programmed with a host-side doorbell that
            // never targets this page. Honouring a guest write here would let
            // the guest inject an interrupt with a forged DeviceID, since the
            // DeviceID of a real translation comes from the bus transaction
            // rather than from the written data.
            tracelimit::warn_ratelimited!(value, "its: ignoring guest write to GITS_TRANSLATER");
            return;
        }

        if offset >= CONTROL_FRAME_SIZE {
            tracelimit::warn_ratelimited!(offset, len, "its: write outside the control frame");
            return;
        }

        let Err(err) = self.core.write_mmio(offset, len, value) else {
            return;
        };

        if offset == GITS_CWRITER_OFFSET {
            // The queue is drained synchronously from this write, so a failure
            // here means some command was rejected and `GITS_CREADR` was left
            // unadvanced. A guest waiting for command completion will spin.
            //
            // A real ITS reports this by stalling the queue via
            // `GITS_CTLR.Stalled`; `vits_core` has no error reporting path yet,
            // so for now the condition is only visible in traces.
            tracelimit::error_ratelimited!(
                ?err,
                "its: command queue processing failed; the guest may hang waiting for completion"
            );
        } else {
            tracelimit::warn_ratelimited!(
                offset,
                len,
                value,
                ?err,
                "its: unhandled register write"
            );
        }
    }

    /// Reads a known-good control register, for diagnostics only.
    fn peek(&self, offset: u32, len: u32) -> u64 {
        self.core.read_mmio(offset, len).unwrap_or(0)
    }
}

impl ChipsetDevice for GicItsDevice {
    fn supports_mmio(&mut self) -> Option<&mut dyn MmioIntercept> {
        Some(self)
    }

    // DEVNOTE: `supports_poll_device` is deliberately left at `None`. The
    // command queue is drained synchronously from the `GITS_CWRITER` write, so
    // a large batch blocks the writing vCPU. That is acceptable for a
    // configuration-path register; if it ever is not, the fix is to implement
    // `PollDevice` here rather than to change the MMIO path.
}

impl MmioIntercept for GicItsDevice {
    fn mmio_read(&mut self, addr: u64, data: &mut [u8]) -> IoResult {
        // The GIC architecture permits only 32- and 64-bit accesses to the ITS
        // register frame. An unsupported width is a property of the access
        // rather than of the device, so it is the one condition reported as an
        // error; everything else is handled internally.
        if !matches!(data.len(), 4 | 8) {
            return IoResult::Err(IoError::InvalidAccessSize);
        }

        let Some(offset) = self.offset_of(addr) else {
            tracelimit::error_ratelimited!(addr, "its: read routed outside the MMIO window");
            data.fill(0);
            return IoResult::Ok;
        };

        let len = data.len() as u32;
        let value = self.read_offset(offset, len);
        if len == 4 {
            data.copy_from_slice(&(value as u32).to_le_bytes());
        } else {
            data.copy_from_slice(&value.to_le_bytes());
        }

        IoResult::Ok
    }

    fn mmio_write(&mut self, addr: u64, data: &[u8]) -> IoResult {
        let value = match data.len() {
            4 => u32::from_le_bytes(data.try_into().unwrap()) as u64,
            8 => u64::from_le_bytes(data.try_into().unwrap()),
            _ => return IoResult::Err(IoError::InvalidAccessSize),
        };

        let Some(offset) = self.offset_of(addr) else {
            tracelimit::error_ratelimited!(addr, "its: write routed outside the MMIO window");
            return IoResult::Ok;
        };

        self.write_offset(offset, data.len() as u32, value);

        IoResult::Ok
    }

    fn get_static_regions(&mut self) -> &[(&str, RangeInclusive<u64>)] {
        std::slice::from_ref(&self.mmio_region)
    }
}

impl ChangeDeviceState for GicItsDevice {
    fn start(&mut self) {
        // Nothing to do: the ITS has no worker task, and all of its activity is
        // driven synchronously by guest MMIO.
    }

    async fn stop(&mut self) {
        // Nothing to do, for the same reason. Command queue processing
        // completes inside `mmio_write`, so the device is always in a stable
        // state once the device lock is released.
    }

    async fn reset(&mut self) {
        let GicItsDevice {
            // Fixed at construction; the window is never remapped.
            mmio_region: _,
            mmio_base: _,
            // Cheap handles retained so the core can be rebuilt.
            guest_memory,
            programmer,
            core,
        } = self;

        // Replacing the core is both simpler and more obviously correct than
        // resetting each table and register in place: every shadow table, the
        // translation cache, and the whole register file return to their
        // power-on values by construction.
        //
        // DEVNOTE: once a real data plane is wired up, this must also tear down
        // every outstanding binding, because a binding is ownership of an LPI
        // INTID in the hypervisor. Dropping the bindings here without releasing
        // them would leak that ownership across guest reboot, and the guest's
        // next attempt to map the same INTID would fail.
        *core = ItsCore::new(GicItsEnv::new(guest_memory.clone(), programmer.clone()));
    }
}

impl SaveRestore for GicItsDevice {
    // Save/restore is not supported yet, and is more than serialization
    // boilerplate when it is added. The ITS keeps shadow tables rather than
    // walking the guest's own tables, so nothing can be recovered by re-reading
    // guest memory on restore: all of it has to travel in the saved state. On
    // top of that, restore has to re-issue the data-plane programming to
    // rebuild every binding on the destination host, since the hypervisor's
    // view of which LPIs are reserved and where they target does not travel
    // with this device's state.
    type SavedState = SavedStateNotSupported;

    fn save(&mut self) -> Result<Self::SavedState, SaveError> {
        Err(SaveError::NotSupported)
    }

    fn restore(&mut self, state: Self::SavedState) -> Result<(), RestoreError> {
        match state {}
    }
}

impl InspectMut for GicItsDevice {
    fn inspect_mut(&mut self, req: inspect::Request<'_>) {
        let ctlr = self.peek(GITS_CTLR_OFFSET, 4);

        req.respond()
            .hex("mmio_base", self.mmio_base)
            .field("enabled", ctlr & GITS_CTLR_ENABLED != 0)
            .field("quiescent", ctlr & GITS_CTLR_QUIESCENT != 0)
            .hex("gits_ctlr", ctlr)
            .hex("gits_iidr", self.peek(GITS_IIDR_OFFSET, 4))
            .hex("gits_typer", self.peek(GITS_TYPER_OFFSET, 8))
            // GITS_CBASER carries the command queue base and size, and
            // CWRITER/CREADR its producer and consumer indices.
            .hex("gits_cbaser", self.peek(GITS_CBASER_OFFSET, 8))
            .hex("gits_cwriter", self.peek(GITS_CWRITER_OFFSET, 8))
            .hex("gits_creadr", self.peek(GITS_CREADR_OFFSET, 8))
            .hex("gits_baser0", self.peek(GITS_BASER0_OFFSET, 8))
            .hex("gits_baser1", self.peek(GITS_BASER1_OFFSET, 8));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use parking_lot::Mutex;
    use test_with_tracing::test;
    use vits_core::DeviceId;
    use vits_core::EventId;
    use vits_core::InvalidateTarget;
    use vits_core::Translation;

    const MMIO_BASE: u64 = 0xEFFC_0000;
    const GUEST_MEM_SIZE: usize = 0x4_0000;
    /// Where the tests place the ITS command queue in guest memory.
    const QUEUE_GPA: u64 = 0x1_0000;

    const GITS_PIDR2_OFFSET: u32 = 0xFFE8;
    const GITS_BASER2_OFFSET: u32 = 0x0110;

    const GITS_CTLR_ENABLE: u64 = 1 << 0;
    const GITS_CBASER_VALID: u64 = 1 << 63;

    /// An [`ItsDataPlane`] that records what it was asked to do.
    #[derive(Default)]
    struct RecordingDataPlane {
        retargets: Mutex<Vec<(DeviceId, EventId, Option<Translation>)>>,
        invalidations: Mutex<Vec<InvalidateTarget>>,
        asserts: Mutex<Vec<(DeviceId, EventId)>>,
    }

    impl ItsDataPlane for RecordingDataPlane {
        fn retarget(
            &self,
            device_id: DeviceId,
            event_id: EventId,
            translation: Option<Translation>,
        ) {
            self.retargets
                .lock()
                .push((device_id, event_id, translation));
        }

        fn invalidate(&self, target: InvalidateTarget) {
            self.invalidations.lock().push(target);
        }

        fn assert(&self, device_id: DeviceId, event_id: EventId) {
            self.asserts.lock().push((device_id, event_id));
        }
    }

    struct TestDevice {
        dev: GicItsDevice,
        gm: GuestMemory,
        programmer: Arc<RecordingDataPlane>,
    }

    fn new_device() -> TestDevice {
        let gm = GuestMemory::allocate(GUEST_MEM_SIZE);
        let programmer = Arc::new(RecordingDataPlane::default());
        let dev = GicItsDevice::new(MMIO_BASE, gm.clone(), programmer.clone());
        TestDevice {
            dev,
            gm,
            programmer,
        }
    }

    impl TestDevice {
        fn read32(&mut self, offset: u32) -> u32 {
            let mut data = [0; 4];
            self.dev
                .mmio_read(MMIO_BASE + offset as u64, &mut data)
                .unwrap();
            u32::from_le_bytes(data)
        }

        fn read64(&mut self, offset: u32) -> u64 {
            let mut data = [0; 8];
            self.dev
                .mmio_read(MMIO_BASE + offset as u64, &mut data)
                .unwrap();
            u64::from_le_bytes(data)
        }

        fn write32(&mut self, offset: u32, value: u32) {
            self.dev
                .mmio_write(MMIO_BASE + offset as u64, &value.to_le_bytes())
                .unwrap();
        }

        fn write64(&mut self, offset: u32, value: u64) {
            self.dev
                .mmio_write(MMIO_BASE + offset as u64, &value.to_le_bytes())
                .unwrap();
        }

        /// Writes a batch of commands to the queue and rings the doorbell.
        fn submit(&mut self, commands: &[[u64; 4]]) {
            let read_index = self.read64(GITS_CREADR_OFFSET) / 32;
            for (i, command) in commands.iter().enumerate() {
                let gpa = QUEUE_GPA + (read_index + i as u64) * 32;
                for (j, qword) in command.iter().enumerate() {
                    self.gm
                        .write_at(gpa + j as u64 * 8, &qword.to_le_bytes())
                        .unwrap();
                }
            }
            self.write64(
                GITS_CWRITER_OFFSET,
                (read_index + commands.len() as u64) * 32,
            );
        }

        /// Brings the ITS up with a one-page command queue, as a guest does.
        fn enable(&mut self) {
            self.write64(GITS_CBASER_OFFSET, QUEUE_GPA | GITS_CBASER_VALID);
            self.write32(GITS_CTLR_OFFSET, GITS_CTLR_ENABLE as u32);
        }
    }

    fn mapd(device_id: u32) -> [u64; 4] {
        [0x08 | ((device_id as u64) << 32), 0, 1 << 63, 0]
    }

    fn mapc(collection_id: u32, processor_id: u32) -> [u64; 4] {
        [
            0x09,
            0,
            (collection_id as u64) | ((processor_id as u64) << 16) | (1 << 63),
            0,
        ]
    }

    fn mapti(device_id: u32, event_id: u32, interrupt_id: u32, collection_id: u32) -> [u64; 4] {
        [
            0x0A | ((device_id as u64) << 32),
            (event_id as u64) | ((interrupt_id as u64) << 32),
            collection_id as u64,
            0,
        ]
    }

    #[test]
    fn identification_registers_read_back() {
        let mut d = new_device();

        // GITS_PIDR2.ArchRev == 0x3 identifies a GICv3 ITS.
        assert_eq!(d.read32(GITS_PIDR2_OFFSET), 0x30);
        assert_ne!(d.read32(GITS_IIDR_OFFSET), 0);
        // GITS_TYPER.Physical must be set for the guest to use physical LPIs.
        assert_eq!(d.read64(GITS_TYPER_OFFSET) & 1, 1);
        // Quiescent, not enabled.
        assert_eq!(d.read32(GITS_CTLR_OFFSET), 1 << 31);
    }

    /// A guest's ITS probe loops over all eight BASER registers looking for
    /// table types. `vits_core` implements only the first two, so the rest must
    /// read as zero (GITS_BASER_TYPE_NONE) rather than fail the access.
    #[test]
    fn unimplemented_basers_are_raz_wi() {
        let mut d = new_device();

        assert_ne!(d.read64(GITS_BASER0_OFFSET), 0);
        assert_ne!(d.read64(GITS_BASER1_OFFSET), 0);

        for i in 0..6 {
            let offset = GITS_BASER2_OFFSET + i * 8;
            assert_eq!(
                d.read64(offset),
                0,
                "GITS_BASER{} should read as zero",
                i + 2
            );
            d.write64(offset, !0);
            assert_eq!(
                d.read64(offset),
                0,
                "GITS_BASER{} should ignore writes",
                i + 2
            );
        }
    }

    /// A write to a read-only register must be ignored, not reported as a bus
    /// error — the guest is free to try.
    #[test]
    fn writes_to_read_only_registers_are_ignored() {
        let mut d = new_device();

        let typer = d.read64(GITS_TYPER_OFFSET);
        d.write64(GITS_TYPER_OFFSET, !0);
        assert_eq!(d.read64(GITS_TYPER_OFFSET), typer);

        let pidr2 = d.read32(GITS_PIDR2_OFFSET);
        d.write32(GITS_PIDR2_OFFSET, !0);
        assert_eq!(d.read32(GITS_PIDR2_OFFSET), pidr2);
    }

    /// The GIC architecture permits only 32- and 64-bit accesses to the ITS
    /// register frame. Width is the one thing reported as a bus error.
    #[test]
    fn bad_access_widths_are_rejected() {
        let mut d = new_device();

        for len in [1usize, 2, 3, 16] {
            let mut data = vec![0; len];
            assert!(matches!(
                d.dev.mmio_read(MMIO_BASE, &mut data),
                IoResult::Err(IoError::InvalidAccessSize)
            ));
            assert!(matches!(
                d.dev.mmio_write(MMIO_BASE, &data),
                IoResult::Err(IoError::InvalidAccessSize)
            ));
        }

        assert!(matches!(
            d.dev.mmio_read(MMIO_BASE, &mut [0; 4]),
            IoResult::Ok
        ));
        assert!(matches!(
            d.dev.mmio_read(MMIO_BASE, &mut [0; 8]),
            IoResult::Ok
        ));
    }

    /// Unimplemented offsets, including the whole second frame, are RAZ/WI.
    #[test]
    fn unimplemented_offsets_are_raz_wi() {
        let mut d = new_device();

        for offset in [0x0040, 0x0200, 0xF000, 0x1_0000, 0x1_8000, 0x1_FFF8] {
            assert_eq!(
                d.read64(offset),
                0,
                "offset {offset:#x} should read as zero"
            );
            d.write64(offset, !0);
            assert_eq!(
                d.read64(offset),
                0,
                "offset {offset:#x} should ignore writes"
            );
        }
    }

    /// `GITS_TRANSLATER` is architecturally written by devices, not by guest
    /// software. A guest write must not be treated as a doorbell, or the guest
    /// could inject interrupts with a forged DeviceID.
    #[test]
    fn guest_writes_to_translater_do_not_signal() {
        let mut d = new_device();
        d.enable();
        d.submit(&[mapd(0x100), mapc(1, 0), mapti(0x100, 7, 8192, 1)]);

        d.write32(GITS_TRANSLATER_OFFSET, 7);
        d.write64(GITS_TRANSLATER_OFFSET, 7);

        assert!(d.programmer.asserts.lock().is_empty());
        assert_eq!(d.read64(GITS_TRANSLATER_OFFSET), 0);
    }

    #[test]
    fn enabling_the_command_queue_processes_commands() {
        let mut d = new_device();
        d.enable();

        assert_eq!(d.read32(GITS_CTLR_OFFSET) & 1, 1);
        assert_eq!(
            d.read64(GITS_CBASER_OFFSET),
            QUEUE_GPA | GITS_CBASER_VALID,
            "GITS_CBASER should read back the programmed queue"
        );

        // The queue is only really mapped if commands out of it are consumed.
        d.submit(&[mapd(0x100)]);
        assert_eq!(d.read64(GITS_CREADR_OFFSET), 32);
        assert_eq!(d.read64(GITS_CREADR_OFFSET), d.read64(GITS_CWRITER_OFFSET));
    }

    /// The end-to-end control-plane path: a guest maps a device, a collection
    /// and a translation, and the resulting binding is reported to the data
    /// plane.
    #[test]
    fn mapping_an_interrupt_reports_a_translation() {
        let mut d = new_device();
        d.enable();

        // Subscribe first, as the MSI path does when a vector is enabled.
        d.dev.core.subscribe_interrupt(0x100, 7);
        d.programmer.retargets.lock().clear();

        d.submit(&[mapd(0x100), mapc(1, 3), mapti(0x100, 7, 8192, 1)]);

        // All commands consumed, or the guest would spin waiting.
        assert_eq!(d.read64(GITS_CREADR_OFFSET), d.read64(GITS_CWRITER_OFFSET));

        let expected = Translation {
            processor_id: 3,
            interrupt_id: 8192,
        };
        assert_eq!(*d.programmer.retargets.lock(), [(0x100, 7, Some(expected))]);
        assert_eq!(
            d.dev.core.translate_interrupt(0x100, 7),
            Some(expected),
            "the core should resolve the mapping it just reported"
        );
    }

    /// `MAPC` retargets every interrupt in a collection at once, which is the
    /// fan-out case the subscription mechanism exists to handle.
    #[test]
    fn remapping_a_collection_retargets_subscribers() {
        let mut d = new_device();
        d.enable();
        d.dev.core.subscribe_interrupt(0x100, 7);
        d.submit(&[mapd(0x100), mapc(1, 3), mapti(0x100, 7, 8192, 1)]);
        d.programmer.retargets.lock().clear();

        d.submit(&[mapc(1, 5)]);

        let expected = Translation {
            processor_id: 5,
            interrupt_id: 8192,
        };
        assert_eq!(*d.programmer.retargets.lock(), [(0x100, 7, Some(expected))]);
        assert_eq!(d.dev.core.translate_interrupt(0x100, 7), Some(expected));
    }

    /// `INV` tells the ITS that the guest's LPI configuration table changed.
    /// The ITS does not own that table, so this must reach the data plane.
    #[test]
    fn invalidation_reaches_the_data_plane() {
        let mut d = new_device();
        d.enable();
        d.submit(&[mapd(0x100), mapc(1, 3), mapti(0x100, 7, 8192, 1)]);

        // INV DeviceID, EventID
        d.submit(&[[0x0C | (0x100u64 << 32), 7, 0, 0]]);

        assert_eq!(
            *d.programmer.invalidations.lock(),
            [InvalidateTarget::Interrupt(8192)]
        );
    }

    #[pal_async::async_test]
    async fn reset_returns_to_power_on_state() {
        let mut d = new_device();
        d.enable();
        d.dev.core.subscribe_interrupt(0x100, 7);
        d.submit(&[mapd(0x100), mapc(1, 3), mapti(0x100, 7, 8192, 1)]);
        assert!(d.dev.core.translate_interrupt(0x100, 7).is_some());

        d.dev.reset().await;

        assert_eq!(d.read32(GITS_CTLR_OFFSET), 1 << 31);
        assert_eq!(d.read64(GITS_CBASER_OFFSET), 0);
        assert_eq!(d.read64(GITS_CREADR_OFFSET), 0);
        assert_eq!(d.read64(GITS_CWRITER_OFFSET), 0);
        // The guest's old mappings must be gone, not merely unreachable.
        assert_eq!(d.dev.core.translate_interrupt(0x100, 7), None);
    }

    #[test]
    fn static_region_covers_the_whole_window() {
        let mut d = new_device();
        let regions = d.dev.get_static_regions();
        assert_eq!(regions.len(), 1);
        assert_eq!(
            *regions[0].1.start()..=*regions[0].1.end(),
            MMIO_BASE..=MMIO_BASE + MMIO_REGION_SIZE - 1
        );
    }
}
