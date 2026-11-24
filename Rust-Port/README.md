# OpenTaskBar-Rust

A Rust port of [OpenTaskbar](https://github.com/alztrk/opentaskbar), the macOS-inspired floating dock and taskbar replacement for Windows 10 and 11.

Goals: the same dock with a fraction of the GPU, CPU and memory cost, stutter-free magnification, and an animated first-run setup wizard.

Status: early port. See [docs/PORTING_PLAN.md](docs/PORTING_PLAN.md) for the phases.

## Build

Requirements: Rust (stable, MSVC toolchain) and Visual Studio Build Tools with the C++ x64 tools and the Windows SDK.

```powershell
cargo build --release
cargo test
```

## License

MIT. Based on OpenTaskbar, © OpenTaskbar contributors.
