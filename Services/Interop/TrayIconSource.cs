using System.ComponentModel;
using System.Diagnostics;
using System.IO;
using System.Runtime.InteropServices;
using System.Windows.Media.Imaging;
using ManagedShell.WindowsTray;
using Microsoft.Win32;
using Microsoft.Win32.SafeHandles;

namespace OpenTaskbar.Services;

/// <summary>
/// The single authoritative snapshot of background tray apps. ManagedShell live icons are primary; the registry /
/// process snapshot from <see cref="TrayIcons.RunningApps"/> fills the gaps (hidden icons, icons not yet
/// re-registered). Entries are deduplicated on <c>(path, GUID, UID)</c>, and a registry watcher plus a low-frequency
/// process refresh keeps the snapshot current without restarting the dock.
/// </summary>
public sealed class TrayIconSource : IDisposable
{
    private const string SettingsKey = @"Control Panel\NotifyIconSettings";
    private const uint RegistryNotifyFilter = 0x00000001 | 0x00000002 | 0x00000004; // name | attributes | last set
    private const long MinRefreshIntervalMs = 750;
    private const int DebounceMs = 200;
    private const int PeriodicRefreshSeconds = 30;

    private static readonly Lazy<TrayIconSource> SharedInstance = new(static () => new TrayIconSource());

    private readonly object _gate = new();
    private readonly object _watchLock = new();
    private readonly CancellationTokenSource _lifetime = new();
    private IReadOnlyList<TrayApp> _snapshot = [];
    private Task<IReadOnlyList<TrayApp>>? _pending;
    private long _lastEnumerated;
    private int _changePending;
    private bool _disposed;

    private RegistryKey? _watchKey;
    private AutoResetEvent? _changeEvent;
    private RegisteredWaitHandle? _waitHandle;
    private bool _watcherArmed;

    /// <summary>Process-wide shared instance used by the tray button and the pinned strip.</summary>
    public static TrayIconSource Shared => SharedInstance.Value;

    /// <summary>Seam for tests: overrides the live icon collection (defaults to <see cref="TrayHost.AppIcons"/>).</summary>
    internal Func<IReadOnlyList<NotifyIcon>>? LiveIconsProvider { get; set; }

    /// <summary>Raised off-thread whenever a fresh merged snapshot is published; subscribers must marshal to the UI thread.</summary>
    public event Action? SnapshotChanged;

    public TrayIconSource()
    {
        StartWatching();
        _ = PeriodicRefreshLoopAsync();
    }

    /// <summary>The current merged snapshot (never null, never mutated in place).</summary>
    public IReadOnlyList<TrayApp> Snapshot
    {
        get { lock (_gate) return _snapshot; }
    }

    /// <summary>
    /// Returns a fresh snapshot, coalescing concurrent callers onto one enumeration. Non-forced calls made shortly
    /// after a successful enumeration reuse the cached list; registry/process changes call <see cref="Invalidate"/>
    /// and pass <paramref name="force"/> to bypass the throttle.
    /// </summary>
    public Task<IReadOnlyList<TrayApp>> RefreshAsync(CancellationToken cancellationToken = default, bool force = false)
    {
        lock (_gate)
        {
            if (_disposed) return Task.FromResult(_snapshot);
            if (_pending is { IsCompleted: false } existing) return existing;
            if (!force && _snapshot.Count > 0
                && Environment.TickCount64 - _lastEnumerated < MinRefreshIntervalMs)
            {
                return Task.FromResult(_snapshot);
            }

            var linked = CancellationTokenSource.CreateLinkedTokenSource(cancellationToken, _lifetime.Token);
            Volatile.Write(ref _changePending, 0);
            _pending = RefreshCoreAsync(linked);
            return _pending;
        }
    }

    /// <summary>Signals that the registry or process set changed; safe to call from any thread and any number of times.</summary>
    public void Invalidate()
    {
        if (_disposed) return;
        Volatile.Write(ref _changePending, 1);
        if (!_watcherArmed) StartWatching();
        _ = RefreshAsync(force: true);
    }

    private async Task<IReadOnlyList<TrayApp>> RefreshCoreAsync(CancellationTokenSource linked)
    {
        var token = linked.Token;
        try
        {
            do
            {
                Volatile.Write(ref _changePending, 0);
                var total = Stopwatch.StartNew();

                var live = ReadLiveApps();
                ShellTelemetry.Event("TrayEnumeration", ("source", "live"), ("count", live.Count),
                    ("ms", total.ElapsedMilliseconds));

                // TrayIcons.RunningApps emits its own TrayEnumeration(source=registry) event with the exact
                // registry/process timing, so do not duplicate it here.
                var fallback = await Task.Run(() => TrayIcons.RunningApps(), token).ConfigureAwait(false);

                var merged = Merge(live, fallback);
                lock (_gate)
                {
                    _snapshot = merged;
                    _lastEnumerated = Environment.TickCount64;
                }
                ShellTelemetry.Event("TrayEnumeration", ("source", "merged"), ("count", merged.Count),
                    ("ms", total.ElapsedMilliseconds));
                RaiseSnapshotChanged();

                if (Volatile.Read(ref _changePending) == 1 && !token.IsCancellationRequested)
                {
                    await Task.Delay(DebounceMs, token).ConfigureAwait(false);
                }
            }
            while (Volatile.Read(ref _changePending) == 1 && !token.IsCancellationRequested);

            lock (_gate)
            {
                return _snapshot;
            }
        }
        catch (OperationCanceledException) when (token.IsCancellationRequested)
        {
            lock (_gate)
            {
                return _snapshot;
            }
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException
            or System.ComponentModel.Win32Exception or InvalidOperationException)
        {
            AppLog.Warning($"Tray icon source enumeration failed: {ex.Message}");
            lock (_gate)
            {
                return _snapshot;
            }
        }
        finally
        {
            linked.Dispose();
            lock (_gate)
            {
                _pending = null;
            }
        }
    }

    private IReadOnlyList<TrayApp> ReadLiveApps()
    {
        try
        {
            var customIcons = LiveIconsProvider?.Invoke();
            if (customIcons is null)
            {
                var dispatcher = System.Windows.Application.Current?.Dispatcher;
                if (dispatcher is not null && !dispatcher.CheckAccess())
                {
                    if (dispatcher.HasShutdownStarted || dispatcher.HasShutdownFinished)
                        return [];
                    return dispatcher.Invoke(ReadLiveAppsCore);
                }
            }
            return ReadLiveAppsCore(customIcons);
        }
        catch (Exception ex) when (ex is InvalidOperationException or ObjectDisposedException or ArgumentException
            or TaskCanceledException or OperationCanceledException)
        {
            AppLog.Warning($"Live tray icon enumeration failed: {ex.Message}");
            return [];
        }
    }

    private IReadOnlyList<TrayApp> ReadLiveAppsCore() => ReadLiveAppsCore(null);

    private IReadOnlyList<TrayApp> ReadLiveAppsCore(IReadOnlyList<NotifyIcon>? icons)
    {
        if (icons is null)
        {
            var host = TrayHost.Current;
            icons = host is { IsRunning: true } ? host.AppIcons() : [];
        }

        var seen = new HashSet<TrayIconRef>(TrayIconRefComparer.Instance);
        var apps = new List<TrayApp>(icons.Count);
        foreach (var icon in icons)
        {
            var path = icon.Path?.Trim() ?? string.Empty;
            // Path-less system icons (volume, network, ...) have no launchable executable and are shown through
            // the live flyout; the merged snapshot is the registry/fallback representation.
            if (path.Length == 0) continue;
            var identity = new TrayIconRef(icon.GUID, path, icon.UID, IconSource.Live);
            if (!seen.Add(identity)) continue;
            var name = TrayPinIdentity.CleanTitle(icon.Title, path, Path.GetFileNameWithoutExtension(path));
            var bitmap = icon.Icon as BitmapSource;
            if (bitmap is { CanFreeze: true, IsFrozen: false })
                bitmap.Freeze();
            apps.Add(new TrayApp(name, path, bitmap,
                identity.Guid, identity.Uid, IconSource.Live));
        }
        return apps;
    }

    private static IReadOnlyList<TrayApp> Merge(IReadOnlyList<TrayApp> live, IReadOnlyList<TrayApp> fallback)
    {
        var result = new List<TrayApp>(live.Count + fallback.Count);
        var seen = new HashSet<TrayIconRef>(TrayIconRefComparer.Instance);
        var livePaths = new HashSet<string>(StringComparer.OrdinalIgnoreCase);

        foreach (var app in live)
        {
            var path = app.ExecutablePath?.Trim() ?? string.Empty;
            if (path.Length == 0) continue;
            var identity = new TrayIconRef(app.Guid, path, app.Uid, IconSource.Live);
            if (!seen.Add(identity)) continue;
            livePaths.Add(path);
            result.Add(new TrayApp(app.Name, path, app.Icon,
                identity.Guid, identity.Uid, IconSource.Live));
        }

        foreach (var app in fallback)
        {
            var path = app.ExecutablePath?.Trim() ?? string.Empty;
            if (path.Length == 0 || livePaths.Contains(path)) continue;
            var identity = new TrayIconRef(app.Guid, path, app.Uid, IconSource.Registry);
            if (!seen.Add(identity)) continue;
            result.Add(new TrayApp(app.Name, path, app.Icon,
                identity.Guid, identity.Uid, IconSource.Registry));
        }

        result.Sort((a, b) => StringComparer.CurrentCultureIgnoreCase.Compare(a.Name, b.Name));
        return result;
    }

    private void RaiseSnapshotChanged()
    {
        var handlers = SnapshotChanged;
        if (handlers is null) return;
        foreach (var handler in handlers.GetInvocationList())
        {
            if (handler is not Action callback) continue;
            try
            {
                callback();
            }
            catch (Exception ex)
            {
                AppLog.Warning($"Tray snapshot listener failed: {ex.Message}");
            }
        }
    }

    private void StartWatching()
    {
        lock (_watchLock)
        {
            if (_disposed || _watcherArmed) return;
            try
            {
                var key = Registry.CurrentUser.OpenSubKey(SettingsKey);
                if (key is null)
                {
                    ShellTelemetry.Result("TrayRegistryWatcher", "missing-key", success: false, verified: false);
                    return;
                }

                var changeEvent = new AutoResetEvent(false);
                _watchKey = key;
                _changeEvent = changeEvent;
                var status = RegNotifyChangeKeyValue(key.Handle, true, RegistryNotifyFilter,
                    changeEvent.SafeWaitHandle, true);
                if (status != 0) throw new Win32Exception(status);

                _waitHandle = ThreadPool.RegisterWaitForSingleObject(changeEvent, OnRegistryChanged, null,
                    Timeout.Infinite, executeOnlyOnce: false);
                _watcherArmed = true;
                ShellTelemetry.Result("TrayRegistryWatcher", "watch", true, true,
                    ("key", SettingsKey));
            }
            catch (Exception ex) when (ex is Win32Exception or UnauthorizedAccessException or IOException
                or ObjectDisposedException or ArgumentException)
            {
                StopWatchingLocked();
                ShellTelemetry.Result("TrayRegistryWatcher", "watch", success: false, verified: false,
                    ("error", ex.Message));
            }
        }
    }

    private void OnRegistryChanged(object? state, bool timedOut)
    {
        if (!_watcherArmed || _disposed) return;
        ArmWatcherAgain();
        Invalidate();
    }

    private void ArmWatcherAgain()
    {
        lock (_watchLock)
        {
            if (!_watcherArmed || _disposed) return;
            try
            {
                var key = _watchKey;
                var changeEvent = _changeEvent;
                if (key is null || changeEvent is null)
                {
                    _watcherArmed = false;
                    return;
                }
                var status = RegNotifyChangeKeyValue(key.Handle, true, RegistryNotifyFilter,
                    changeEvent.SafeWaitHandle, true);
                if (status != 0) throw new Win32Exception(status);
            }
            catch (Exception ex) when (ex is Win32Exception or UnauthorizedAccessException or IOException
                or ObjectDisposedException or ArgumentException)
            {
                StopWatchingLocked();
                ShellTelemetry.Result("TrayRegistryWatcher", "rearm", success: false, verified: false,
                    ("error", ex.Message));
            }
        }
    }

    private void StopWatching()
    {
        lock (_watchLock)
        {
            StopWatchingLocked();
        }
    }

    private void StopWatchingLocked()
    {
        _watcherArmed = false;
        var wait = _waitHandle;
        _waitHandle = null;
        var changeEvent = _changeEvent;
        _changeEvent = null;
        var key = _watchKey;
        _watchKey = null;
        try
        {
            wait?.Unregister(null);
        }
        catch (Exception ex) when (ex is InvalidOperationException or ObjectDisposedException)
        {
            AppLog.Warning($"Tray registry wait unregister failed: {ex.Message}");
        }
        changeEvent?.Dispose();
        key?.Dispose();
    }

    private async Task PeriodicRefreshLoopAsync()
    {
        try
        {
            while (!_lifetime.IsCancellationRequested)
            {
                await Task.Delay(TimeSpan.FromSeconds(PeriodicRefreshSeconds), _lifetime.Token).ConfigureAwait(false);
                if (_lifetime.IsCancellationRequested) return;
                // Catches process starts that do not touch the registry (or when the watcher is unavailable).
                Volatile.Write(ref _changePending, 1);
                _ = RefreshAsync(force: true);
            }
        }
        catch (OperationCanceledException)
        {
            // Shutdown.
        }
    }

    public void Dispose()
    {
        if (_disposed) return;
        _disposed = true;
        StopWatching();
        _lifetime.Cancel();
    }

    [DllImport("advapi32.dll", SetLastError = true)]
    private static extern int RegNotifyChangeKeyValue(
        SafeRegistryHandle hKey,
        [MarshalAs(UnmanagedType.Bool)] bool bWatchSubtree,
        uint dwNotifyFilter,
        SafeWaitHandle hEvent,
        [MarshalAs(UnmanagedType.Bool)] bool fAsynchronous);
}
