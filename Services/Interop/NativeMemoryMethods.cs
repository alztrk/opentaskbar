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
    private static readonly object IdleTrimLock = new();
    private static CancellationTokenSource? _idleTrimCts;

    public static void ScheduleIdleTrim(TimeSpan? delay = null)
    {
        CancellationTokenSource cts;
        CancellationToken token;
        lock (IdleTrimLock)
        {
            _idleTrimCts?.Cancel();
            _idleTrimCts?.Dispose();
            cts = new CancellationTokenSource();
            _idleTrimCts = cts;
            token = cts.Token;
        }
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
        lock (IdleTrimLock)
        {
            _idleTrimCts?.Cancel();
            _idleTrimCts?.Dispose();
            _idleTrimCts = null;
        }
    }

    public static void TrimWorkingSet(bool force = false)
    {
        var now = Stopwatch.GetTimestamp();
        var last = Volatile.Read(ref _lastTrimTicks);
        if (!force && last != 0 && Stopwatch.GetElapsedTime(last, now) < MinTrimInterval) return;
        Volatile.Write(ref _lastTrimTicks, now);

        try
        {
            // Scheduled trims run on a background thread; forced trims may be called from the dispatcher.
            // Two blocking, compacting Gen2 collections plus a finalizer wait froze the dock on startup.
            // A gentle non-blocking collection is enough to release short-lived garbage;
            // EmptyWorkingSet below is what actually returns pages to the OS.
            GC.Collect(2, GCCollectionMode.Optimized, blocking: false, compacting: false);

            using var process = Process.GetCurrentProcess();
            var handle = process.Handle;
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
