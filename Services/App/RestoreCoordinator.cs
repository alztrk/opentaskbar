using System.Diagnostics;
using System.Threading;

namespace OpenTaskbar.Services;

/// <summary>
/// Single entry point for every "put Windows' taskbar back" path (clean exit, crash handler, startup heal,
/// watchdog and --restore). Serializes attempts, enforces a bounded wait and records a correlated result so a
/// half-restored state can no longer be diagnosed from a missing log line.
/// </summary>
public static class RestoreCoordinator
{
    private static readonly SemaphoreSlim Gate = new(1, 1);
    private static readonly TimeSpan GateTimeout = TimeSpan.FromSeconds(3);

    /// <summary>Restores the native taskbar. Returns null on success, otherwise a reportable error key/message.</summary>
    public static string? Restore(string reason)
    {
        if (!Gate.Wait(GateTimeout))
        {
            ShellTelemetry.Result("TaskbarRestore", reason, success: false, verified: false, ("reason", "busy"));
            return "RestoreBusy";
        }

        var stopwatch = Stopwatch.StartNew();
        try
        {
            var error = TaskbarReplacementSession.ForceRestoreNativeTaskbar();
            stopwatch.Stop();
            ShellTelemetry.Result("TaskbarRestore", reason, success: error is null,
                verified: error is null, ("ms", stopwatch.ElapsedMilliseconds), ("error", error ?? "none"));
            return error;
        }
        catch (Exception ex)
        {
            stopwatch.Stop();
            ShellTelemetry.Result("TaskbarRestore", reason, success: false, verified: false,
                ("ms", stopwatch.ElapsedMilliseconds), ("exception", ex.GetType().Name));
            AppLog.Error($"Taskbar restore ({reason}) failed: {ex.Message}");
            return ex.Message;
        }
        finally
        {
            Gate.Release();
        }
    }

    /// <summary>Heals a pending recovery journal only when one exists.</summary>
    public static string? HealPending(string reason)
    {
        if (!TaskbarReplacementSession.HasPendingRecovery) return null;
        return Restore(reason);
    }

    /// <summary>
    /// The out-of-process watchdog must stay armed until both restore paths succeeded and the journal is gone.
    /// #173: signalling a clean exit before verification can leave the taskbar hidden after a restart.
    /// </summary>
    internal static bool CanSignalCleanExit(string? restoreError, bool journalPending) =>
        restoreError is null && !journalPending;
}
