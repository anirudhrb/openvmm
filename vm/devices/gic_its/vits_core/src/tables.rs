// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Shadow tables maintained by the virtual ITS:
//! - [`DeviceTable`]: tracks mapped device IDs
//! - [`CollectionTable`]: maps collection IDs to target processors
//! - [`InterruptTranslationTable`]: maps `(DeviceID, EventID)` to `(InterruptID, CollectionID)`

use crate::types::CollectionId;
use crate::types::DeviceId;
use crate::types::EventId;
use crate::types::InterruptId;
use crate::types::ProcessorId;
use std::collections::HashMap;

/// Device Table Entry.
#[derive(Debug, Clone)]
pub(crate) struct DeviceTableEntry {
    pub valid: bool,
}

/// Collection Table Entry.
#[derive(Debug, Clone)]
pub(crate) struct CollectionTableEntry {
    pub valid: bool,
    pub processor_id: ProcessorId,
}

/// Interrupt Translation Table Entry.
#[derive(Debug, Clone)]
pub(crate) struct InterruptTranslationTableEntry {
    pub valid: bool,
    pub collection_id: CollectionId,
    pub interrupt_id: InterruptId,
}

/// Shadow Device Table.
pub(crate) struct DeviceTable {
    pub max_device_id: u32,
    devices: HashMap<DeviceId, DeviceTableEntry>,
}

impl DeviceTable {
    pub fn new(max_device_id: u32) -> Self {
        Self {
            max_device_id,
            devices: HashMap::new(),
        }
    }

    pub fn map_device(&mut self, device_id: DeviceId) -> bool {
        if device_id > self.max_device_id {
            return false;
        }

        self.devices
            .insert(device_id, DeviceTableEntry { valid: true });
        true
    }

    pub fn unmap_device(&mut self, device_id: DeviceId) -> bool {
        if let Some(entry) = self.devices.get_mut(&device_id) {
            entry.valid = false;
            true
        } else {
            false
        }
    }
}

/// Shadow Collection Table.
pub(crate) struct CollectionTable {
    pub max_collection_id: u32,
    collections: HashMap<CollectionId, CollectionTableEntry>,
}

impl CollectionTable {
    pub fn new(max_collection_id: u32) -> Self {
        Self {
            max_collection_id,
            collections: HashMap::new(),
        }
    }

    pub fn map_collection(
        &mut self,
        collection_id: CollectionId,
        processor_id: ProcessorId,
    ) -> bool {
        if collection_id > self.max_collection_id {
            return false;
        }

        let entry = CollectionTableEntry {
            processor_id,
            valid: true,
        };

        self.collections.insert(collection_id, entry);
        true
    }

    pub fn unmap_collection(&mut self, collection_id: CollectionId) -> bool {
        if let Some(entry) = self.collections.get_mut(&collection_id) {
            entry.valid = false;
            true
        } else {
            false
        }
    }

    pub fn get_collection(&self, collection_id: CollectionId) -> Option<&CollectionTableEntry> {
        self.collections
            .get(&collection_id)
            .filter(|entry| entry.valid)
    }
}

/// Shadow Interrupt Translation Table (ITT).
pub(crate) struct InterruptTranslationTable {
    entries: HashMap<(DeviceId, EventId), InterruptTranslationTableEntry>,
}

impl InterruptTranslationTable {
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
        }
    }

    pub fn map_interrupt(
        &mut self,
        device_id: DeviceId,
        event_id: EventId,
        interrupt_id: InterruptId,
        collection_id: CollectionId,
    ) -> bool {
        let entry = InterruptTranslationTableEntry {
            interrupt_id,
            collection_id,
            valid: true,
        };

        self.entries.insert((device_id, event_id), entry);
        true
    }

    pub fn translate_event(&self, device_id: DeviceId, event_id: EventId) -> Option<InterruptId> {
        self.entries
            .get(&(device_id, event_id))
            .filter(|entry| entry.valid)
            .map(|entry| entry.interrupt_id)
    }

    pub fn get_entry(
        &self,
        device_id: DeviceId,
        event_id: EventId,
    ) -> Option<&InterruptTranslationTableEntry> {
        self.entries
            .get(&(device_id, event_id))
            .filter(|entry| entry.valid)
    }
}
