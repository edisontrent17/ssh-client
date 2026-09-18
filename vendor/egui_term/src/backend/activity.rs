use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// Tracks terminal output independently from whether its tab is currently drawn.
pub(super) struct Activity {
    generation: AtomicU64,
    visible: AtomicBool,
    pending: AtomicBool,
}

impl Default for Activity {
    fn default() -> Self {
        Self {
            generation: AtomicU64::new(1),
            visible: AtomicBool::new(true),
            pending: AtomicBool::new(false),
        }
    }
}

impl Activity {
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }
    pub fn set_visible(&self, visible: bool) -> bool {
        let changed = self.visible.swap(visible, Ordering::AcqRel) != visible;
        if changed {
            self.pending.store(false, Ordering::Release);
        }
        changed && visible
    }
    pub fn changed(&self) {
        self.generation.fetch_add(1, Ordering::Release);
    }
    pub fn queue_frame(&self) -> bool {
        self.visible.load(Ordering::Acquire) && !self.pending.swap(true, Ordering::AcqRel)
    }
    pub fn begin_frame(&self) {
        self.pending.store(false, Ordering::Release);
    }
}
