using System.IO;
using System.Reflection;
using System.Text;
using System.Threading;
using OpenTaskbar.Services;

// P3: simulates shell recovery signals without restarting Explorer, broadcasting TaskbarCreated,
// writing HKCU or changing the live taskbar. Reflection is used only to inject deterministic state
// into seams that are not yet injectable (see the missing-seam notes in docs/windows-taskbar-model.md §6).
internal static class ShellRecoveryChecks
{
    private const BindingFlags StaticNonPublic = BindingFlags.NonPublic | BindingFlags.Static;

    internal static int Run()
    {
        var checks = 0;
        void Check(bool condition, string name)
        {
            if (!condition) throw new InvalidOperationException(name);
            checks++;
        }

        // Correlation sequences are strictly increasing and never zero: a self-broadcast can never be
        // confused with the initial state or with a later broadcast.
        var first = ShellTelemetry.NextSequence();
        var second = ShellTelemetry.NextSequence();
        var third = ShellTelemetry.NextSequence();
        Check(first > 0 && second > first && third > second,
            "Shell telemetry sequences must be positive and strictly increasing.");

        // wParam markers are the first half of MainWindow's self/external discrimination.
        Check(TrayHost.SelfBroadcastWParam != 0 && TrayHost.RestoreBroadcastWParam != 0
            && TrayHost.SelfBroadcastWParam != TrayHost.RestoreBroadcastWParam,
            "Self and restore TaskbarCreated wParam markers must be distinct and non-zero.");

        // Simulate a self-broadcast by injecting a generated sequence into the correlation field.
        // No SendNotifyMessage / RegisterWindowMessage call is made, so the real shell is untouched.
        var sequenceField = typeof(TrayHost).GetField("_lastSelfBroadcastSequence", StaticNonPublic)!;
        var originalSequence = (long)sequenceField.GetValue(null)!;
        try
        {
            var ownSequence = ShellTelemetry.NextSequence();
            sequenceField.SetValue(null, ownSequence);

            Check(TrayHost.IsOwnBroadcast(new IntPtr(ownSequence)),
                "A broadcast carrying the recorded self sequence must be recognized as our own.");
            Check(!TrayHost.IsOwnBroadcast(new IntPtr(ownSequence + 1)),
                "A different sequence is an Explorer/watchdog broadcast and must not be treated as own.");
            Check(!TrayHost.IsOwnBroadcast(IntPtr.Zero),
                "A zero lParam carries no correlation sequence and must never be treated as own.");

            // MainWindow.WindowProc combines the wParam marker with IsOwnBroadcast(lParam).
            bool DispatcherSeesOwn(nuint wParam, long sequence) =>
                wParam == TrayHost.SelfBroadcastWParam && TrayHost.IsOwnBroadcast(new IntPtr(sequence));

            Check(DispatcherSeesOwn(TrayHost.SelfBroadcastWParam, ownSequence),
                "A self-tagged broadcast with the matching sequence must be swallowed by the dock.");
            Check(!DispatcherSeesOwn(TrayHost.RestoreBroadcastWParam, ownSequence),
                "A restore-tagged broadcast must be handled even if its lParam carries our last self sequence.");
            Check(!DispatcherSeesOwn(0, ownSequence + 7),
                "An external broadcast with a fresh sequence must be handled as an Explorer restart.");
        }
        finally
        {
            sequenceField.SetValue(null, originalSequence);
        }

        // RestoreCoordinator is a single-entry gate. The busy branch is exercised here: while the gate is
        // held, concurrent callers are rejected with RestoreBusy and NO OS restore path is entered.
        var gateField = typeof(RestoreCoordinator).GetField("Gate", StaticNonPublic)!;
        var gate = (SemaphoreSlim)gateField.GetValue(null)!;
        Check(gate.CurrentCount == 1, "The restore gate must allow exactly one caller at a time.");
        Check(gate.Wait(0), "A free restore gate must admit the first caller.");
        try
        {
            Check(!gate.Wait(0), "A second concurrent caller must be rejected by the single-entry restore gate.");
        }
        finally
        {
            gate.Release();
        }

        var log = CaptureAppLog(() =>
        {
            gate.Wait();
            try
            {
                var firstCall = Task.Run(() => RestoreCoordinator.Restore("simulated-concurrent-1"));
                var secondCall = Task.Run(() => RestoreCoordinator.Restore("simulated-concurrent-2"));
                Check(Task.WhenAll(firstCall, secondCall).Wait(TimeSpan.FromSeconds(10)),
                    "Two concurrent restore calls must both finish within the bounded gate timeout.");
                Check(firstCall.Result == "RestoreBusy" && secondCall.Result == "RestoreBusy",
                    "Concurrent restore calls must fail fast as RestoreBusy instead of entering the OS restore path.");
            }
            finally
            {
                gate.Release();
            }
        });
        Check(log.Contains("EVENT TaskbarRestore") && log.Contains("reason=busy"),
            "The rejected restore attempts must be recorded as correlated telemetry.");

        // #173: the watchdog must stay armed until restore is verified and the journal is gone.
        Check(RestoreCoordinator.CanSignalCleanExit(null, false)
              && !RestoreCoordinator.CanSignalCleanExit("ErrorTaskbarRestoreFailed", false)
              && !RestoreCoordinator.CanSignalCleanExit(null, true),
            "A clean exit may be signalled only after a verified restore with no pending recovery journal.");

        // Missing Explorer HWND degradation: the lease acquired for shell operations must never be a window
        // owned by this process, and an absent taskbar must leave an invalid lease (no self-fallback).
        var hasTaskbar = NativeTaskbarWindow.TryAcquire(out var taskbar);
        Check(!hasTaskbar
                || (taskbar.IsValid && taskbar.ProcessId != (uint)Environment.ProcessId && taskbar.IsForeign),
            "A validated shell lease must be foreign; a missing Explorer HWND must degrade rather than target our own window.");
        Check(!HwndLease.Invalid.IsValid && !HwndLease.Capture(0).IsValid,
            "Invalid/zero handles must never produce a usable shell lease.");

        RunUpdaterScriptChecks(Check);

        return checks;
    }

    private static void RunUpdaterScriptChecks(Action<bool, string> check)
    {
        const string tempExe = @"C:\Users\Kullanıcı\AppData\Local\Temp\100%Update\OpenTaskbar.exe";
        const string targetExe = @"C:\Users\Kullanıcı\Masaüstü\Özel İÖÜŞÇĞ 50%\OpenTaskbar.exe";

        var script = UpdateChecker.GenerateUpdaterScript(tempExe, targetExe);
        check(script.StartsWith("@echo off\r\nchcp 65001 >nul\r\n", StringComparison.Ordinal),
            "Updater script must switch cmd.exe to UTF-8 (chcp 65001 >nul) immediately after @echo off.");
        check(script.Contains(@"set ""SRC=C:\Users\Kullanıcı\AppData\Local\Temp\100%%Update\OpenTaskbar.exe""", StringComparison.Ordinal),
            "Updater script must escape % as %% and preserve Unicode characters in SRC.");
        check(script.Contains(@"set ""DST=C:\Users\Kullanıcı\Masaüstü\Özel İÖÜŞÇĞ 50%%\OpenTaskbar.exe""", StringComparison.Ordinal),
            "Updater script must escape % as %% and preserve Turkish/Unicode paths (Masaüstü\\Özel) in DST.");
        check(script.Contains("if not errorlevel 1 if exist \"%DST%\" goto launch", StringComparison.Ordinal),
            "Updater script must verify copy succeeded and destination exists before deleting SRC and launching.");
        check(script.Contains("if %ATTEMPTS% lss 30 goto wait_loop", StringComparison.Ordinal),
            "Updater script must retry copy up to 30 attempts.");

        static bool ThrowsArgumentException(Action action)
        {
            try
            {
                action();
                return false;
            }
            catch (ArgumentException)
            {
                return true;
            }
        }

        foreach (var bad in new[]
        {
            "",
            "   ",
            "C:\\Users\\test\r\\OpenTaskbar.exe",
            "C:\\Users\\test\n\\OpenTaskbar.exe",
            "C:\\Users\\test\"&calc\\OpenTaskbar.exe",
            "C:\\Users\\test\0\\OpenTaskbar.exe"
        })
        {
            check(ThrowsArgumentException(() => UpdateChecker.GenerateUpdaterScript(bad, targetExe)),
                $"Updater script generation must reject unsafe tempExe input: {bad.Replace("\r", "\\r").Replace("\n", "\\n")}");
            check(ThrowsArgumentException(() => UpdateChecker.GenerateUpdaterScript(tempExe, bad)),
                $"Updater script generation must reject unsafe targetExe input: {bad.Replace("\r", "\\r").Replace("\n", "\\n")}");
        }

        check(UpdateChecker.UpdaterScriptEncoding.GetPreamble().Length == 0,
            "Updater script encoding must be UTF-8 without BOM.");

        var scriptPath = Path.Combine(Path.GetTempPath(), $"opentaskbar-updater-check-{Guid.NewGuid():N}.cmd");
        try
        {
            UpdateChecker.WriteUpdaterScript(scriptPath, tempExe, targetExe);
            var rawBytes = File.ReadAllBytes(scriptPath);
            var hasUtf8Bom = rawBytes.Length >= 3 && rawBytes[0] == 0xEF && rawBytes[1] == 0xBB && rawBytes[2] == 0xBF;
            check(!hasUtf8Bom, "Written updater .cmd file must not start with a UTF-8 BOM.");
            check(rawBytes.AsSpan().StartsWith("@echo off"u8),
                "Written updater .cmd file must start directly with ASCII '@echo off' bytes.");

            var decoded = new UTF8Encoding(encoderShouldEmitUTF8Identifier: false, throwOnInvalidBytes: true).GetString(rawBytes);
            check(decoded == script && decoded.Contains(@"Masaüstü\Özel", StringComparison.Ordinal),
                "Written updater .cmd file must round-trip Turkish/Unicode paths cleanly as valid UTF-8.");
        }
        finally
        {
            if (File.Exists(scriptPath)) File.Delete(scriptPath);
        }
    }

    // Redirects AppLog to a throwaway file so telemetry from the simulated path can be asserted without
    // touching (or rotating) the user's real OpenTaskbar log.
    private static string CaptureAppLog(Action action)
    {
        var writerField = typeof(AppLog).GetField("_writer", StaticNonPublic)!;
        var pathField = typeof(AppLog).GetField("_logPath", StaticNonPublic)!;
        var originalWriter = writerField.GetValue(null);
        var originalPath = pathField.GetValue(null);
        var path = Path.Combine(Path.GetTempPath(), Path.GetRandomFileName() + ".log");
        try
        {
            var writer = new StreamWriter(path) { AutoFlush = true };
            writerField.SetValue(null, writer);
            pathField.SetValue(null, path);
            try
            {
                action();
            }
            finally
            {
                writer.Flush();
                writerField.SetValue(null, originalWriter);
                pathField.SetValue(null, originalPath);
                writer.Dispose();
            }
            return File.Exists(path) ? File.ReadAllText(path) : string.Empty;
        }
        finally
        {
            if (File.Exists(path)) File.Delete(path);
        }
    }
}
