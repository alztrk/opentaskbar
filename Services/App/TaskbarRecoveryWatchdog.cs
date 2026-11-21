using System.Diagnostics;
using System.IO;
using System.Threading;
using Microsoft.Win32.SafeHandles;

namespace OpenTaskbar.Services;

/// <summary>
/// A tiny second process that watches the main instance and restores the native taskbar if the main process
/// disappears without saying goodbye. Managed crash handlers cannot cover End Task, taskkill /F, FailFast or
/// power loss; the only reliable recovery for those cases lives outside the dying process.
///
/// The main process owns two named auto-reset events (a heartbeat and a clean-exit signal) keyed by its PID and
/// a unique token. The watchdog exits quietly on a clean exit; otherwise it runs the journal-based restore.
/// </summary>
internal static class TaskbarRecoveryWatchdog
{
    private const string ArgumentPrefix = "--watchdog=";
    private const string CleanExitPrefix = @"Local\OpenTaskbar.CleanExit.";
    private const string HeartbeatPrefix = @"Local\OpenTaskbar.Heartbeat.";
    private const int HeartbeatIntervalMs = 1000;
    private const int HeartbeatTimeoutMs = 4000;
    // A single delayed heartbeat (GC, thread-pool starvation) must never be mistaken for a hung dock and
    // trigger a restore that would delete a live session's journals.
    private const int HeartbeatMissesRequired = 3;

    private static EventWaitHandle? _cleanExit;
    private static EventWaitHandle? _heartbeat;
    private static Timer? _heartbeatTimer;
    private static bool _started;

    internal static bool IsWatchdogInvocation(string[] args) =>
        args.Any(arg => arg.StartsWith(ArgumentPrefix, StringComparison.OrdinalIgnoreCase));

    internal static string BuildArguments(int parentPid, string token) => $"{ArgumentPrefix}{parentPid}:{token}";

    internal static bool TryParseArguments(string[] args, out int parentPid, out string token)
    {
        parentPid = 0;
        token = string.Empty;
        var argument = args.FirstOrDefault(arg => arg.StartsWith(ArgumentPrefix, StringComparison.OrdinalIgnoreCase));
        if (argument is null) return false;

        var parts = argument[ArgumentPrefix.Length..].Split(':');
        if (parts.Length != 2 || !int.TryParse(parts[0], out parentPid) || parentPid <= 0
            || string.IsNullOrWhiteSpace(parts[1]))
        {
            parentPid = 0;
            return false;
        }

        token = parts[1];
        return true;
    }

    /// <summary>Watchdog process entry point. Returns after either a clean exit or a completed recovery.</summary>
    public static void Run(string[] args)
    {
        if (!TryParseArguments(args, out var parentPid, out var token)) return;
        using var cleanExit = OpenEvent(CleanExitPrefix, parentPid, token);
        using var heartbeat = OpenEvent(HeartbeatPrefix, parentPid, token);
        if (cleanExit is null || heartbeat is null) return;

        Process? parent;
        try
        {
            parent = Process.GetProcessById(parentPid);
        }
        catch (ArgumentException)
        {
            TaskbarReplacementSession.ForceRestoreNativeTaskbar();
            return;
        }

        using (parent)
        {
            AutoResetEvent? processWaitHandle = null;
            try
            {
                if (!parent.HasExited)
                {
                    processWaitHandle = new AutoResetEvent(false)
                    {
                        SafeWaitHandle = new SafeWaitHandle(parent.Handle, ownsHandle: false)
                    };
                }
            }
            catch (InvalidOperationException)
            {
            }

            if (processWaitHandle is null)
            {
                TaskbarReplacementSession.ForceRestoreNativeTaskbar();
                return;
            }

            using (processWaitHandle)
            {
                var exitedCleanly = false;
                var heartbeatMisses = 0;
                WaitHandle[] waitHandles = [cleanExit, processWaitHandle, heartbeat];

                while (true)
                {
                    var signaled = WaitHandle.WaitAny(waitHandles, HeartbeatTimeoutMs);
                    if (signaled == 0)
                    {
                        exitedCleanly = true;
                        break;
                    }

                    if (signaled == 1 || parent.HasExited)
                    {
                        if (cleanExit.WaitOne(0))
                        {
                            exitedCleanly = true;
                        }
                        break;
                    }

                    if (signaled == WaitHandle.WaitTimeout)
                    {
                        if (++heartbeatMisses >= HeartbeatMissesRequired) break;
                        continue;
                    }
                    heartbeatMisses = 0;
                }

                if (exitedCleanly) return;
            }
        }

        var error = TaskbarReplacementSession.ForceRestoreNativeTaskbar();
        if (error is not null) Trace.TraceError(error);
    }

    /// <summary>Starts the watchdog once the taskbar replacement is actually active.</summary>
    public static void Start(int parentPid)
    {
        if (_started) return;
        var executable = Environment.ProcessPath;
        if (string.IsNullOrWhiteSpace(executable) || !File.Exists(executable)) return;
        var executablePath = Path.GetFullPath(executable);
        // `dotnet run` hosts us in dotnet.exe, which has no --watchdog entry point.
        if (Path.GetFileNameWithoutExtension(executablePath).Equals("dotnet", StringComparison.OrdinalIgnoreCase)) return;

        var token = Guid.NewGuid().ToString("N");
        try
        {
            _cleanExit = OpenEvent(CleanExitPrefix, parentPid, token);
            _heartbeat = OpenEvent(HeartbeatPrefix, parentPid, token);
            if (_cleanExit is null || _heartbeat is null)
            {
                Cleanup();
                return;
            }

            _heartbeat.Set();
            _heartbeatTimer = new Timer(_ =>
            {
                // NotifyCleanExit disposes the timer and event while a callback may already be in flight.
                try { _heartbeat?.Set(); }
                catch (ObjectDisposedException) { }
            }, null, HeartbeatIntervalMs, HeartbeatIntervalMs);

            var watchdogExecutable = EnsureWatchdogExecutable(executablePath);

            using var watchdog = Process.Start(new ProcessStartInfo(watchdogExecutable)
            {
                Arguments = BuildArguments(parentPid, token),
                CreateNoWindow = true,
                UseShellExecute = false,
                WindowStyle = ProcessWindowStyle.Hidden
            });
            _started = watchdog is not null;
            if (!_started) Cleanup();
        }
        catch (Exception ex)
        {
            Trace.TraceWarning($"Taskbar watchdog could not start: {ex}");
            Cleanup();
        }
    }

    private static string EnsureWatchdogExecutable(string mainExecutablePath)
    {
        try
        {
            var targetDir = Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "OpenTaskbar");
            Directory.CreateDirectory(targetDir);
            var watchdogExe = Path.Combine(targetDir, "OpenTaskbarWatchdog.exe");

            using var resourceStream = typeof(TaskbarRecoveryWatchdog).Assembly
                .GetManifestResourceStream("OpenTaskbar.Resources.OpenTaskbarWatchdog.exe");

            if (resourceStream is not null)
            {
                try
                {
                    var resourceBytes = new byte[resourceStream.Length];
                    var totalRead = 0;
                    while (totalRead < resourceBytes.Length)
                    {
                        var read = resourceStream.Read(resourceBytes, totalRead, resourceBytes.Length - totalRead);
                        if (read == 0) break;
                        totalRead += read;
                    }

                    var needsWrite = true;
                    if (File.Exists(watchdogExe))
                    {
                        var existing = File.ReadAllBytes(watchdogExe);
                        if (existing.AsSpan().SequenceEqual(resourceBytes))
                        {
                            needsWrite = false;
                        }
                    }

                    if (needsWrite)
                    {
                        // A kill mid-write must not leave a truncated watchdog that cannot start next time.
                        var temporary = watchdogExe + ".tmp";
                        File.WriteAllBytes(temporary, resourceBytes);
                        File.Move(temporary, watchdogExe, overwrite: true);
                    }
                }
                catch (IOException)
                {
                    // Fall through: only return a path that actually exists and can be launched.
                }
                if (File.Exists(watchdogExe)) return watchdogExe;
            }

            var appDir = Path.GetDirectoryName(mainExecutablePath) ?? string.Empty;
            var localExe = Path.Combine(appDir, "OpenTaskbarWatchdog.exe");
            if (File.Exists(localExe)) return localExe;

            return mainExecutablePath;
        }
        catch (Exception ex)
        {
            Trace.TraceWarning($"Could not prepare native watchdog binary: {ex}");
            return mainExecutablePath;
        }
    }

    /// <summary>Called on every exit path, so the watchdog never mistakes a normal shutdown for a crash.</summary>
    public static void NotifyCleanExit()
    {
        _heartbeatTimer?.Dispose();
        _heartbeatTimer = null;
        try
        {
            _cleanExit?.Set();
        }
        catch (ObjectDisposedException)
        {
        }
        Cleanup();
    }

    private static EventWaitHandle? OpenEvent(string prefix, int pid, string token)
    {
        try
        {
            return new EventWaitHandle(false, EventResetMode.AutoReset, $"{prefix}{pid}.{token}", out _);
        }
        catch (Exception ex) when (ex is UnauthorizedAccessException or ArgumentException or IOException)
        {
            Trace.TraceWarning($"Taskbar watchdog event could not be created: {ex}");
            return null;
        }
    }

    private static void Cleanup()
    {
        _cleanExit?.Dispose();
        _cleanExit = null;
        _heartbeat?.Dispose();
        _heartbeat = null;
        _started = false;
    }
}
