using System.IO;
using OpenTaskbar;
using OpenTaskbar.Models;
using OpenTaskbar.Services;

internal static class TaskbarRecoveryChecks
{
    internal static int Run()
    {
        var checks = 0;
        void Check(bool condition, string name)
        {
            if (!condition) throw new InvalidOperationException(name);
            checks++;
        }

        // The journal only ever holds the two taskbar state bits OpenTaskbar understands.
        Check(TaskbarReplacementSession.ParseJournal("0") == 0, "A zero journal must restore the default taskbar state.");
        Check(TaskbarReplacementSession.ParseJournal(" 3 ") == 3, "Journal parsing must tolerate surrounding whitespace.");
        foreach (var invalid in new[] { "-1", "4", "abc", "1.5", "", "18446744073709551616" })
        {
            try
            {
                TaskbarReplacementSession.ParseJournal(invalid);
                throw new InvalidOperationException($"An invalid journal value must be rejected: '{invalid}'");
            }
            catch (InvalidDataException)
            {
                checks++;
            }
        }

        // A damaged or missing journal must fail safe to a visible taskbar, never to a hidden state.
        Check(TaskbarReplacementSession.FallbackState(null) == 0, "A missing journal must fall back to state zero.");
        Check(TaskbarReplacementSession.FallbackState("corrupt") == 0, "A corrupt journal must fall back to state zero.");
        Check(TaskbarReplacementSession.FallbackState("2") == 2, "A valid journal must be used as the restore state.");

        // Faz 3: Rust `journal.rs` and the C watchdog read the same single masked decimal, with no BOM/newline.
        Check(TaskbarReplacementSession.SupportedTaskbarState == 0x03
              && TaskbarReplacementSession.DefaultTaskbarState == 0,
            "Only ABS_AUTOHIDE | ABS_ALWAYSONTOP is a valid taskbar state; the safe default is zero.");
        Check(TaskbarReplacementSession.FormatJournal(0) == "0"
              && TaskbarReplacementSession.FormatJournal(0x82) == "2"
              && TaskbarReplacementSession.FormatJournal(0xFF) == "3"
              && TaskbarReplacementSession.FormatJournal(0x04) == "0",
            "The journal writer must mask unsupported bits before handing the state to the watchdog.");
        Check(TaskbarReplacementSession.JournalTemporaryFilePath.EndsWith("taskbar-state.txt.tmp", StringComparison.Ordinal)
              && Path.GetFileName(TaskbarReplacementSession.JournalFilePath) == "taskbar-state.txt",
            "The taskbar journal temp name must match Rust's with_extension(\"txt.tmp\").");
        foreach (var damaged in new[] { "4", "0x2", "-1", "+1", "", "  ", "2\n3", "abc", "4294967296" })
        {
            Check(TaskbarReplacementSession.FallbackState(damaged) == 0,
                $"A damaged taskbar journal must fall back to a visible taskbar: '{damaged}'");
        }

        const int parentPid = 4242;
        const string token = "abc123";
        var arguments = TaskbarRecoveryWatchdog.BuildArguments(parentPid, token);
        Check(TaskbarRecoveryWatchdog.IsWatchdogInvocation([arguments]), "Watchdog arguments must be recognized.");
        Check(!TaskbarRecoveryWatchdog.IsWatchdogInvocation(["--inspect"]), "Normal arguments must not start the watchdog.");
        Check(TaskbarRecoveryWatchdog.TryParseArguments([arguments], out var pid, out var parsedToken)
            && pid == parentPid && parsedToken == token, "Watchdog arguments must round-trip.");
        Check(TaskbarRecoveryWatchdog.TryParseArguments(["--inspect", arguments], out pid, out parsedToken)
            && pid == parentPid && parsedToken == token, "Watchdog arguments must be found among other switches.");

        foreach (var malformed in new[] { "--watchdog=0:abc", "--watchdog=-1:abc", "--watchdog=42",
                     "--watchdog=42:", "--watchdog=abc:token", "--watchdog=42:a:b" })
        {
            Check(!TaskbarRecoveryWatchdog.TryParseArguments([malformed], out _, out _),
                $"Malformed watchdog arguments must be ignored: {malformed}");
        }
        Check(!TaskbarRecoveryWatchdog.TryParseArguments([], out _, out _), "An empty argument list must be ignored.");

        // Watchdog with non-existent parent must trigger safe recovery and exit without throwing.
        TaskbarRecoveryWatchdog.Run(["--watchdog=99999999:deadparent"]);
        checks++;

        // Win-based hotkeys are retired: a modifier lost during a lock transition can trigger them without Win.
        Check(MainWindow.DescribeHotkey(HotkeyChoice.WinAltD) is null, "MOD_WIN hotkeys must never be registered.");
        Check(MainWindow.DescribeHotkey(HotkeyChoice.CtrlAltD) is { Modifiers: 0x3, Key: 0x44 },
            "Legacy WinAltD selections must fall back to a safe Ctrl+Alt+D hotkey.");
        Check(MainWindow.DescribeHotkey(HotkeyChoice.AltSpace) is { Modifiers: 0x1, Key: 0x20 },
            "The launcher hotkey must remain unchanged.");

        Check(SyntheticKeyboard.SendChord() is null, "An empty synthetic chord must be a no-op.");
        Check(SyntheticKeyboard.TrackedModifiers.Count == 0, "No modifier can remain tracked after an empty chord.");

        // Deterministic bottom reservation math (#86, #92): must stack cleanly above Explorer's strip without
        // depending on lagging workArea.Bottom values, and yield full strip control when auto-hide/release is active.
        var (natGrow, extraGrow, workGrow) = TaskbarReplacementSession.ComputeBottomReservation(
            monitorBottom: 1440, shelfPixels: 60, nativeBottomTaskbarDips: 48, dpi: 96,
            autoHideEnabled: false, releaseNativeStrip: false, nativeTaskbarAtBottom: true, previousWasAutoHide: false);
        Check(natGrow == 48 && extraGrow == 12 && workGrow == 1380,
            $"60px shelf above 48px native taskbar must reserve 12px extra and put workBottom at 1380 (got {natGrow},{extraGrow},{workGrow}).");

        var (natDpi, extraDpi, workDpi) = TaskbarReplacementSession.ComputeBottomReservation(
            monitorBottom: 2160, shelfPixels: 108, nativeBottomTaskbarDips: 48, dpi: 144,
            autoHideEnabled: false, releaseNativeStrip: false, nativeTaskbarAtBottom: true, previousWasAutoHide: false);
        Check(natDpi == 72 && extraDpi == 36 && workDpi == 2052,
            $"150% DPI 108px shelf above 72px native strip must reserve 36px extra (got {natDpi},{extraDpi},{workDpi}).");

        var (natRelease, extraRelease, workRelease) = TaskbarReplacementSession.ComputeBottomReservation(
            monitorBottom: 1080, shelfPixels: 40, nativeBottomTaskbarDips: 48, dpi: 96,
            autoHideEnabled: false, releaseNativeStrip: true, nativeTaskbarAtBottom: true, previousWasAutoHide: false);
        Check(natRelease == 0 && extraRelease == 40 && workRelease == 1040,
            $"Shelf smaller than native taskbar must release native strip and reserve full 40px (got {natRelease},{extraRelease},{workRelease}).");

        var (natHide, extraHide, workHide) = TaskbarReplacementSession.ComputeBottomReservation(
            monitorBottom: 1080, shelfPixels: 60, nativeBottomTaskbarDips: 48, dpi: 96,
            autoHideEnabled: true, releaseNativeStrip: false, nativeTaskbarAtBottom: true, previousWasAutoHide: false);
        Check(natHide == 0 && extraHide == 0 && workHide == 1080,
            $"Auto-hide enabled must reserve 0px so workBottom matches monitor bottom (got {natHide},{extraHide},{workHide}).");

        // TrayHost shared-memory AppBar forwarding (#86, #92): messages carrying SHAllocShared APPBARDATAV2 buffers
        // must be re-allocated into Explorer's PID while non-shared messages pass through directly.
        Check(TrayHost.UsesSharedMemory(ManagedShell.Interop.NativeMethods.ABMsg.ABM_QUERYPOS)
            && TrayHost.UsesSharedMemory(ManagedShell.Interop.NativeMethods.ABMsg.ABM_SETPOS)
            && TrayHost.UsesSharedMemory(ManagedShell.Interop.NativeMethods.ABMsg.ABM_GETTASKBARPOS)
            && TrayHost.UsesSharedMemory(ManagedShell.Interop.NativeMethods.ABMsg.ABM_GETAUTOHIDEBAREX),
            "TrayHost must re-allocate shared memory for ABM_QUERYPOS, ABM_SETPOS, ABM_GETTASKBARPOS, and ABM_GETAUTOHIDEBAREX.");
        Check(!TrayHost.UsesSharedMemory(ManagedShell.Interop.NativeMethods.ABMsg.ABM_NEW)
            && !TrayHost.UsesSharedMemory(ManagedShell.Interop.NativeMethods.ABMsg.ABM_REMOVE)
            && !TrayHost.UsesSharedMemory(ManagedShell.Interop.NativeMethods.ABMsg.ABM_SETSTATE)
            && !TrayHost.UsesSharedMemory(ManagedShell.Interop.NativeMethods.ABMsg.ABM_GETSTATE),
            "Non-shared AppBar messages must not attempt shared-memory re-allocation.");

        // Verify ManagedShell SHAllocShared / SHLockShared roundtrip with APPBARDATAV2 on this architecture.
        var selfPid = (uint)Environment.ProcessId;
        var abdSize = (uint)System.Runtime.InteropServices.Marshal.SizeOf<ManagedShell.Interop.NativeMethods.APPBARDATAV2>();
        var hShared = ManagedShell.Interop.NativeMethods.SHAllocShared(nint.Zero, abdSize, selfPid);
        Check(hShared != nint.Zero, "SHAllocShared must allocate a valid APPBARDATAV2 shared handle.");
        try
        {
            var ptr = ManagedShell.Interop.NativeMethods.SHLockShared(hShared, selfPid);
            Check(ptr != nint.Zero, "SHLockShared must lock the allocated shared handle.");
            try
            {
                var input = new ManagedShell.Interop.NativeMethods.APPBARDATAV2
                {
                    cbSize = (int)abdSize,
                    uEdge = 3,
                    rc = new ManagedShell.Interop.NativeMethods.Rect { Left = 0, Top = 1372, Right = 2560, Bottom = 1392 }
                };
                System.Runtime.InteropServices.Marshal.StructureToPtr(input, ptr, false);
                var output = System.Runtime.InteropServices.Marshal.PtrToStructure<ManagedShell.Interop.NativeMethods.APPBARDATAV2>(ptr);
                Check(output.uEdge == 3 && output.rc.Top == 1372 && output.rc.Bottom == 1392,
                    "APPBARDATAV2 shared-memory roundtrip must preserve edge and rect coordinates.");
            }
            finally
            {
                ManagedShell.Interop.NativeMethods.SHUnlockShared(ptr);
            }
        }
        finally
        {
            ManagedShell.Interop.NativeMethods.SHFreeShared(hShared, selfPid);
        }

        return checks;
    }
}
