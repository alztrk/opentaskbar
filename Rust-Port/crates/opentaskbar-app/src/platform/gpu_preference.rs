//! Exports the standard GPU-preference symbols recognised by NVIDIA and AMD
//! drivers. Both return 0 so hybrid-GPU laptops keep the integrated,
//! power-saving GPU for a 2D taskbar instead of loading ~150 MB of shader
//! compilers from the discrete driver.
//!
//! Drivers look these symbols up by name in the process export table. The
//! `#[used]` statics keep them alive through LTO; if a lead-side `.def` file is
//! ever added for the binary, expose `NvOptimusEnablement` and
//! `AmdPowerXpressRequestHighPerformance` there as well.

#[unsafe(no_mangle)]
pub extern "C" fn NvOptimusEnablement() -> u32 {
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn AmdPowerXpressRequestHighPerformance() -> i32 {
    0
}

#[used]
static NV_OPTIMUS_ENABLEMENT_KEEP: extern "C" fn() -> u32 = NvOptimusEnablement;

#[used]
static AMD_POWER_XPRESS_KEEP: extern "C" fn() -> i32 = AmdPowerXpressRequestHighPerformance;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exports_request_the_integrated_gpu() {
        assert_eq!(NvOptimusEnablement(), 0);
        assert_eq!(AmdPowerXpressRequestHighPerformance(), 0);
    }
}
