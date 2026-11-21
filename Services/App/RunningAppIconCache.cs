using System.Collections.Concurrent;
using System.Runtime.InteropServices;
using System.Windows.Media.Imaging;
using OpenTaskbar.Models;

namespace OpenTaskbar.Services;

/// <summary>
/// Icons for running windows. Executable resources come first. Packaged apps and apps hosted by
/// ApplicationFrameHost often have none, so the window's AppUserModelID selects the Start menu image
/// (<c>shell:AppsFolder\{AUMID}</c>) before the generic file icon is used.
/// </summary>
/// <remarks>
/// With an <c>offload</c> function the expensive lookups (icon resources, Start menu images, file icons) run off the
/// caller's thread. They took 30 ms to almost a second on the UI thread whenever a new app window appeared, freezing
/// the dock. Until a lookup finishes, <see cref="Get"/> reports no icon for that step, and <see cref="IconLoaded"/>
/// asks the caller to call <see cref="Get"/> again. All cache state is only touched on the caller's thread.
/// </remarks>
public sealed class RunningAppIconCache
{
    public const int AttemptLimit = 4;
    public static readonly TimeSpan RetryDelay = TimeSpan.FromSeconds(1);

    private readonly Func<string, BitmapSource?> _resourceIcon;
    private readonly Func<nint, string?> _appId;
    private readonly Func<string, BitmapSource?> _appIcon;
    private readonly Func<string, BitmapSource?> _fileIcon;
    private readonly TimeProvider _clock;
    private readonly Func<Func<BitmapSource?>, Task<BitmapSource?>>? _offload;
    private readonly Dictionary<string, BitmapSource?> _resourceIcons = new(StringComparer.OrdinalIgnoreCase);
    private readonly Dictionary<string, BitmapSource?> _fileIcons = new(StringComparer.OrdinalIgnoreCase);
    private readonly Dictionary<string, BitmapSource> _appIcons = new(StringComparer.OrdinalIgnoreCase);
    private readonly HashSet<string> _appIconMisses = new(StringComparer.OrdinalIgnoreCase);
    private readonly HashSet<string> _loading = new(StringComparer.OrdinalIgnoreCase);
    private readonly Dictionary<nint, Lookup> _windows = [];

    /// <summary>Raised on the caller's thread when an offloaded lookup finished; call <see cref="Get"/> again.</summary>
    public event Action? IconLoaded;

    public RunningAppIconCache()
        : this(
            Guard<string, BitmapSource>(path => ApplicationIconProvider.GetForResource(path, 0)),
            Guard<nint, string>(window => NativeJumpListSession.ReadWindowAppId(window) ?? NativeWindowTracker.GetPackagedAppId(window)),
            Guard<string, BitmapSource>(appId => ApplicationIconProvider.GetForShellItem(@"shell:AppsFolder\" + appId, ApplicationIconProvider.PreferredIconPixels, iconOnly: true)),
            Guard<string, BitmapSource>(path => ApplicationIconProvider.GetForFile(path)),
            TimeProvider.System,
            IconLoaderThread.Run)
    {
    }

    public RunningAppIconCache(
        Func<string, BitmapSource?> resourceIcon,
        Func<nint, string?> appId,
        Func<string, BitmapSource?> appIcon,
        Func<string, BitmapSource?> fileIcon,
        TimeProvider clock,
        Func<Func<BitmapSource?>, Task<BitmapSource?>>? offload = null)
    {
        _resourceIcon = resourceIcon;
        _appId = appId;
        _appIcon = appIcon;
        _fileIcon = fileIcon;
        _clock = clock;
        _offload = offload;
    }

    /// <summary>True while a window without an identity icon still has lookups left; call <see cref="Get"/> again after <see cref="RetryDelay"/>.</summary>
    public bool HasPendingLookups
    {
        get
        {
            var now = _clock.GetTimestamp();
            return _windows.Values.Any(lookup => IsPending(lookup, now));
        }
    }

    /// <summary>True while an offloaded lookup is still running; <see cref="IconLoaded"/> follows when it ends.</summary>
    public bool IsLoading => _loading.Count > 0;

    /// <summary>True while the lookups for this window are running, so an unrelated app keeps its own icon.</summary>
    public bool IsLoadingFor(string executablePath, nint window)
    {
        if (_loading.Contains("resource:" + executablePath) || _loading.Contains("file:" + executablePath)) return true;
        return _windows.TryGetValue(window, out var lookup)
            && lookup.AppId is { } appId
            && _loading.Contains("app:" + appId);
    }

    public BitmapSource? Get(string executablePath, nint window)
    {
        if (!TryCached(_resourceIcons, "resource:", executablePath, _resourceIcon, out var icon)) return null;

        // Shell images are keyed by identity, never by executable: ApplicationFrameHost.exe hosts many apps.
        if (icon is null)
        {
            icon = GetAppIcon(window, out var appIconLoading);
            if (appIconLoading) return null;
        }

        if (icon is null && !TryCached(_fileIcons, "file:", executablePath, _fileIcon, out icon)) return null;

        // Application icons remain fixed to their native high-resolution system icons.
        return icon;
    }

    /// <summary>Clears cached icon bitmaps so a DPI or display change reloads icons cleanly.</summary>
    public void Clear()
    {
        _resourceIcons.Clear();
        _fileIcons.Clear();
        _appIcons.Clear();
        _appIconMisses.Clear();
        _windows.Clear();
    }

    /// <summary>Drops entries for windows and executables that are no longer tracked, so the caches stay bounded.</summary>
    public void Retain(IReadOnlyList<TrackedWindow> windows)
    {
        var handles = windows.Select(window => window.Handle).ToHashSet();
        foreach (var handle in _windows.Keys.Where(handle => !handles.Contains(handle)).ToList())
        {
            _windows.Remove(handle);
        }

        var appIds = _windows.Values.Select(lookup => lookup.AppId).OfType<string>().ToHashSet(StringComparer.OrdinalIgnoreCase);
        foreach (var appId in _appIcons.Keys.Where(appId => !appIds.Contains(appId)).ToList())
        {
            _appIcons.Remove(appId);
        }
        _appIconMisses.RemoveWhere(appId => !appIds.Contains(appId));

        var paths = windows.Select(window => window.ExecutablePath).OfType<string>().ToHashSet(StringComparer.OrdinalIgnoreCase);
        foreach (var path in _resourceIcons.Keys.Where(path => !paths.Contains(path)).ToList())
        {
            _resourceIcons.Remove(path);
            _fileIcons.Remove(path);
        }
    }

    /// <summary>Cached result, or a synchronous load, or (when offloading) a background load: false while it runs.</summary>
    private bool TryCached(Dictionary<string, BitmapSource?> cache, string kind, string key,
        Func<string, BitmapSource?> load, out BitmapSource? icon)
    {
        if (cache.TryGetValue(key, out icon)) return true;
        if (_offload is null)
        {
            cache[key] = icon = load(key);
            return true;
        }

        StartLoad(kind + key, () => load(key), result => cache[key] = result);
        return false;
    }

    private BitmapSource? GetAppIcon(nint window, out bool loading)
    {
        loading = false;
        if (window == 0) return null;
        var now = _clock.GetTimestamp();
        var known = _windows.TryGetValue(window, out var lookup);
        if (known && (!IsPending(lookup, now) || now < lookup.NextAttempt))
        {
            return lookup.Icon;
        }

        // A window can be listed before it publishes its AppUserModelID, so a miss is retried a few times.
        // The deadline also ends retries for windows that are no longer asked for, such as a background frame.
        var appId = lookup.AppId ?? _appId(window);
        BitmapSource? icon = null;
        if (appId is not null && !_appIcons.TryGetValue(appId, out icon))
        {
            if (_offload is null)
            {
                icon = _appIcon(appId);
                if (icon is not null) _appIcons[appId] = icon;
            }
            else if (!_appIconMisses.Remove(appId))
            {
                // Still loading: no attempt is used up until the result is known. The window keeps its identity,
                // so Retain (run by the refresh this load triggers) does not drop the image it is about to use.
                StartLoad("app:" + appId, () => _appIcon(appId), result =>
                {
                    if (result is not null) _appIcons[appId] = result;
                    else _appIconMisses.Add(appId);
                });
                var retryDelay = (long)(RetryDelay.TotalSeconds * _clock.TimestampFrequency);
                _windows[window] = new Lookup(appId, null, lookup.Attempts, now,
                    known ? lookup.Deadline : now + retryDelay * AttemptLimit);
                loading = true;
                return null;
            }
        }

        var delay = (long)(RetryDelay.TotalSeconds * _clock.TimestampFrequency);
        var deadline = known ? lookup.Deadline : now + delay * AttemptLimit;
        _windows[window] = new Lookup(appId, icon, lookup.Attempts + 1, now + delay, deadline);
        return icon;
    }

    private void StartLoad(string id, Func<BitmapSource?> load, Action<BitmapSource?> store)
    {
        if (!_loading.Add(id)) return;
        var context = SynchronizationContext.Current;
        _offload!(load).ContinueWith(task =>
        {
            var result = task.IsCompletedSuccessfully ? task.Result : null;
            void Apply()
            {
                _loading.Remove(id);
                store(result);
                IconLoaded?.Invoke();
            }

            if (context is null) Apply();
            else context.Post(_ => Apply(), null);
        }, TaskContinuationOptions.ExecuteSynchronously);
    }

    private static bool IsPending(Lookup lookup, long now) =>
        lookup.Icon is null && lookup.Attempts < AttemptLimit && now < lookup.Deadline;

    // The default sources call into the Shell; a failure there must only cost the icon.
    private static Func<T, TResult?> Guard<T, TResult>(Func<T, TResult?> source) where TResult : class => value =>
    {
        try
        {
            return source(value);
        }
        catch (Exception ex) when (ex is ExternalException or ArgumentException or InvalidOperationException
            or UnauthorizedAccessException or System.IO.IOException)
        {
            AppLog.Warning($"Application icon lookup failed: {ex.Message}");
            return null;
        }
    };

    private readonly record struct Lookup(string? AppId, BitmapSource? Icon, int Attempts, long NextAttempt, long Deadline);

    /// <summary>
    /// One background STA thread for Shell icon work: shell items and icon handlers expect an apartment thread,
    /// and a single thread keeps the loads in order without competing for the CPU with the UI.
    /// </summary>
    private static class IconLoaderThread
    {
        private static readonly BlockingCollection<Action> Work = new();
        private static readonly Lazy<Thread> Worker = new(() =>
        {
            var thread = new Thread(() =>
            {
                foreach (var item in Work.GetConsumingEnumerable()) item();
            })
            {
                IsBackground = true,
                Name = "Running app icon loader",
                Priority = ThreadPriority.BelowNormal
            };
            thread.SetApartmentState(ApartmentState.STA);
            thread.Start();
            return thread;
        });

        public static Task<BitmapSource?> Run(Func<BitmapSource?> load)
        {
            _ = Worker.Value;
            var completion = new TaskCompletionSource<BitmapSource?>(TaskCreationOptions.RunContinuationsAsynchronously);
            Work.Add(() =>
            {
                try
                {
                    var icon = load();
                    // Icons cross to the UI thread; unfrozen bitmaps would throw there.
                    if (icon is { IsFrozen: false } && icon.CanFreeze) icon.Freeze();
                    completion.SetResult(icon is { IsFrozen: true } ? icon : null);
                }
                catch (Exception ex)
                {
                    completion.SetException(ex);
                }
            });
            return completion.Task;
        }
    }
}
