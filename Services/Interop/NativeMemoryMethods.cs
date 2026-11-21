using System.Diagnostics;
using System.Runtime.InteropServices;

namespace OpenTaskbar.Services;

/// <summary>
/// Trims inactive virtual memory and working set pages back to the operating system after startup
/// and during extended idle periods.
/// </summary>
internal static class NativeMemoryMethods
{
    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern bool SetProcessWorkingSetSize(nint process, nint minimumWorkingSetSize, nint maximumWorkingSetSize);

    [DllImport("psapi.dll", SetLastError = true)]
    private static extern bool EmptyWorkingSet(nint process);

    private static long _lastTrimTicks;
    private static readonly TimeSpan MinTrimInterval = TimeSpan.FromSeconds(20);
    private static CancellationTokenSource? _idleTrimCts;

    public static void ScheduleIdleTrim(TimeSpan? delay = null)
    {
        _idleTrimCts?.Cancel();
        var cts = new CancellationTokenSource();
        _idleTrimCts = cts;
        var token = cts.Token;
        Task.Delay(delay ?? TimeSpan.FromSeconds(2), token).ContinueWith(_ =>
        {
            if (!token.IsCancellationRequested)
            {
                TrimWorkingSet();
            }
        }, TaskScheduler.Default);
    }

    public static void CancelIdleTrim()
    {
        _idleTrimCts?.Cancel();
        _idleTrimCts = null;
    }

    public static void TrimWorkingSet(bool force = false)
    {
        var now = Stopwatch.GetTimestamp();
        if (!force && _lastTrimTicks != 0 && Stopwatch.GetElapsedTime(_lastTrimTicks, now) < MinTrimInterval) return;
        _lastTrimTicks = now;

        try
        {
            // These trims run on the UI dispatcher. Two blocking, compacting Gen2 collections plus a
            // finalizer wait froze the dock for a noticeable moment right after it first appeared.
            // A gentle non-blocking collection is enough to release the short-lived startup garbage;
            // EmptyWorkingSet below is what actually returns pages to the OS.
            GC.Collect(2, GCCollectionMode.Optimized, blocking: false, compacting: false);

            var handle = Process.GetCurrentProcess().Handle;
            if (handle != 0)
            {
                _ = EmptyWorkingSet(handle);
                _ = SetProcessWorkingSetSize(handle, -1, -1);
            }
        }
        catch (Exception ex)
        {
            AppLog.Warning($"Memory trim skipped: {ex.Message}");
        }
    }
}
