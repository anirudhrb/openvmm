// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Core register file and command-queue state machine for the virtual ITS.

use crate::cache::TranslationCache;
use crate::commands::process_command;
use crate::env::CommandQueue;
use crate::env::ItsEnvironment;
use crate::env::Translation;
use crate::error::ItsError;
use crate::registers::*;
use crate::tables::CollectionTable;
use crate::tables::DeviceTable;
use crate::tables::InterruptTranslationTable;
use crate::types::DeviceId;
use crate::types::EventId;

macro_rules! define_registers {
    ($(($name:ident, $offset:ident, $type:ty, $write_handler:ident)),* $(,)?) => {
        pub(crate) struct ItsRegisters {
            $(pub $name: $type,)*
        }

        impl ItsRegisters {
            pub fn new() -> Self {
                Self {
                    $($name: 0 as $type,)*
                }
            }
        }

        impl<E: ItsEnvironment> ItsCore<E> {
            fn read(&self, offset: u32, len: u32) -> Result<u64, ItsError> {
                if len == 0 {
                    return Ok(0);
                }

                if (offset % 4 != 0) || !matches!(len, 4 | 8) {
                    return Err(ItsError::InvalidMemoryAccess);
                }

                let value = match offset {
                    $($offset => self.regs.$name as u64,)*
                    _ => return Err(ItsError::InvalidRegisterAccess),
                };

                if len == 4 {
                    Ok(value & 0xFFFF_FFFF)
                } else {
                    Ok(value)
                }
            }

            fn write(&mut self, offset: u32, len: u32, value: u64) -> Result<(), ItsError> {
                if len == 0 {
                    return Ok(());
                }

                if (offset % 4 != 0) || !matches!(len, 4 | 8) {
                    return Err(ItsError::InvalidMemoryAccess);
                }

                match offset {
                    $($offset => self.$write_handler(value, len)?,)*
                    _ => return Err(ItsError::InvalidRegisterAccess),
                }

                Ok(())
            }
        }
    };
}

define_registers! {
    (gits_ctlr, GITS_CTLR_OFFSET, u32, set_gits_ctlr),
    (gits_iidr, GITS_IIDR_OFFSET, u32, set_gits_readonly),
    (gits_typer, GITS_TYPER_OFFSET, u64, set_gits_readonly),
    (gits_cbaser, GITS_CBASER_OFFSET, u64, set_gits_cbaser),
    (gits_cwriter, GITS_CWRITER_OFFSET, u64, set_gits_cwriter),
    (gits_creadr, GITS_CREADR_OFFSET, u64, set_gits_creadr),
    (gits_device_table_baser, GITS_BASER0_OFFSET, u64, set_gits_device_table_baser),
    (gits_collection_table_baser, GITS_BASER1_OFFSET, u64, set_gits_collection_table_baser),
    (gits_pidr2, GITS_PIDR2_OFFSET, u32, set_gits_readonly),
}

/// Virtual GICv3 ITS control-plane state machine.
pub struct ItsCore<E: ItsEnvironment> {
    env: E,
    enabled: bool,
    regs: ItsRegisters,
    device_table: DeviceTable,
    collection_table: CollectionTable,
    interrupt_translation_table: InterruptTranslationTable,
    translation_cache: TranslationCache,
    command_queue: Option<CommandQueue>,
}

impl<E: ItsEnvironment> ItsCore<E> {
    /// Creates a new [`ItsCore`] initialized to its architectural reset state.
    pub fn new(env: E) -> Self {
        let max_device_id = (1 << (GITS_TYPER_DEVBITS + 1)) - 1;

        Self {
            env,
            enabled: false,
            regs: ItsRegisters::new(),
            device_table: DeviceTable::new(max_device_id),
            collection_table: CollectionTable::new(100),
            interrupt_translation_table: InterruptTranslationTable::new(),
            translation_cache: TranslationCache::new(),
            command_queue: None,
        }
        .initialize()
    }

    /// Returns a reference to the host environment.
    pub fn env(&self) -> &E {
        &self.env
    }

    /// Returns a mutable reference to the host environment.
    pub fn env_mut(&mut self) -> &mut E {
        &mut self.env
    }

    /// Reads an ITS control register at `offset` with access width `len`.
    pub fn read_mmio(&self, offset: u32, len: u32) -> Result<u64, ItsError> {
        self.read(offset, len)
    }

    /// Writes `value` to an ITS control register at `offset` with access width `len`.
    pub fn write_mmio(&mut self, offset: u32, len: u32, value: u64) -> Result<(), ItsError> {
        self.write(offset, len, value)
    }

    fn initialize(mut self) -> Self {
        // Read-only identification registers
        self.regs.gits_iidr = GITS_IIDR_MSFT;
        self.regs.gits_pidr2 = GITS_PIDR2_GICV3;
        self.regs.gits_typer = GITS_TYPER_PLPIS
            | (GITS_TYPER_DEVBITS << GITS_TYPER_DEVBITS_SHIFT)
            | (GITS_TYPER_IDBITS << GITS_TYPER_IDBITS_SHIFT)
            | (GITS_TYPER_ITTE_SIZE << GITS_TYPER_ITTE_SIZE_SHIFT);

        // Command queue reset values
        self.regs.gits_cbaser = 0;
        self.regs.gits_cwriter = 0;
        self.regs.gits_creadr = 0;

        // Table base register reset values
        let gits_baser = (GITS_BASER_TYPE_NONE << GITS_BASER_TYPE_SHIFT)
            | (GITS_BASER_CACHE_SAME_AS_INNER << GITS_BASER_OUTER_CACHEABILITY_SHIFT)
            | (GITS_BASER_CACHE_RA_WB << GITS_BASER_INNER_CACHEABILITY_SHIFT)
            | (GITS_BASER_INNER_SHAREABLE << GITS_BASER_SHAREABILITY_SHIFT);

        self.regs.gits_device_table_baser =
            (GITS_BASER_TYPE_DEVICE << GITS_BASER_TYPE_SHIFT) | gits_baser;
        self.regs.gits_collection_table_baser =
            (GITS_BASER_TYPE_COLLECTION << GITS_BASER_TYPE_SHIFT) | gits_baser;

        // Control register reset value
        self.regs.gits_ctlr = GITS_CTLR_QUIESCENT;
        self.enabled = false;

        tracing::debug!(gits_typer = self.regs.gits_typer, "initialized ITS");

        self
    }

    fn set_gits_ctlr(&mut self, value: u64, _len: u32) -> Result<(), ItsError> {
        let its_enabled = (value as u32) & GITS_CTLR_ENABLE != 0;
        self.regs.gits_ctlr = GITS_CTLR_QUIESCENT | its_enabled as u32;
        self.enabled = its_enabled;
        tracing::debug!(gits_ctlr = self.regs.gits_ctlr, "updated GITS_CTLR");
        Ok(())
    }

    fn set_gits_readonly(&mut self, _value: u64, _len: u32) -> Result<(), ItsError> {
        Err(ItsError::InvalidRegisterAccess)
    }

    fn set_gits_cbaser(&mut self, value: u64, len: u32) -> Result<(), ItsError> {
        if self.enabled {
            return Ok(());
        }

        let ro_mask = if len == 4 { 0xFFFF_FFFF_0000_0000 } else { 0 };

        let cmdq_enabled = self.regs.gits_cbaser & GITS_CBASER_VALID != 0;
        let value1 = self.regs.gits_cbaser & ro_mask;
        let value2 = value & !ro_mask;
        self.regs.gits_cbaser = value1 | value2;

        if !cmdq_enabled && self.regs.gits_cbaser & GITS_CBASER_VALID != 0 {
            let address = self.regs.gits_cbaser & GITS_CBASER_ADDRESS_MASK;
            let size = self.regs.gits_cbaser & GITS_CBASER_SIZE_MASK;
            let size = (size + 1) * PAGE_SIZE;

            if self.command_queue.is_none() {
                let queue = CommandQueue {
                    base_gpa: address,
                    size_bytes: size,
                };
                self.command_queue = Some(queue);
                self.env.command_queue_changed(Some(queue));

                tracing::debug!(
                    base_gpa = address,
                    size_bytes = size,
                    "command queue mapped"
                );
            }
        } else if cmdq_enabled && self.regs.gits_cbaser & GITS_CBASER_VALID == 0 {
            self.command_queue = None;
            self.env.command_queue_changed(None);

            tracing::debug!("command queue unmapped");
        }

        Ok(())
    }

    fn set_gits_cwriter(&mut self, value: u64, len: u32) -> Result<(), ItsError> {
        let ro_mask = if len == 4 { 0xFFFF_FFFF_0000_0000 } else { 0 };

        let value1 = self.regs.gits_cwriter & ro_mask;
        let value2 = value & !ro_mask;
        self.regs.gits_cwriter = value1 | value2;

        tracing::debug!(
            cwriter = self.regs.gits_cwriter,
            creadr = self.regs.gits_creadr,
            "updated GITS_CWRITER"
        );

        if self.enabled {
            self.process_command_queue()?;
        }

        Ok(())
    }

    fn process_command_queue(&mut self) -> Result<(), ItsError> {
        let Some(queue) = self.command_queue else {
            return Ok(());
        };

        if queue.size_bytes == 0 {
            return Ok(());
        }

        let start_offset = self.regs.gits_creadr >> GITS_CREADR_SHIFT;
        let end_offset = self.regs.gits_cwriter >> GITS_CWRITER_SHIFT;

        if start_offset == end_offset {
            return Ok(());
        }

        let max_entries = queue.size_bytes / GITS_COMMAND_SIZE;

        let mut commands_processed = 0u64;
        let mut current_offset = start_offset;
        while current_offset != end_offset {
            let command = self.read_command_at_offset(&queue, current_offset, max_entries)?;

            process_command(
                &command,
                &mut self.device_table,
                &mut self.collection_table,
                &mut self.interrupt_translation_table,
                &mut self.env,
            )?;

            current_offset = (current_offset + 1) % max_entries;
            commands_processed += 1;
        }

        self.update_translation_cache();
        self.flush_translation_cache();

        self.regs.gits_creadr = current_offset << GITS_CREADR_SHIFT;

        tracing::debug!(
            commands_processed,
            start_offset,
            end_offset,
            "processed ITS command queue"
        );

        Ok(())
    }

    fn read_command_at_offset(
        &self,
        queue: &CommandQueue,
        offset: u64,
        max_entries: u64,
    ) -> Result<[u64; 4], ItsError> {
        if offset >= max_entries {
            return Err(ItsError::InvalidMemoryAccess);
        }

        let gpa = queue.base_gpa + offset * GITS_COMMAND_SIZE;

        self.env.read_command(gpa).map_err(|err| {
            tracelimit::warn_ratelimited!(gpa, ?err, "failed to read ITS command");
            ItsError::InvalidMemoryAccess
        })
    }

    fn set_gits_creadr(&mut self, value: u64, len: u32) -> Result<(), ItsError> {
        if self.enabled {
            return Ok(());
        }

        let ro_mask = if len == 4 { 0xFFFF_FFFF_0000_0000 } else { 0 };

        let value1 = self.regs.gits_creadr & ro_mask;
        let value2 = value & !ro_mask;
        self.regs.gits_creadr = value1 | value2;

        Ok(())
    }

    fn set_gits_device_table_baser(&mut self, value: u64, len: u32) -> Result<(), ItsError> {
        if self.enabled {
            return Ok(());
        }

        let ro_mask = if len == 4 {
            0xFFFF_FFFF_0000_0000 | GITS_BASER_RO_MASK
        } else {
            GITS_BASER_RO_MASK
        };

        let value1 = self.regs.gits_device_table_baser & ro_mask;
        let value2 = value & !ro_mask;
        self.regs.gits_device_table_baser = value1 | value2;
        Ok(())
    }

    fn set_gits_collection_table_baser(&mut self, value: u64, len: u32) -> Result<(), ItsError> {
        if self.enabled {
            return Ok(());
        }

        let ro_mask = if len == 4 {
            0xFFFF_FFFF_0000_0000 | GITS_BASER_RO_MASK
        } else {
            GITS_BASER_RO_MASK
        };

        let value1 = self.regs.gits_collection_table_baser & ro_mask;
        let value2 = value & !ro_mask;
        self.regs.gits_collection_table_baser = value1 | value2;
        Ok(())
    }

    /// Subscribes to translation updates for an interrupt.
    ///
    /// The host is told the interrupt's current translation immediately, and
    /// again whenever a command changes it.
    pub fn subscribe_interrupt(&mut self, device_id: DeviceId, event_id: EventId) {
        self.translation_cache.insert_interrupt(device_id, event_id);
        self.update_translation_cache();
        self.flush_translation_cache();
    }

    /// Unsubscribes from translation updates for an interrupt.
    pub fn unsubscribe_interrupt(&mut self, device_id: DeviceId, event_id: EventId) {
        self.translation_cache.remove_interrupt(device_id, event_id);
    }

    /// Re-resolves every subscribed interrupt, marking those whose translation
    /// has changed.
    ///
    /// This is what handles fan-out: a single `MAPC` or `MOVALL` can retarget
    /// many interrupts at once, and there is no reverse index from a collection
    /// back to its translation table entries.
    fn update_translation_cache(&mut self) {
        for (device_id, event_id) in self.translation_cache.subscribed_interrupts() {
            let translation = self.translate_interrupt(device_id, event_id);
            self.translation_cache
                .update_translation(device_id, event_id, translation);
        }
    }

    /// Resolves `(device_id, event_id)` through the shadow ITT and collection
    /// tables.
    pub fn translate_interrupt(
        &self,
        device_id: DeviceId,
        event_id: EventId,
    ) -> Option<Translation> {
        let entry = self
            .interrupt_translation_table
            .get_entry(device_id, event_id)?;

        let interrupt_id = entry.interrupt_id;
        let collection_id = entry.collection_id;

        let entry = self.collection_table.get_collection(collection_id)?;

        Some(Translation {
            processor_id: entry.processor_id,
            interrupt_id,
        })
    }

    /// Reports every changed translation to the host.
    pub fn flush_translation_cache(&mut self) {
        self.translation_cache.flush(&mut self.env);
    }
}
