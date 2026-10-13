use std::collections::HashMap;
use std::sync::Arc;

use gpui_kit::*;

/// Keeps the decoded thumbnails of the most recently drawn tiles. GPUI's
/// default cache keeps every image it ever loaded, which for a large library
/// would hold hundreds of megabytes of thumbnails that are off screen.
pub struct ThumbnailImageCache {
    capacity: usize,
    items: HashMap<u64, Entry>,
    clock: u64,
}

struct Entry {
    item: ImageCacheItem,
    last_used: u64,
}

impl ThumbnailImageCache {
    /// `capacity` must exceed the number of tiles visible at once, or visible
    /// tiles evict each other.
    pub fn new(capacity: usize, cx: &mut App) -> Entity<Self> {
        let cache = cx.new(|_| Self {
            capacity,
            items: HashMap::new(),
            clock: 0,
        });
        cx.observe_release(&cache, |cache, cx| {
            for (_, entry) in cache.items.drain() {
                if let Some(Ok(image)) = entry.item.get() {
                    cx.drop_image(image, None);
                }
            }
        })
        .detach();
        cache
    }

    fn evict_least_recently_used(&mut self, window: &mut Window, cx: &mut App) {
        while self.items.len() > self.capacity {
            let Some(oldest) = self
                .items
                .iter()
                .min_by_key(|(_, entry)| entry.last_used)
                .map(|(key, _)| *key)
            else {
                return;
            };
            if let Some(entry) = self.items.remove(&oldest)
                && let Some(Ok(image)) = entry.item.get()
            {
                cx.drop_image(image, Some(window));
            }
        }
    }
}

impl ImageCache for ThumbnailImageCache {
    fn load(
        &mut self,
        resource: &Resource,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<Result<Arc<RenderImage>, ImageCacheError>> {
        self.clock += 1;
        let key = hash(resource);
        if !self.items.contains_key(&key) {
            self.items.insert(
                key,
                Entry {
                    item: ImageCacheItem::new(resource, cx),
                    last_used: self.clock,
                },
            );
            self.evict_least_recently_used(window, cx);
        }
        let entry = self.items.get_mut(&key)?;
        entry.last_used = self.clock;
        entry.item.use_image(window)
    }
}
