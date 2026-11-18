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

        const int parentPid = 4242;
        const string token = "abc123";
        var arguments = TaskbarRecoveryWatchdog.BuildArguments(parentPid, token);
        Check(TaskbarRecoveryWatchdog.IsWatchdogInvocation([arguments]), "Watchdog arguments must be recognized.");
        Check(!TaskbarRecoveryWatchdog.IsWatchdogInvocation(["--preview"]), "Normal arguments must not start the watchdog.");
        Check(TaskbarRecoveryWatchdog.TryParseArguments([arguments], out var pid, out var parsedToken)
            && pid == parentPid && parsedToken == token, "Watchdog arguments must round-trip.");
        Check(TaskbarRecoveryWatchdog.TryParseArguments(["--preview", arguments], out pid, out parsedToken)
            && pid == parentPid && parsedToken == token, "Watchdog arguments must be found among other switches.");

        foreach (var malformed in new[] { "--watchdog=0:abc", "--watchdog=-1:abc", "--watchdog=42",
                     "--watchdog=42:", "--watchdog=abc:token", "--watchdog=42:a:b" })
        {
            Check(!TaskbarRecoveryWatchdog.TryParseArguments([malformed], out _, out _),
                $"Malformed watchdog arguments must be ignored: {malformed}");
        }
        Check(!TaskbarRecoveryWatchdog.TryParseArguments([], out _, out _), "An empty argument list must be ignored.");

        // Win-based hotkeys are retired: a modifier lost during a lock transition can trigger them without Win.
        Check(MainWindow.DescribeHotkey(HotkeyChoice.WinAltD) is null, "MOD_WIN hotkeys must never be registered.");
        Check(MainWindow.DescribeHotkey(HotkeyChoice.CtrlAltD) is { Modifiers: 0x3, Key: 0x44 },
            "Legacy WinAltD selections must fall back to a safe Ctrl+Alt+D hotkey.");
        Check(MainWindow.DescribeHotkey(HotkeyChoice.AltSpace) is { Modifiers: 0x1, Key: 0x20 },
            "The launcher hotkey must remain unchanged.");

        Check(SyntheticKeyboard.SendChord() is null, "An empty synthetic chord must be a no-op.");
        Check(SyntheticKeyboard.TrackedModifiers.Count == 0, "No modifier can remain tracked after an empty chord.");

        return checks;
    }
}
