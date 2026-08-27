// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Smoke tests driving [`ItsCore`] through the [`ItsEnvironment`] trait.

use std::collections::HashMap;
use test_with_tracing::test;
use vits_core::CommandQueue;
use vits_core::DeviceId;
use vits_core::EventId;
use vits_core::InvalidateTarget;
use vits_core::ItsCore;
use vits_core::ItsEnvironment;
use vits_core::Translation;

/// A host that keeps the ITS command queue in a plain map.
#[derive(Default)]
struct TestEnv {
    memory: HashMap<u64, [u64; 4]>,
    queue: Option<CommandQueue>,
    /// True for each map notification, false for each unmap.
    queue_changes: Vec<bool>,
    invalidations: Vec<InvalidateTarget>,
    translations: Vec<(DeviceId, EventId, Option<Translation>)>,
}

impl ItsEnvironment for TestEnv {
    type Error = ();

    fn read_command(&self, gpa: u64) -> Result<[u64; 4], Self::Error> {
        self.memory.get(&gpa).copied().ok_or(())
    }

    fn command_queue_changed(&mut self, queue: Option<CommandQueue>) {
        self.queue_changes.push(queue.is_some());
        self.queue = queue;
    }

    fn invalidate(&mut self, target: InvalidateTarget) {
        self.invalidations.push(target);
    }

    fn translation_changed(
        &mut self,
        device_id: DeviceId,
        event_id: EventId,
        translation: Option<Translation>,
    ) {
        self.translations.push((device_id, event_id, translation));
    }
}

const QUEUE_BASE: u64 = 0x10_0000;

// Register offsets, from the GICv3 ITS specification.
const GITS_CTLR: u32 = 0x0000;
const GITS_TYPER: u32 = 0x0008;
const GITS_CBASER: u32 = 0x0080;
const GITS_CWRITER: u32 = 0x0088;
const GITS_CREADR: u32 = 0x0090;

// GITS_CBASER.Valid is bit 63; Size is bits [7:0] and holds pages minus one.
const CBASER_VALID: u64 = 1 << 63;

/// Builds a GITS_CBASER value for a queue of `pages` 4 KiB pages.
fn cbaser(pages: u64) -> u64 {
    QUEUE_BASE | CBASER_VALID | (pages - 1)
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

fn invall(collection_id: u32) -> [u64; 4] {
    [0x0D, 0, collection_id as u64, 0]
}

/// Enables the ITS and points it at a single-page command queue holding
/// `commands`.
fn start(commands: &[[u64; 4]]) -> ItsCore<TestEnv> {
    let mut env = TestEnv::default();
    for (i, command) in commands.iter().enumerate() {
        env.memory.insert(QUEUE_BASE + (i as u64) * 32, *command);
    }

    let mut its = ItsCore::new(env);

    its.write_mmio(GITS_CBASER, 8, cbaser(1)).unwrap();
    its.write_mmio(GITS_CTLR, 4, 1).unwrap();
    its
}

#[test]
fn reports_a_gicv3_its() {
    let its = start(&[]);
    let typer = its.read_mmio(GITS_TYPER, 8).unwrap();

    // PLPIS: physical LPIs are supported.
    assert_eq!(typer & 1, 1);
    assert_eq!(its.read_mmio(GITS_CTLR, 4).unwrap() & 1, 1);
}

#[test]
fn enabling_the_command_queue_notifies_the_host() {
    let its = start(&[]);
    let queue = its.env().queue.expect("queue should be mapped");

    assert_eq!(queue.base_gpa, QUEUE_BASE);
    assert_eq!(queue.size_bytes, 0x1000);
}

/// A single-page queue has Size = 0. Reading Valid from bit 0 rather than bit
/// 63 made this case, and every other odd page count, fail to map at all.
#[test]
fn a_single_page_queue_maps() {
    let mut its = ItsCore::new(TestEnv::default());
    its.write_mmio(GITS_CBASER, 8, cbaser(1)).unwrap();

    let queue = its.env().queue.expect("a one page queue should map");
    assert_eq!(queue.size_bytes, 0x1000);
}

#[test]
fn queue_size_follows_the_size_field() {
    for pages in [1u64, 2, 3, 4, 16] {
        let mut its = ItsCore::new(TestEnv::default());
        its.write_mmio(GITS_CBASER, 8, cbaser(pages)).unwrap();

        let queue = its
            .env()
            .queue
            .unwrap_or_else(|| panic!("a {pages} page queue should map"));
        assert_eq!(queue.size_bytes, pages * 0x1000);
    }
}

#[test]
fn a_queue_with_valid_clear_never_maps() {
    let mut its = ItsCore::new(TestEnv::default());

    // Size of 16 pages, but Valid is clear.
    its.write_mmio(GITS_CBASER, 8, QUEUE_BASE | 15).unwrap();
    assert!(its.env().queue.is_none());
}

#[test]
fn clearing_valid_unmaps_the_queue() {
    let mut its = ItsCore::new(TestEnv::default());

    // Sixteen pages, so that Size has its low bit set. Clearing Valid must
    // still unmap, even though bit 0 of Size remains set.
    its.write_mmio(GITS_CBASER, 8, cbaser(16)).unwrap();
    assert!(its.env().queue.is_some());

    its.write_mmio(GITS_CBASER, 8, QUEUE_BASE | 15).unwrap();
    assert!(its.env().queue.is_none());
    assert_eq!(its.env().queue_changes, vec![true, false]);
}

#[test]
fn resizing_a_valid_queue_does_not_unmap_it() {
    let mut its = ItsCore::new(TestEnv::default());

    // Two pages, then one. Size goes from 1 to 0, so its low bit clears while
    // Valid stays set; the queue must remain mapped.
    its.write_mmio(GITS_CBASER, 8, cbaser(2)).unwrap();
    its.write_mmio(GITS_CBASER, 8, cbaser(1)).unwrap();

    assert!(its.env().queue.is_some());
    assert_eq!(its.env().queue_changes, vec![true]);
}

#[test]
fn translates_a_mapped_interrupt() {
    let mut its = start(&[mapd(7), mapc(3, 2), mapti(7, 5, 8192, 3)]);

    // Three commands queued.
    its.write_mmio(GITS_CWRITER, 8, 3 << 5).unwrap();

    assert_eq!(
        its.translate_interrupt(7, 5),
        Some(Translation {
            processor_id: 2,
            interrupt_id: 8192,
        })
    );

    // The emulator consumed every command.
    assert_eq!(its.read_mmio(GITS_CREADR, 8).unwrap(), 3 << 5);
}

#[test]
fn unmapped_interrupts_do_not_translate() {
    let its = start(&[]);
    assert_eq!(its.translate_interrupt(1, 1), None);
}

#[test]
fn invall_carries_the_collection_id() {
    let mut its = start(&[invall(9)]);
    its.write_mmio(GITS_CWRITER, 8, 1 << 5).unwrap();

    assert_eq!(
        its.env().invalidations,
        vec![InvalidateTarget::Collection(9)]
    );
}

#[test]
fn subscribers_are_told_when_a_collection_moves() {
    let mut its = start(&[
        mapd(7),
        mapc(3, 2),
        mapti(7, 5, 8192, 3),
        // Retarget collection 3 from processor 2 to processor 6.
        mapc(3, 6),
    ]);

    // Process the first three commands, then subscribe.
    its.write_mmio(GITS_CWRITER, 8, 3 << 5).unwrap();
    its.subscribe_interrupt(7, 5);

    assert_eq!(
        its.env().translations,
        vec![(
            7,
            5,
            Some(Translation {
                processor_id: 2,
                interrupt_id: 8192,
            })
        )]
    );

    // Now move the collection. The subscriber must be told, even though no
    // command mentions (DeviceID 7, EventID 5) directly.
    its.write_mmio(GITS_CWRITER, 8, 4 << 5).unwrap();

    assert_eq!(
        its.env().translations.last(),
        Some(&(
            7,
            5,
            Some(Translation {
                processor_id: 6,
                interrupt_id: 8192,
            })
        ))
    );
}

#[test]
fn unchanged_translations_are_not_reported_twice() {
    let mut its = start(&[mapd(7), mapc(3, 2), mapti(7, 5, 8192, 3)]);
    its.write_mmio(GITS_CWRITER, 8, 3 << 5).unwrap();

    its.subscribe_interrupt(7, 5);
    let reported = its.env().translations.len();

    // Flushing again with nothing changed must not repeat the notification,
    // since every redundant push costs the host a routing update.
    its.flush_translation_cache();
    assert_eq!(its.env().translations.len(), reported);
}
