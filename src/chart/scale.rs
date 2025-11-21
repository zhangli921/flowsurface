pub mod linear;
pub mod timeseries;
// This entire module is temporarily stubbed out because it relies on the
// old canvas-based rendering system for axis labels. The `Caches` and `TEXT_SIZE`
// constants it depends on have been removed as part of the refactoring to a
// shader-based renderer. This needs to be re-implemented using a WGPU-compatible
// approach.