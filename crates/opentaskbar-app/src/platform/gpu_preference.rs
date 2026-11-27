//! Exports the standard GPU-preference symbols recognised by NVIDIA and AMD
//! drivers. Both hold 0 so hybrid-GPU laptops keep the integrated,
//! power-saving GPU for a 2D taskbar instead of loading ~150 MB of shader
//! compilers from the discrete driver.
//!
//! Drivers read these as 32-bit **data** variables in the process export table,
//! not as function addresses. They are therefore declared as statics; exporting
//! functions under the same names could select the discrete GPU instead. The
//! `#[used]` attribute keeps the symbols alive through LTO. If a `.def` file is
//! ever added for the binary, list both names there as `DATA`.

#[unsafe(no_mangle)]
#[used]
pub static NvOptimusEnablement: u32 = 0;

#[unsafe(no_mangle)]
#[used]
pub static AmdPowerXpressRequestHighPerformance: i32 = 0;

// Compile-time proof of the exported symbol shape: a function item cannot be
// used where a `u32`/`i32` constant is required.
const _: u32 = NvOptimusEnablement;
const _: i32 = AmdPowerXpressRequestHighPerformance;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exports_are_data_words_requesting_the_integrated_gpu() {
        let nvidia: u32 = NvOptimusEnablement;
        let amd: i32 = AmdPowerXpressRequestHighPerformance;
        assert_eq!(nvidia, 0);
        assert_eq!(amd, 0);
        // Their addresses must be readable data, not executable function stubs.
        assert_ne!(&NvOptimusEnablement as *const u32, std::ptr::null());
        assert_ne!(
            &AmdPowerXpressRequestHighPerformance as *const i32,
            std::ptr::null()
        );
    }
}
