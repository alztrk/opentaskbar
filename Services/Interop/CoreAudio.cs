using System.ComponentModel;
using System.Diagnostics;
using System.IO;
using System.Runtime.InteropServices;
using System.Windows.Media.Imaging;

namespace OpenTaskbar.Services;

public sealed record AudioDeviceInfo(string Id, string Name, bool IsDefault, bool IsInput);

/// <summary>
/// Associates a Windows media source with an unambiguous WASAPI app session.
/// </summary>
internal static class AppAudioSessionMatcher
{
    internal static int Match(string processName, string? appUserModelId, string sourceId, string? windowAppUserModelId = null)
    {
        const StringComparison comparison = StringComparison.OrdinalIgnoreCase;
        if (string.IsNullOrWhiteSpace(sourceId)) return 0;
        if (string.Equals(appUserModelId, sourceId, comparison)
            || string.Equals(windowAppUserModelId, sourceId, comparison)) return 2;

        var app = ExecutableName(processName);
        if (app.IsEmpty) return 0;

        var separator = sourceId.IndexOf('!');
        if (separator >= 0)
        {
            var application = sourceId.AsSpan(separator + 1);
            return appUserModelId is null && !application.Equals("App", comparison)
                && app.Equals(application, comparison) ? 1 : 0;
        }

        var source = ExecutableName(sourceId);
        if (source.Equals(app, comparison)) return 1;
        // Chromium app and profile IDs can append dot-separated components to the browser ID.
        return (app.Equals("chrome", comparison) || app.Equals("msedge", comparison))
            && source.Length > app.Length && source[app.Length] == '.'
            && source.StartsWith(app, comparison) ? 1 : 0;
    }

    internal static AppAudioSession? Select(IReadOnlyList<AppAudioSession> sessions, string sourceId)
    {
        AppAudioSession? best = null;
        var bestRank = 0;
        foreach (var session in sessions)
        {
            var match = Match(session.Name, session.AppUserModelId, sourceId, session.WindowAppUserModelId);
            var state = session.State;
            if (session.ProcessId <= 0 || match == 0 || state == AppAudioSessionState.Unavailable) continue;

            var rank = match * 2 + (state == AppAudioSessionState.Active ? 1 : 0);
            if (rank < bestRank) continue;
            // An app ID alone cannot distinguish multiple equally plausible audio streams.
            best = rank == bestRank ? null : session;
            bestRank = rank;
        }

        foreach (var session in sessions)
        {
            if (!ReferenceEquals(session, best)) session.Dispose();
        }
        return best;
    }

    private static ReadOnlySpan<char> ExecutableName(string value)
    {
        var name = value.AsSpan().Trim();
        name = name[(name.LastIndexOfAny('\\', '/') + 1)..];
        return name.EndsWith(".exe", StringComparison.OrdinalIgnoreCase) ? name[..^4] : name;
    }
}

internal enum AppAudioSessionState
{
    Inactive,
    Active,
    Unavailable
}

public sealed record AppAudioSession(
    int ProcessId,
    string Name,
    BitmapSource? Icon,
    float Volume,
    bool IsMuted,
    Func<float, bool> SetVolume,
    Func<bool, bool> SetMute,
    Action? Release = null,
    Func<float>? GetVolume = null,
    Func<bool>? GetMute = null) : IDisposable
{
    private int _disposed;
    internal string? AppUserModelId { get; init; }
    internal string? WindowAppUserModelId { get; init; }
    internal Func<AppAudioSessionState>? ReadState { private get; init; }
    internal AppAudioSessionState State => ReadState?.Invoke() ?? AppAudioSessionState.Inactive;

    /// <summary>Live value from the audio session, when the provider supplies one.</summary>
    public float CurrentVolume => GetVolume?.Invoke() ?? Volume;
    public bool CurrentMute => GetMute?.Invoke() ?? IsMuted;

    public void Dispose()
    {
        if (Interlocked.Exchange(ref _disposed, 1) == 0) Release?.Invoke();
    }
}

/// <summary>
/// Tracks default playback and recording devices, lists endpoints, and controls app audio sessions
/// through Core Audio (WASAPI).
/// </summary>
public sealed class AudioEndpointMonitor : IDisposable
{
    private static readonly object SharedLock = new();
    private static AudioEndpointMonitor? _sharedInstance;
    private static int _sharedRefCount;

    private const int DataFlowRender = 0;
    private const int DataFlowCapture = 1;
    private const int RoleConsole = 0;
    private const int RoleMultimedia = 1;
    private const int RoleCommunications = 2;
    private const uint DeviceStateActive = 0x00000001;
    private const int ClsCtxAll = 0x17;
    private const int StgmRead = 0;
    private static readonly Guid AudioEndpointVolumeId = new("5CDF2C82-841E-4546-9722-0CF74078229A");
    private static readonly Guid AudioSessionManager2Id = new("77AA99A0-1BD6-484F-8BC7-2C654C9A9B6F");
    private static readonly PropertyKey FriendlyNameKey = new(new Guid("a45c254e-df1c-4efd-8020-67d146a850e0"), 14);

    private readonly IMMDeviceEnumerator _enumerator;
    private readonly NotificationClient _notificationClient;
    private readonly VolumeCallback _volumeCallback;
    private readonly VolumeCallback _micVolumeCallback;
    private IAudioEndpointVolume? _volume;
    private IAudioEndpointVolume? _micVolume;
    private Guid _eventContext = Guid.NewGuid();
    private long _pendingDeviceGeneration;
    private long _boundDeviceGeneration;
    private bool _disposed;

    public event Action? VolumeChanged;
    public event Action? DefaultDeviceChanged;
    public event Action? MicVolumeChanged;
    public event Action? DefaultMicChanged;

    public string? DeviceName { get; private set; }
    public bool HasDevice => _volume is not null;

    public string? MicDeviceName { get; private set; }
    public bool HasMicDevice => _micVolume is not null;

    public AudioEndpointMonitor()
    {
        _enumerator = (IMMDeviceEnumerator)new MMDeviceEnumeratorComObject();
        _notificationClient = new NotificationClient(this);
        _volumeCallback = new VolumeCallback(this, isMic: false);
        _micVolumeCallback = new VolumeCallback(this, isMic: true);
        Marshal.ThrowExceptionForHR(_enumerator.RegisterEndpointNotificationCallback(_notificationClient));
        BindDefaultDevice();
        BindDefaultMicDevice();
    }

    public float Volume
    {
        get
        {
            if (_volume is null) return 0;
            return _volume.GetMasterVolumeLevelScalar(out var level) == 0 ? level : 0;
        }
    }

    public bool Muted
    {
        get
        {
            if (_volume is null) return false;
            return _volume.GetMute(out var muted) == 0 && muted;
        }
    }

    public bool SetVolume(float level)
    {
        if (_volume is null) return false;
        var result = _volume.SetMasterVolumeLevelScalar(Math.Clamp(level, 0f, 1f), ref _eventContext);
        if (result == 0 && Muted) _volume.SetMute(false, ref _eventContext);
        return result == 0;
    }

    public bool ToggleMute() => _volume is not null && _volume.SetMute(!Muted, ref _eventContext) == 0;

    public float MicVolume
    {
        get
        {
            if (_micVolume is null) return 0;
            return _micVolume.GetMasterVolumeLevelScalar(out var level) == 0 ? level : 0;
        }
    }

    public bool MicMuted
    {
        get
        {
            if (_micVolume is null) return false;
            return _micVolume.GetMute(out var muted) == 0 && muted;
        }
    }

    public bool SetMicVolume(float level)
    {
        if (_micVolume is null) return false;
        var result = _micVolume.SetMasterVolumeLevelScalar(Math.Clamp(level, 0f, 1f), ref _eventContext);
        if (result == 0 && MicMuted) _micVolume.SetMute(false, ref _eventContext);
        return result == 0;
    }

    public bool ToggleMicMute() => _micVolume is not null && _micVolume.SetMute(!MicMuted, ref _eventContext) == 0;

    /// <summary>Rebinds to the current default playback device. Call on the UI thread.</summary>
    public void BindDefaultDevice()
    {
        if (_disposed) return;
        ReleaseVolume();
        DeviceName = null;
        if (_enumerator.GetDefaultAudioEndpoint(DataFlowRender, RoleMultimedia, out var device) != 0 || device is null)
        {
            return;
        }

        try
        {
            DeviceName = ReadFriendlyName(device);
            var iid = AudioEndpointVolumeId;
            if (device.Activate(ref iid, ClsCtxAll, 0, out var activated) == 0 && activated is IAudioEndpointVolume volume)
            {
                _volume = volume;
                _volume.RegisterControlChangeNotify(_volumeCallback);
            }
        }
        finally
        {
            Marshal.ReleaseComObject(device);
        }
    }

    /// <summary>Rebinds to the current default microphone/recording device. Call on the UI thread.</summary>
    public void BindDefaultMicDevice()
    {
        if (_disposed) return;
        ReleaseMicVolume();
        MicDeviceName = null;
        if (_enumerator.GetDefaultAudioEndpoint(DataFlowCapture, RoleMultimedia, out var device) != 0 || device is null)
        {
            return;
        }

        try
        {
            MicDeviceName = ReadFriendlyName(device);
            var iid = AudioEndpointVolumeId;
            if (device.Activate(ref iid, ClsCtxAll, 0, out var activated) == 0 && activated is IAudioEndpointVolume volume)
            {
                _micVolume = volume;
                _micVolume.RegisterControlChangeNotify(_micVolumeCallback);
            }
        }
        finally
        {
            Marshal.ReleaseComObject(device);
        }
    }

    public IReadOnlyList<AudioDeviceInfo> GetDevices(bool isRecording = false)
    {
        var list = new List<AudioDeviceInfo>();
        var flow = isRecording ? DataFlowCapture : DataFlowRender;
        string? defaultId = null;

        if (_enumerator.GetDefaultAudioEndpoint(flow, RoleMultimedia, out var defDev) == 0 && defDev is not null)
        {
            try { defDev.GetId(out defaultId); }
            finally { Marshal.ReleaseComObject(defDev); }
        }

        if (_enumerator.EnumAudioEndpoints(flow, DeviceStateActive, out var collection) != 0 || collection is null)
        {
            return list;
        }

        try
        {
            if (collection.GetCount(out var count) == 0)
            {
                for (uint i = 0; i < count; i++)
                {
                    if (collection.Item(i, out var dev) == 0 && dev is not null)
                    {
                        try
                        {
                            if (dev.GetId(out var id) == 0 && id is not null)
                            {
                                var name = ReadFriendlyName(dev) ?? (isRecording ? "Microphone" : "Speaker");
                                list.Add(new AudioDeviceInfo(id, name, string.Equals(id, defaultId, StringComparison.OrdinalIgnoreCase), isRecording));
                            }
                        }
                        finally
                        {
                            Marshal.ReleaseComObject(dev);
                        }
                    }
                }
            }
        }
        finally
        {
            Marshal.ReleaseComObject(collection);
        }

        return list;
    }

    public static bool SetDefaultDevice(string deviceId)
    {
        var policy = (IPolicyConfig)new PolicyConfigClient();
        try
        {
            Marshal.ThrowExceptionForHR(policy.SetDefaultEndpoint(deviceId, RoleConsole));
            Marshal.ThrowExceptionForHR(policy.SetDefaultEndpoint(deviceId, RoleMultimedia));
            Marshal.ThrowExceptionForHR(policy.SetDefaultEndpoint(deviceId, RoleCommunications));
            return true;
        }
        catch (Exception ex)
        {
            AppLog.Warning($"SetDefaultEndpoint failed: {ex.Message}");
            return false;
        }
        finally
        {
            Marshal.ReleaseComObject(policy);
        }
    }

    public IReadOnlyList<AppAudioSession> GetAppSessions(string? targetDeviceId = null) =>
        GetAppSessions(targetDeviceId, mediaSourceId: null);

    private IReadOnlyList<AppAudioSession> GetAppSessions(string? targetDeviceId, string? mediaSourceId)
    {
        var sessions = new List<AppAudioSession>();
        var seenSessionIds = new HashSet<string>(StringComparer.OrdinalIgnoreCase);
        var seenNames = new HashSet<string>(StringComparer.OrdinalIgnoreCase);

        void CollectFromDevice(IMMDevice device)
        {
            try
            {
                var iid = AudioSessionManager2Id;
                if (device.Activate(ref iid, ClsCtxAll, 0, out var activated) != 0 || activated is not IAudioSessionManager2 sessionManager)
                {
                    return;
                }

                try
                {
                    if (sessionManager.GetSessionEnumerator(out var sessionEnum) != 0 || sessionEnum is null)
                    {
                        return;
                    }

                    try
                    {
                        if (sessionEnum.GetCount(out var count) != 0 || count == 0) return;
                        var myPid = Environment.ProcessId;

                        for (var i = 0; i < count; i++)
                        {
                            if (sessionEnum.GetSession(i, out var control) != 0 || control is null) continue;
                            var retainedForMixer = false;
                            try
                            {
                                if (control is not IAudioSessionControl2 ctl2 || control is not ISimpleAudioVolume simpleVolume) continue;
                                if (ctl2.GetState(out var state) != 0 || state is not (0 or 1)) continue;

                                var isSystemSounds = ctl2.IsSystemSoundsSession() == 0;
                                if (ctl2.GetProcessId(out var pid) < 0) continue;
                                if (pid == myPid) continue;
                                if (mediaSourceId is not null && (isSystemSounds || pid <= 0)) continue;

                                if (ctl2.GetSessionInstanceIdentifier(out var instanceId) == 0 && !string.IsNullOrWhiteSpace(instanceId))
                                {
                                    if (!seenSessionIds.Add(instanceId)) continue;
                                }
                                else if (mediaSourceId is null && !seenSessionIds.Add($"pid:{pid}:{i}"))
                                {
                                    continue;
                                }

                                string appName;
                                BitmapSource? appIcon = null;
                                string? appUserModelId = null;
                                string? windowAppUserModelId = null;

                                if (isSystemSounds)
                                {
                                    if (!seenNames.Add("SystemSounds")) continue;
                                    appName = UiText.Get("SystemSounds");
                                    try
                                    {
                                        appIcon = ApplicationIconProvider.GetForResource(@"%SystemRoot%\System32\audiosrv.dll", -1);
                                    }
                                    catch { }
                                }
                                else if (pid > 0)
                                {
                                    try
                                    {
                                        using var proc = Process.GetProcessById(pid);
                                        appName = proc.ProcessName;
                                        if (mediaSourceId is not null)
                                        {
                                            appUserModelId = NativeWindowTracker.GetProcessPackagedAppId((uint)pid);
                                            if (!string.Equals(appUserModelId, mediaSourceId, StringComparison.OrdinalIgnoreCase))
                                            {
                                                // Desktop apps can publish an opaque media ID as a window property.
                                                windowAppUserModelId = NativeJumpListSession.ReadWindowAppId(proc.MainWindowHandle);
                                            }
                                        }
                                        else if (proc.MainModule?.FileName is { Length: > 0 } exePath)
                                        {
                                            appIcon = ApplicationIconProvider.GetForFile(exePath, 32);
                                        }
                                    }
                                    catch (Exception ex) when (ex is ArgumentException or InvalidOperationException or Win32Exception)
                                    {
                                        appName = $"App ({pid})";
                                    }
                                    if (mediaSourceId is not null
                                        && AppAudioSessionMatcher.Match(appName, appUserModelId, mediaSourceId, windowAppUserModelId) == 0)
                                        continue;
                                }
                                else
                                {
                                    ctl2.GetDisplayName(out var dn);
                                    appName = !string.IsNullOrWhiteSpace(dn) ? dn : "Audio";
                                    if (!seenNames.Add(appName)) continue;
                                }

                                if (simpleVolume.GetMasterVolume(out var vol) != 0 || simpleVolume.GetMute(out var muted) != 0) continue;

                                // The slider must retain its COM session until the mixer is closed.
                                // Releasing it here makes later volume writes fail against a disconnected RCW.
                                var handle = new AppSessionVolumeHandle(control, simpleVolume);
                                sessions.Add(new AppAudioSession(
                                    pid, appName, appIcon, vol, muted,
                                    handle.SetVolume, handle.SetMute, handle.Dispose,
                                    handle.GetVolume, handle.GetMute)
                                {
                                    AppUserModelId = appUserModelId,
                                    WindowAppUserModelId = windowAppUserModelId,
                                    ReadState = handle.GetState
                                });
                                retainedForMixer = true;
                            }
                            catch (Exception ex)
                            {
                                AppLog.Warning($"Error reading audio session: {ex.Message}");
                            }
                            finally
                            {
                                if (!retainedForMixer) Marshal.ReleaseComObject(control);
                            }
                        }
                    }
                    finally
                    {
                        Marshal.ReleaseComObject(sessionEnum);
                    }
                }
                finally
                {
                    Marshal.ReleaseComObject(sessionManager);
                }
            }
            catch (Exception ex)
            {
                AppLog.Warning($"CollectFromDevice failed: {ex.Message}");
            }
        }

        if (!string.IsNullOrEmpty(targetDeviceId))
        {
            if (_enumerator.GetDevice(targetDeviceId, out var targetDev) == 0 && targetDev is not null)
            {
                try { CollectFromDevice(targetDev); }
                finally { Marshal.ReleaseComObject(targetDev); }
                return sessions;
            }
        }

        if (mediaSourceId is null && _enumerator.GetDefaultAudioEndpoint(DataFlowRender, RoleMultimedia, out var defDevice) == 0
            && defDevice is not null)
        {
            try { CollectFromDevice(defDevice); }
            finally { Marshal.ReleaseComObject(defDevice); }
        }

        if (_enumerator.EnumAudioEndpoints(DataFlowRender, DeviceStateActive, out var coll) == 0 && coll is not null)
        {
            try
            {
                if (coll.GetCount(out var count) == 0)
                {
                    for (uint i = 0; i < count; i++)
                    {
                        if (coll.Item(i, out var dev) == 0 && dev is not null)
                        {
                            try { CollectFromDevice(dev); }
                            finally { Marshal.ReleaseComObject(dev); }
                        }
                    }
                }
            }
            finally
            {
                Marshal.ReleaseComObject(coll);
            }
        }

        return sessions;
    }

    /// <summary>
    /// Finds the unambiguous WASAPI session for a media source. The caller owns the returned session.
    /// </summary>
    public AppAudioSession? FindAppSession(string sourceId) => string.IsNullOrWhiteSpace(sourceId)
        ? null
        : AppAudioSessionMatcher.Select(GetAppSessions(targetDeviceId: null, sourceId), sourceId);

    // Owns one audio session for as long as its mixer controls are on screen.
    private sealed class AppSessionVolumeHandle(IAudioSessionControl control, ISimpleAudioVolume volume) : IDisposable
    {
        private IAudioSessionControl? _control = control;
        private readonly ISimpleAudioVolume _volume = volume;
        private Guid _eventContext = Guid.NewGuid();

        public AppAudioSessionState GetState()
        {
            try
            {
                if (_control is { } control && control.GetState(out var state) == 0 && state is 0 or 1)
                    return (AppAudioSessionState)state;
            }
            catch (COMException ex)
            {
                AppLog.Warning($"Session state unavailable: {ex.Message}");
            }
            return AppAudioSessionState.Unavailable;
        }

        public bool SetVolume(float level)
        {
            if (_control is null) return false;
            try
            {
                var result = _volume.SetMasterVolume(Math.Clamp(level, 0f, 1f), ref _eventContext);
                if (result == 0) return true;
                AppLog.Warning($"Session volume change failed: HRESULT 0x{result:X8}");
            }
            catch (Exception ex) { AppLog.Warning($"Session volume change failed: {ex.Message}"); }
            return false;
        }

        public bool SetMute(bool muted)
        {
            if (_control is null) return false;
            try
            {
                var result = _volume.SetMute(muted, ref _eventContext);
                if (result == 0) return true;
                AppLog.Warning($"Session mute change failed: HRESULT 0x{result:X8}");
            }
            catch (Exception ex) { AppLog.Warning($"Session mute change failed: {ex.Message}"); }
            return false;
        }

        public float GetVolume()
        {
            if (_control is null) return 0;
            try { return _volume.GetMasterVolume(out var level) == 0 ? level : 0; }
            catch { return 0; }
        }

        public bool GetMute()
        {
            if (_control is null) return false;
            try { return _volume.GetMute(out var muted) == 0 && muted; }
            catch { return false; }
        }

        public void Dispose()
        {
            var control = _control;
            if (control is null) return;
            _control = null;
            Marshal.ReleaseComObject(control);
        }
    }

    private static string? ReadFriendlyName(IMMDevice device)
    {
        if (device.OpenPropertyStore(StgmRead, out var store) != 0 || store is null) return null;
        try
        {
            var key = FriendlyNameKey;
            if (store.GetValue(ref key, out var value) != 0) return null;
            try
            {
                const ushort vtLpwstr = 31;
                return value.Type == vtLpwstr && value.Pointer != 0 ? Marshal.PtrToStringUni(value.Pointer) : null;
            }
            finally
            {
                PropVariantClear(ref value);
            }
        }
        finally
        {
            Marshal.ReleaseComObject(store);
        }
    }

    private void ReleaseVolume()
    {
        if (_volume is null) return;
        try { _volume.UnregisterControlChangeNotify(_volumeCallback); }
        catch (COMException) { }
        Marshal.ReleaseComObject(_volume);
        _volume = null;
    }

    private void ReleaseMicVolume()
    {
        if (_micVolume is null) return;
        try { _micVolume.UnregisterControlChangeNotify(_micVolumeCallback); }
        catch (COMException) { }
        Marshal.ReleaseComObject(_micVolume);
        _micVolume = null;
    }

    private void RaiseVolumeChanged()
    {
        if (!_disposed) VolumeChanged?.Invoke();
    }

    private void RaiseMicVolumeChanged()
    {
        if (!_disposed) MicVolumeChanged?.Invoke();
    }

    private void RaiseDefaultDeviceChanged()
    {
        if (_disposed) return;
        Interlocked.Increment(ref _pendingDeviceGeneration);
        DefaultDeviceChanged?.Invoke();
    }

    private void RaiseDefaultMicChanged()
    {
        if (_disposed) return;
        DefaultMicChanged?.Invoke();
    }

    public void Dispose()
    {
        if (_disposed) return;
        _disposed = true;
        ReleaseVolume();
        ReleaseMicVolume();
        try { _enumerator.UnregisterEndpointNotificationCallback(_notificationClient); }
        catch (COMException) { }
        Marshal.ReleaseComObject(_enumerator);
    }

    public static SharedHandle AcquireShared()
    {
        lock (SharedLock)
        {
            if (_sharedInstance is null || _sharedInstance._disposed)
            {
                _sharedInstance = new AudioEndpointMonitor();
                _sharedRefCount = 0;
            }

            _sharedRefCount++;
            return new SharedHandle(_sharedInstance);
        }
    }

    private static void ReleaseShared(AudioEndpointMonitor instance)
    {
        lock (SharedLock)
        {
            if (!ReferenceEquals(_sharedInstance, instance)) return;
            if (--_sharedRefCount > 0) return;
            _sharedInstance = null;
            _sharedRefCount = 0;
            instance.Dispose();
        }
    }

    public sealed class SharedHandle : IDisposable
    {
        private readonly AudioEndpointMonitor _owner;
        private bool _disposed;

        internal SharedHandle(AudioEndpointMonitor owner)
        {
            _owner = owner;
            _owner.VolumeChanged += OnVolumeChanged;
            _owner.DefaultDeviceChanged += OnDefaultDeviceChanged;
            _owner.MicVolumeChanged += OnMicVolumeChanged;
            _owner.DefaultMicChanged += OnDefaultMicChanged;
        }

        public event Action? VolumeChanged;
        public event Action? DefaultDeviceChanged;
        public event Action? MicVolumeChanged;
        public event Action? DefaultMicChanged;

        public string? DeviceName => _disposed ? null : _owner.DeviceName;
        public bool HasDevice => !_disposed && _owner.HasDevice;
        public float Volume => _disposed ? 0 : _owner.Volume;
        public bool Muted => !_disposed && _owner.Muted;

        public string? MicDeviceName => _disposed ? null : _owner.MicDeviceName;
        public bool HasMicDevice => !_disposed && _owner.HasMicDevice;
        public float MicVolume => _disposed ? 0 : _owner.MicVolume;
        public bool MicMuted => !_disposed && _owner.MicMuted;

        public bool SetVolume(float level) => !_disposed && _owner.SetVolume(level);
        public bool ToggleMute() => !_disposed && _owner.ToggleMute();

        public bool SetMicVolume(float level) => !_disposed && _owner.SetMicVolume(level);
        public bool ToggleMicMute() => !_disposed && _owner.ToggleMicMute();

        public IReadOnlyList<AudioDeviceInfo> GetPlaybackDevices() => _disposed ? [] : _owner.GetDevices(isRecording: false);
        public IReadOnlyList<AudioDeviceInfo> GetRecordingDevices() => _disposed ? [] : _owner.GetDevices(isRecording: true);
        public bool SelectDefaultDevice(string deviceId) => !_disposed && SetDefaultDevice(deviceId);
        public IReadOnlyList<AppAudioSession> GetAppSessions(string? targetDeviceId = null) => _disposed ? [] : _owner.GetAppSessions(targetDeviceId);
        public AppAudioSession? FindAppSession(string sourceId) => _disposed ? null : _owner.FindAppSession(sourceId);

        public void BindDefaultDevice()
        {
            if (_disposed) return;
            var pending = Interlocked.Read(ref _owner._pendingDeviceGeneration);
            if (_owner._boundDeviceGeneration == pending) return;
            _owner._boundDeviceGeneration = pending;
            _owner.BindDefaultDevice();
        }

        public void BindDefaultMicDevice()
        {
            if (_disposed) return;
            _owner.BindDefaultMicDevice();
        }

        private void OnVolumeChanged() => VolumeChanged?.Invoke();
        private void OnDefaultDeviceChanged() => DefaultDeviceChanged?.Invoke();
        private void OnMicVolumeChanged() => MicVolumeChanged?.Invoke();
        private void OnDefaultMicChanged() => DefaultMicChanged?.Invoke();

        public void Dispose()
        {
            if (_disposed) return;
            _disposed = true;
            _owner.VolumeChanged -= OnVolumeChanged;
            _owner.DefaultDeviceChanged -= OnDefaultDeviceChanged;
            _owner.MicVolumeChanged -= OnMicVolumeChanged;
            _owner.DefaultMicChanged -= OnDefaultMicChanged;
            ReleaseShared(_owner);
        }
    }

    private sealed class VolumeCallback(AudioEndpointMonitor owner, bool isMic) : IAudioEndpointVolumeCallback
    {
        public int OnNotify(nint notificationData)
        {
            if (isMic) owner.RaiseMicVolumeChanged();
            else owner.RaiseVolumeChanged();
            return 0;
        }
    }

    private sealed class NotificationClient(AudioEndpointMonitor owner) : IMMNotificationClient
    {
        public int OnDeviceStateChanged(string deviceId, uint newState) => 0;
        public int OnDeviceAdded(string deviceId) => 0;
        public int OnDeviceRemoved(string deviceId) => 0;

        public int OnDefaultDeviceChanged(int flow, int role, string? defaultDeviceId)
        {
            if (role == RoleMultimedia)
            {
                if (flow == DataFlowRender) owner.RaiseDefaultDeviceChanged();
                else if (flow == DataFlowCapture) owner.RaiseDefaultMicChanged();
            }
            return 0;
        }

        public int OnPropertyValueChanged(string deviceId, PropertyKey key) => 0;
    }

    [StructLayout(LayoutKind.Sequential)]
    public struct PropertyKey(Guid format, uint id)
    {
        public Guid Format = format;
        public uint Id = id;
    }

    [StructLayout(LayoutKind.Sequential)]
    internal struct PropVariant
    {
        // The native header is eight bytes on both architectures. The union also holds
        // a count and a pointer, so it needs two pointer-sized slots for COM to write it.
        public ushort Type;
        public ushort Reserved1;
        public ushort Reserved2;
        public ushort Reserved3;
        public nint Pointer;
        public nint UnionRemainder;
    }

    [ComImport, Guid("BCDE0395-E52F-467C-8E3D-C4579291692E")]
    private class MMDeviceEnumeratorComObject;

    [ComImport, Guid("870af99c-171d-4f9e-af0d-e63df40c2bc9")]
    private class PolicyConfigClient;

    [ComImport, Guid("F8679F50-850A-41CF-9C72-430F290290C8"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    private interface IPolicyConfig
    {
        [PreserveSig] int GetMixFormat(string pszDeviceName, out nint ppFormat);
        [PreserveSig] int GetDeviceFormat(string pszDeviceName, int bDefault, out nint ppFormat);
        [PreserveSig] int ResetDeviceFormat(string pszDeviceName);
        [PreserveSig] int SetDeviceFormat(string pszDeviceName, nint pEndpointFormat, nint pMixFormat);
        [PreserveSig] int GetProcessingPeriod(string pszDeviceName, int bDefault, out long pmftDefaultPeriod, out long pmftMinimumPeriod);
        [PreserveSig] int SetProcessingPeriod(string pszDeviceName, ref long pmftPeriod);
        [PreserveSig] int GetShareMode(string pszDeviceName, out nint pMode);
        [PreserveSig] int SetShareMode(string pszDeviceName, nint mode);
        [PreserveSig] int GetPropertyValue(string pszDeviceName, int bStore, nint key, out nint pv);
        [PreserveSig] int SetPropertyValue(string pszDeviceName, int bStore, nint key, nint pv);
        [PreserveSig] int SetDefaultEndpoint([MarshalAs(UnmanagedType.LPWStr)] string pszDeviceName, int role);
        [PreserveSig] int SetEndpointVisibility(string pszDeviceName, int bVisible);
    }

    [ComImport, Guid("A95664D2-9614-4F35-A746-DE8DB63617E6"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    private interface IMMDeviceEnumerator
    {
        [PreserveSig] int EnumAudioEndpoints(int dataFlow, uint stateMask, out IMMDeviceCollection devices);
        [PreserveSig] int GetDefaultAudioEndpoint(int dataFlow, int role, out IMMDevice? endpoint);
        [PreserveSig] int GetDevice([MarshalAs(UnmanagedType.LPWStr)] string id, out IMMDevice? device);
        [PreserveSig] int RegisterEndpointNotificationCallback(IMMNotificationClient client);
        [PreserveSig] int UnregisterEndpointNotificationCallback(IMMNotificationClient client);
    }

    [ComImport, Guid("0BD7A1BE-7A1A-44DB-8397-CC5392387B5E"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    private interface IMMDeviceCollection
    {
        [PreserveSig] int GetCount(out uint count);
        [PreserveSig] int Item(uint index, out IMMDevice device);
    }

    [ComImport, Guid("D666063F-1587-4E43-81F1-B948E807363F"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    private interface IMMDevice
    {
        [PreserveSig] int Activate(ref Guid iid, int clsCtx, nint activationParams, [MarshalAs(UnmanagedType.IUnknown)] out object? instance);
        [PreserveSig] int OpenPropertyStore(int access, out IPropertyStore? properties);
        [PreserveSig] int GetId([MarshalAs(UnmanagedType.LPWStr)] out string id);
        [PreserveSig] int GetState(out uint state);
    }

    [ComImport, Guid("886d8eeb-8cf2-4446-8d02-cdba1dbdcf99"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    private interface IPropertyStore
    {
        [PreserveSig] int GetCount(out uint count);
        [PreserveSig] int GetAt(uint index, out PropertyKey key);
        [PreserveSig] int GetValue(ref PropertyKey key, out PropVariant value);
        [PreserveSig] int SetValue(ref PropertyKey key, ref PropVariant value);
        [PreserveSig] int Commit();
    }

    [ComImport, Guid("5CDF2C82-841E-4546-9722-0CF74078229A"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    private interface IAudioEndpointVolume
    {
        [PreserveSig] int RegisterControlChangeNotify(IAudioEndpointVolumeCallback notify);
        [PreserveSig] int UnregisterControlChangeNotify(IAudioEndpointVolumeCallback notify);
        [PreserveSig] int GetChannelCount(out uint count);
        [PreserveSig] int SetMasterVolumeLevel(float levelDb, ref Guid eventContext);
        [PreserveSig] int SetMasterVolumeLevelScalar(float level, ref Guid eventContext);
        [PreserveSig] int GetMasterVolumeLevel(out float levelDb);
        [PreserveSig] int GetMasterVolumeLevelScalar(out float level);
        [PreserveSig] int SetChannelVolumeLevel(uint channel, float levelDb, ref Guid eventContext);
        [PreserveSig] int SetChannelVolumeLevelScalar(uint channel, float level, ref Guid eventContext);
        [PreserveSig] int GetChannelVolumeLevel(uint channel, out float levelDb);
        [PreserveSig] int GetChannelVolumeLevelScalar(uint channel, out float level);
        [PreserveSig] int SetMute([MarshalAs(UnmanagedType.Bool)] bool mute, ref Guid eventContext);
        [PreserveSig] int GetMute([MarshalAs(UnmanagedType.Bool)] out bool mute);
        [PreserveSig] int GetVolumeStepInfo(out uint step, out uint stepCount);
        [PreserveSig] int VolumeStepUp(ref Guid eventContext);
        [PreserveSig] int VolumeStepDown(ref Guid eventContext);
        [PreserveSig] int QueryHardwareSupport(out uint hardwareSupportMask);
        [PreserveSig] int GetVolumeRange(out float minDb, out float maxDb, out float incrementDb);
    }

    [ComImport, Guid("657804FA-D6AD-4496-8A60-352752AF4F89"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    private interface IAudioEndpointVolumeCallback
    {
        [PreserveSig] int OnNotify(nint notificationData);
    }

    [ComImport, Guid("77AA99A0-1BD6-484F-8BC7-2C654C9A9B6F"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    private interface IAudioSessionManager2
    {
        [PreserveSig] int GetAudioSessionControl(nint audioSessionGuid, uint streamFlags, out nint sessionControl);
        [PreserveSig] int GetSimpleAudioVolume(nint audioSessionGuid, uint streamFlags, out nint audioVolume);
        [PreserveSig] int GetSessionEnumerator(out IAudioSessionEnumerator sessionEnum);
        [PreserveSig] int RegisterSessionNotification(nint newNotifications);
        [PreserveSig] int UnregisterSessionNotification(nint newNotifications);
        [PreserveSig] int RegisterDuckNotification(string sessionID, nint duckNotification);
        [PreserveSig] int UnregisterDuckNotification(nint duckNotification);
    }

    [ComImport, Guid("E2F5BB11-0570-40CA-ACDD-3AA01277DEE8"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    private interface IAudioSessionEnumerator
    {
        [PreserveSig] int GetCount(out int sessionCount);
        [PreserveSig] int GetSession(int sessionIndex, out IAudioSessionControl session);
    }

    [ComImport, Guid("F4B1A599-7266-4319-A8CA-E70ACB11E8CD"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    private interface IAudioSessionControl
    {
        [PreserveSig] int GetState(out int pRetVal);
        [PreserveSig] int GetDisplayName([MarshalAs(UnmanagedType.LPWStr)] out string pRetVal);
        [PreserveSig] int SetDisplayName([MarshalAs(UnmanagedType.LPWStr)] string value, ref Guid eventContext);
        [PreserveSig] int GetIconPath([MarshalAs(UnmanagedType.LPWStr)] out string pRetVal);
        [PreserveSig] int SetIconPath([MarshalAs(UnmanagedType.LPWStr)] string value, ref Guid eventContext);
        [PreserveSig] int GetGroupingParam(out Guid pRetVal);
        [PreserveSig] int SetGroupingParam(ref Guid overrideParam, ref Guid eventContext);
        [PreserveSig] int RegisterAudioSessionNotification(nint newNotifications);
        [PreserveSig] int UnregisterAudioSessionNotification(nint newNotifications);
    }

    [ComImport, Guid("BFB7FF88-7239-4FC9-8FA2-07C950BE9C6D"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    private interface IAudioSessionControl2
    {
        [PreserveSig] int GetState(out int pRetVal);
        [PreserveSig] int GetDisplayName([MarshalAs(UnmanagedType.LPWStr)] out string pRetVal);
        [PreserveSig] int SetDisplayName([MarshalAs(UnmanagedType.LPWStr)] string value, ref Guid eventContext);
        [PreserveSig] int GetIconPath([MarshalAs(UnmanagedType.LPWStr)] out string pRetVal);
        [PreserveSig] int SetIconPath([MarshalAs(UnmanagedType.LPWStr)] string value, ref Guid eventContext);
        [PreserveSig] int GetGroupingParam(out Guid pRetVal);
        [PreserveSig] int SetGroupingParam(ref Guid overrideParam, ref Guid eventContext);
        [PreserveSig] int RegisterAudioSessionNotification(nint newNotifications);
        [PreserveSig] int UnregisterAudioSessionNotification(nint newNotifications);
        [PreserveSig] int GetSessionIdentifier([MarshalAs(UnmanagedType.LPWStr)] out string pRetVal);
        [PreserveSig] int GetSessionInstanceIdentifier([MarshalAs(UnmanagedType.LPWStr)] out string pRetVal);
        [PreserveSig] int GetProcessId(out int pRetVal);
        [PreserveSig] int IsSystemSoundsSession();
        [PreserveSig] int SetDuckingPreference([MarshalAs(UnmanagedType.Bool)] bool optOut);
    }

    [ComImport, Guid("87CE5498-68D6-44E5-9215-6DA47EF883D8"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    private interface ISimpleAudioVolume
    {
        [PreserveSig] int SetMasterVolume(float fLevel, ref Guid eventContext);
        [PreserveSig] int GetMasterVolume(out float pfLevel);
        [PreserveSig] int SetMute([MarshalAs(UnmanagedType.Bool)] bool bMute, ref Guid eventContext);
        [PreserveSig] int GetMute([MarshalAs(UnmanagedType.Bool)] out bool pbMute);
    }

    [ComImport, Guid("7991EEC9-7E89-4D85-8390-6C703CEC60C0"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    private interface IMMNotificationClient
    {
        [PreserveSig] int OnDeviceStateChanged([MarshalAs(UnmanagedType.LPWStr)] string deviceId, uint newState);
        [PreserveSig] int OnDeviceAdded([MarshalAs(UnmanagedType.LPWStr)] string deviceId);
        [PreserveSig] int OnDeviceRemoved([MarshalAs(UnmanagedType.LPWStr)] string deviceId);
        [PreserveSig] int OnDefaultDeviceChanged(int flow, int role, [MarshalAs(UnmanagedType.LPWStr)] string? defaultDeviceId);
        [PreserveSig] int OnPropertyValueChanged([MarshalAs(UnmanagedType.LPWStr)] string deviceId, PropertyKey key);
    }

    [DllImport("ole32.dll")]
    private static extern int PropVariantClear(ref PropVariant value);
}
