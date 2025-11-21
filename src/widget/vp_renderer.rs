//! This widget is a placeholder and is currently disabled.
//! The functionality is intended to be integrated into the main `ChartRenderer`
//! as part of the unified rendering architecture. This file is kept for now
//! but its contents have been removed to allow compilation.

use data::compute::vp::SparseBar;

#[allow(missing_debug_implementations)]
pub struct SessionVolumeProfile {
    pub data: Vec<SparseBar>,
}

impl SessionVolumeProfile {
    pub fn new(data: Vec<SparseBar>) -> Self {
        Self { data }
    }
}
