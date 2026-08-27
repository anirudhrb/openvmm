// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Interrupt translation cache for the virtual ITS.
//!
//! The cache exists to solve fan-out: a single `MAPC` or `MOVALL` command can
//! retarget every interrupt belonging to a collection, and the emulator holds
//! no reverse index from a collection back to its translation table entries.
//! Rather than build one, the cache tracks the interrupts the host has
//! subscribed to, re-resolves them after each command batch, and reports only
//! those whose translation actually changed.

use crate::env::ItsEnvironment;
use crate::env::Translation;
use crate::types::DeviceId;
use crate::types::EventId;
use std::collections::HashMap;

#[derive(Debug, Clone)]
struct TranslationCacheEntry {
    /// Set when the translation has changed but has not yet been reported.
    dirty: bool,
    /// The last known translation, or `None` if the interrupt is unmapped.
    translation: Option<Translation>,
}

pub(crate) struct TranslationCache {
    entries: HashMap<(DeviceId, EventId), TranslationCacheEntry>,
}

impl TranslationCache {
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
        }
    }

    /// Subscribes to translation updates for an interrupt.
    ///
    /// The entry starts dirty so that the host is told the initial translation
    /// on the next flush.
    pub fn insert_interrupt(&mut self, device_id: DeviceId, event_id: EventId) {
        self.entries.insert(
            (device_id, event_id),
            TranslationCacheEntry {
                dirty: true,
                translation: None,
            },
        );
    }

    /// Unsubscribes from translation updates for an interrupt.
    pub fn remove_interrupt(&mut self, device_id: DeviceId, event_id: EventId) {
        self.entries.remove(&(device_id, event_id));
    }

    /// Records a freshly resolved translation, marking the entry dirty if it
    /// differs from the last known value.
    pub fn update_translation(
        &mut self,
        device_id: DeviceId,
        event_id: EventId,
        translation: Option<Translation>,
    ) {
        if let Some(entry) = self.entries.get_mut(&(device_id, event_id)) {
            if entry.translation != translation {
                entry.translation = translation;
                entry.dirty = true;
            }
        }
    }

    /// Returns the interrupts currently subscribed to.
    pub fn subscribed_interrupts(&self) -> Vec<(DeviceId, EventId)> {
        self.entries.keys().copied().collect()
    }

    /// Reports every changed translation to the host and clears the dirty flags.
    pub fn flush<E: ItsEnvironment>(&mut self, env: &mut E) {
        for ((device_id, event_id), entry) in self.entries.iter_mut() {
            if entry.dirty {
                env.translation_changed(*device_id, *event_id, entry.translation);
                entry.dirty = false;
            }
        }
    }
}

impl Default for TranslationCache {
    fn default() -> Self {
        Self::new()
    }
}
