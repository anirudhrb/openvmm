// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! ITS command decoding and execution.

use crate::env::InvalidateTarget;
use crate::env::ItsEnvironment;
use crate::error::ItsError;
use crate::tables::CollectionTable;
use crate::tables::DeviceTable;
use crate::tables::InterruptTranslationTable;

#[derive(Debug, Clone, PartialEq, Eq)]
enum ItsCommand {
    /// MOVI - Move interrupt
    MoveInterrupt {
        device_id: u32,
        event_id: u32,
        collection_id: u32,
    },
    /// INT - Assert interrupt
    Interrupt { device_id: u32, event_id: u32 },
    /// CLEAR - Clear pending interrupt
    ClearInterrupt { device_id: u32, event_id: u32 },
    /// SYNC - Synchronize state with a specific processor
    Sync { processor_id: u32 },
    /// MAPD - Map device to device table
    MapDevice {
        device_id: u32,
        itt_size: u32,
        itt_base: u64,
        valid: bool,
    },
    /// MAPC - Map collection
    MapCollection {
        collection_id: u32,
        processor_id: u32,
        valid: bool,
    },
    /// MAPTI - Map interrupt with specific interrupt_id
    MapInterruptTranslation {
        device_id: u32,
        event_id: u32,
        interrupt_id: u32,
        collection_id: u32,
    },
    /// MAPI - Map interrupt with interrupt_id = event_id
    MapInterrupt {
        device_id: u32,
        event_id: u32,
        collection_id: u32,
    },
    /// INV - Invalidate by device and event
    Invalidate { device_id: u32, event_id: u32 },
    /// INVALL - Invalidate by collection id
    InvalidateAll { collection_id: u32 },
    /// MOVALL - Move all interrupts
    MoveAllInterrupts {
        src_processor_id: u32,
        dst_processor_id: u32,
    },
    /// DISCARD - Discard pending interrupt
    DiscardInterrupt { device_id: u32, event_id: u32 },
    /// Unknown or unsupported command
    Unknown { cmd_type: u8 },
}

impl ItsCommand {
    fn decode(qword0: u64, qword1: u64, qword2: u64, qword3: u64) -> Self {
        let cmd_type = (qword0 & 0xFF) as u8;

        match cmd_type {
            // MOVI DeviceID, EventID, ICID
            0x01 => ItsCommand::MoveInterrupt {
                device_id: ((qword0 >> 32) & 0xFFFF_FFFF) as u32,
                event_id: (qword1 & 0xFFFF_FFFF) as u32,
                collection_id: (qword2 & 0xFFFF) as u32,
            },

            // INT DeviceID, EventID
            0x03 => ItsCommand::Interrupt {
                device_id: ((qword0 >> 32) & 0xFFFF_FFFF) as u32,
                event_id: (qword1 & 0xFFFF_FFFF) as u32,
            },

            // CLEAR DeviceID, EventID
            0x04 => ItsCommand::ClearInterrupt {
                device_id: ((qword0 >> 32) & 0xFFFF_FFFF) as u32,
                event_id: (qword1 & 0xFFFF_FFFF) as u32,
            },

            // SYNC RDbase
            0x05 => ItsCommand::Sync {
                processor_id: ((qword2 >> 16) & 0x7_FFFF_FFFF) as u32,
            },

            // MAPD DeviceID, itt_base, Size, V
            0x08 => ItsCommand::MapDevice {
                device_id: ((qword0 >> 32) & 0xFFFF_FFFF) as u32,
                itt_size: ((qword1 >> 1) & 0b1_1111) as u32,
                itt_base: (qword2 >> 8) & 0xFFF_FFFF_FFFF,
                valid: (qword2 >> 63) & 0x1 != 0,
            },

            // MAPC ICID, RDbase, V
            0x09 => ItsCommand::MapCollection {
                collection_id: (qword2 & 0xFFFF) as u32,
                processor_id: ((qword2 >> 16) & 0xF_FFFF_FFFF) as u32,
                valid: (qword2 >> 63) & 0x1 != 0,
            },

            // MAPTI DeviceID, EventID, pINTID, ICID
            0x0A => ItsCommand::MapInterruptTranslation {
                device_id: ((qword0 >> 32) & 0xFFFF_FFFF) as u32,
                event_id: (qword1 & 0xFFFF_FFFF) as u32,
                interrupt_id: ((qword1 >> 32) & 0xFFFF_FFFF) as u32,
                collection_id: (qword2 & 0xFFFF) as u32,
            },

            // MAPI DeviceID, EventID, ICID
            0x0B => ItsCommand::MapInterrupt {
                device_id: ((qword0 >> 32) & 0xFFFF_FFFF) as u32,
                event_id: (qword1 & 0xFFFF_FFFF) as u32,
                collection_id: (qword2 & 0xFFFF) as u32,
            },

            // INV DeviceID, EventID
            0x0C => ItsCommand::Invalidate {
                device_id: ((qword0 >> 32) & 0xFFFF_FFFF) as u32,
                event_id: (qword1 & 0xFFFF_FFFF) as u32,
            },

            // INVALL ICID
            0x0D => ItsCommand::InvalidateAll {
                collection_id: (qword2 & 0xFFFF) as u32,
            },

            // MOVALL RDbase1, RDbase2
            0x0E => ItsCommand::MoveAllInterrupts {
                src_processor_id: ((qword2 >> 16) & 0xF_FFFF_FFFF) as u32,
                dst_processor_id: ((qword3 >> 16) & 0xF_FFFF_FFFF) as u32,
            },

            // DISCARD DeviceID, EventID
            0x0F => ItsCommand::DiscardInterrupt {
                device_id: ((qword0 >> 32) & 0xFFFF_FFFF) as u32,
                event_id: (qword1 & 0xFFFF_FFFF) as u32,
            },

            _ => ItsCommand::Unknown { cmd_type },
        }
    }
}

pub(crate) fn process_command<E: ItsEnvironment>(
    raw: &[u64; 4],
    device_table: &mut DeviceTable,
    collection_table: &mut CollectionTable,
    itt_table: &mut InterruptTranslationTable,
    env: &mut E,
) -> Result<(), ItsError> {
    let command = ItsCommand::decode(raw[0], raw[1], raw[2], raw[3]);

    tracing::debug!(?command, "processing ITS command");

    match command {
        ItsCommand::MoveInterrupt {
            device_id,
            event_id,
            collection_id,
        } => process_move_interrupt(
            device_id,
            event_id,
            collection_id,
            device_table,
            collection_table,
            itt_table,
        ),
        ItsCommand::Sync { processor_id: _ } => {
            // Commands are processed synchronously.
            Ok(())
        }
        ItsCommand::MapDevice {
            device_id,
            itt_size,
            itt_base,
            valid,
        } => process_mapd(device_id, itt_size, itt_base, valid, device_table),
        ItsCommand::MapCollection {
            collection_id,
            processor_id,
            valid,
        } => process_mapc(collection_id, processor_id, valid, collection_table),
        ItsCommand::MapInterruptTranslation {
            device_id,
            event_id,
            interrupt_id,
            collection_id,
        } => process_mapti(
            device_id,
            event_id,
            interrupt_id,
            collection_id,
            device_table,
            collection_table,
            itt_table,
        ),
        ItsCommand::MapInterrupt {
            device_id,
            event_id,
            collection_id,
        } => process_mapti(
            device_id,
            event_id,
            event_id,
            collection_id,
            device_table,
            collection_table,
            itt_table,
        ),
        ItsCommand::Invalidate {
            device_id,
            event_id,
        } => process_inv(device_id, event_id, itt_table, env),
        ItsCommand::InvalidateAll { collection_id } => process_invall(collection_id, env),
        ItsCommand::DiscardInterrupt {
            device_id: _,
            event_id: _,
        } => Ok(()),
        ItsCommand::Unknown { cmd_type: _ } => Err(ItsError::InvalidCommand),
        ItsCommand::Interrupt { .. }
        | ItsCommand::ClearInterrupt { .. }
        | ItsCommand::MoveAllInterrupts { .. } => Err(ItsError::CommandNotSupported),
    }
}

fn process_mapd(
    device_id: u32,
    itt_size: u32,
    itt_base: u64,
    valid: bool,
    device_table: &mut DeviceTable,
) -> Result<(), ItsError> {
    tracing::debug!(device_id, itt_size, itt_base, valid, "MAPD");

    if device_id > device_table.max_device_id {
        return Err(ItsError::CommandDeviceIdOutOfRange);
    }

    let status = if valid {
        device_table.map_device(device_id)
    } else {
        device_table.unmap_device(device_id)
    };

    if !status {
        return Err(ItsError::CommandError);
    }

    Ok(())
}

fn process_mapc(
    collection_id: u32,
    processor_id: u32,
    valid: bool,
    collection_table: &mut CollectionTable,
) -> Result<(), ItsError> {
    tracing::debug!(collection_id, processor_id, valid, "MAPC");

    if collection_id > collection_table.max_collection_id {
        return Err(ItsError::CommandCollectionIdOutOfRange);
    }

    let status = if valid {
        collection_table.map_collection(collection_id, processor_id)
    } else {
        collection_table.unmap_collection(collection_id)
    };

    if !status {
        return Err(ItsError::CommandError);
    }

    Ok(())
}

fn process_mapti(
    device_id: u32,
    event_id: u32,
    interrupt_id: u32,
    collection_id: u32,
    device_table: &mut DeviceTable,
    collection_table: &mut CollectionTable,
    itt_table: &mut InterruptTranslationTable,
) -> Result<(), ItsError> {
    tracing::debug!(device_id, event_id, interrupt_id, collection_id, "MAPTI");

    if device_id > device_table.max_device_id {
        return Err(ItsError::CommandDeviceIdOutOfRange);
    }

    if collection_id > collection_table.max_collection_id {
        return Err(ItsError::CommandCollectionIdOutOfRange);
    }

    if !itt_table.map_interrupt(device_id, event_id, interrupt_id, collection_id) {
        return Err(ItsError::CommandError);
    }

    Ok(())
}

fn process_inv<E: ItsEnvironment>(
    device_id: u32,
    event_id: u32,
    itt_table: &InterruptTranslationTable,
    env: &mut E,
) -> Result<(), ItsError> {
    tracing::debug!(device_id, event_id, "INV");

    let Some(interrupt_id) = itt_table.translate_event(device_id, event_id) else {
        return Err(ItsError::CommandInterruptNotMapped);
    };

    env.invalidate(InvalidateTarget::Interrupt(interrupt_id));
    Ok(())
}

fn process_invall<E: ItsEnvironment>(collection_id: u32, env: &mut E) -> Result<(), ItsError> {
    tracing::debug!(collection_id, "INVALL");

    env.invalidate(InvalidateTarget::Collection(collection_id));
    Ok(())
}

fn process_move_interrupt(
    device_id: u32,
    event_id: u32,
    collection_id: u32,
    device_table: &mut DeviceTable,
    collection_table: &mut CollectionTable,
    itt_table: &mut InterruptTranslationTable,
) -> Result<(), ItsError> {
    tracing::debug!(device_id, event_id, collection_id, "MOVI");

    if device_id > device_table.max_device_id {
        return Err(ItsError::CommandDeviceIdOutOfRange);
    }

    if collection_id > collection_table.max_collection_id {
        return Err(ItsError::CommandCollectionIdOutOfRange);
    }

    let Some(interrupt_id) = itt_table.translate_event(device_id, event_id) else {
        return Err(ItsError::CommandInterruptNotMapped);
    };

    if !itt_table.map_interrupt(device_id, event_id, interrupt_id, collection_id) {
        return Err(ItsError::CommandError);
    }

    Ok(())
}
