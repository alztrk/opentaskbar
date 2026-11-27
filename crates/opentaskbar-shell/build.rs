//! Compiles the native Win32 watchdog helper for the *exact* target architecture.
//!
//! The C watchdog is embedded into the shell crate with `include_bytes!`, so an
//! x86 Rust build must never silently embed the checked-in x64 binary. The build
//! script asks `tools/watchdog/build.ps1` for an architecture-specific binary in
//! `OUT_DIR`, verifies its PE machine type, and exports the path as
//! `OT_WATCHDOG_EXE` for `watchdog.rs`.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// PE COFF machine constants (WinNT.h IMAGE_FILE_MACHINE_*).
const IMAGE_FILE_MACHINE_I386: u16 = 0x014C;
const IMAGE_FILE_MACHINE_AMD64: u16 = 0x8664;

fn pe_machine(path: &Path) -> Option<u16> {
    let bytes = fs::read(path).ok()?;
    if bytes.len() < 0x40 || &bytes[..2] != b"MZ" {
        return None;
    }
    let offset = u32::from_le_bytes(bytes[0x3C..0x40].try_into().ok()?) as usize;
    // Checked arithmetic: a corrupt/oversized e_lfanew must fail closed on any
    // host pointer width instead of wrapping and panicking while slicing.
    let pe_end = offset.checked_add(4)?;
    let machine_end = offset.checked_add(6)?;
    if machine_end > bytes.len() || &bytes[offset..pe_end] != b"PE\0\0" {
        return None;
    }
    Some(u16::from_le_bytes(
        bytes[pe_end..machine_end].try_into().ok()?,
    ))
}

fn main() {
    // `powershell.exe` is a host tool; cross-compiling the native watchdog from
    // a non-Windows host is not supported. Fail with a clear reason instead of
    // a confusing "program not found" error.
    const {
        assert!(
            cfg!(windows),
            "opentaskbar-shell's native watchdog is built by tools/watchdog/build.ps1 \
             and requires a Windows build host"
        )
    };
    let target_arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    let (arch, expected_machine) = match target_arch.as_str() {
        "x86_64" => ("x64", IMAGE_FILE_MACHINE_AMD64),
        "x86" => ("x86", IMAGE_FILE_MACHINE_I386),
        other => panic!(
            "No native watchdog build rule for target architecture `{other}`; \
             add one in crates/opentaskbar-shell/build.rs before shipping this target."
        ),
    };

    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let script = manifest_dir.join("../../tools/watchdog/build.ps1");
    // std::path::absolute (unlike canonicalize) does not add a `\\?\` prefix,
    // which powershell.exe -File would otherwise fail to resolve ($PSScriptRoot).
    let script = std::path::absolute(&script)
        .unwrap_or_else(|e| panic!("Bad watchdog build script path {}: {e}", script.display()));
    if !script.is_file() {
        panic!("Missing watchdog build script: {}", script.display());
    }
    let source = std::path::absolute(manifest_dir.join("../../tools/watchdog/watchdog.c"))
        .expect("Bad watchdog source path");
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    let output_dir = out_dir.join("watchdog");
    let watchdog = output_dir.join("OpenTaskbarWatchdog.exe");

    fs::create_dir_all(&output_dir).expect("could not create watchdog OUT_DIR");

    // -NoFallback is deliberate: an x86 build with no 32-bit toolchain must fail
    // here rather than embed a WOW64 x64 binary that cannot recover taskbars on
    // 32-bit Windows.
    let status = Command::new("powershell.exe")
        .arg("-NoProfile")
        .arg("-ExecutionPolicy")
        .arg("Bypass")
        .arg("-File")
        .arg(&script)
        .arg("-Architecture")
        .arg(arch)
        .arg("-OutputDir")
        .arg(&output_dir)
        .arg("-Force")
        .arg("-NoFallback")
        .status()
        .unwrap_or_else(|e| panic!("Could not start watchdog build script: {e}"));
    assert!(
        status.success(),
        "tools/watchdog/build.ps1 -Architecture {arch} -NoFallback failed for {target_arch}"
    );

    let actual = pe_machine(&watchdog).unwrap_or(0);
    assert!(
        actual == expected_machine,
        "watchdog PE machine mismatch: expected 0x{expected_machine:04X}, \
         got 0x{actual:04X} at {}",
        watchdog.display()
    );

    println!(
        "cargo:rustc-env=OT_WATCHDOG_EXE={}",
        watchdog.to_string_lossy()
    );
    println!("cargo:rerun-if-changed={}", source.to_string_lossy());
    println!("cargo:rerun-if-changed={}", script.to_string_lossy());
}
