using System.ComponentModel;
using System.Diagnostics;
using System.Globalization;
using System.Windows;
using System.Windows.Automation;
using System.Windows.Controls;
using System.Windows.Interop;
using System.Windows.Media;
using OpenTaskbar.Models;
using OpenTaskbar.Services;
using Windows.Devices.Enumeration;

namespace OpenTaskbar.Widgets;

public enum BluetoothDeviceKind
{
    Generic,
    Headphones,
    Mouse,
    Keyboard,
    Speaker,
    Watch,
    Phone,
    Gamepad
}

public sealed record BluetoothDeviceBatteryInfo(
    string Name,
    string Id,
    bool IsConnected,
    int BatteryLevel,
    BluetoothDeviceKind Kind = BluetoothDeviceKind.Generic,
    bool IsCharging = false,
    bool IsStale = false,
    string? SourceLabel = null,
    DateTimeOffset? SampledAt = null,
    string? LevelOverrideText = null,
    int? SecondaryBatteryLevel = null);

/// <summary>
/// A single dock card that lists connected wireless and Bluetooth devices side by side with a device-type icon,
/// battery percentage or coarse level, charging bolt indicator, and multi-protocol hover flyout.
/// </summary>
public sealed class BluetoothBatteryWidget : DockWidget
{
    public static string Id => "BluetoothBattery";

    public override bool SupportsCompactMode => true;
    public override int ShrinkPriority => 32;
    public override double CompactWidthEstimate => _compactWidth;
    public override double FullWidthEstimate => _fullWidth;

    internal const int WM_DEVICECHANGE = 0x0219;
    internal const int DBT_DEVNODES_CHANGED = 0x0007;
    internal const int DBT_DEVICEARRIVAL = 0x8000;
    internal const int DBT_DEVICEREMOVECOMPLETE = 0x8004;

    private static readonly int[] PollSecondPresets = [15, 30, 60, 120, 300];
    private static readonly int[] LowThresholdPresets = [0, 10, 20, 30];
    private const long DeviceArrivalCooldownMs = 10_000;
    private static readonly SolidColorBrush FullChargeBrush = CreateFrozenBrush(Color.FromRgb(0x34, 0xD3, 0x99));

    private static SolidColorBrush CreateFrozenBrush(Color color)
    {
        var brush = new SolidColorBrush(color);
        brush.Freeze();
        return brush;
    }

    // Windows publishes the same 0..100 battery byte under both names. This is also the value
    // shown in Settings > Bluetooth, and it covers classic headsets/mice with no GATT service.
    // Bluetooth AEP enumeration is intentionally NOT used: on Windows it starts a fresh discovery
    // that takes ~30 s per protocol. PnP device nodes carry the same battery data in ~150 ms.
    private const string BatteryLevelProperty = "{104EA319-6EE2-4701-BD47-8DDBF425BBE5} 2";
    private const string BatteryLifeProperty = "System.Devices.BatteryLife";
    private const string ContainerProperty = "System.Devices.ContainerId";
    private const string AepIdProperty = "System.Devices.Aep.AepId";
    private const string PresentProperty = "System.Devices.Present";
    private const string DevPkeyIsPresentProperty = "{540b947e-8b40-45bc-a8a2-6a0b894cbda2} 5";
    private const string LastArrivalDateProperty = "{83da6326-97a6-4088-9453-a1923f573b29} 102";
    private const string ConnectedProperty = "System.Devices.Aep.IsConnected";
    private const string CategoryProperty = "System.Devices.Aep.Category";
    private const string AppearanceProperty = "System.Devices.Aep.Bluetooth.Le.Appearance";

    // PnP node enumeration is fast (~150 ms). The AEP id on each node lets us look up the exact
    // endpoint directly (~60 ms for all devices, in parallel) without the 30 s discovery scan
    // that a full AssociationEndpoint enumeration would trigger.
    private static readonly string[] NodeProperties =
    [
        ContainerProperty,
        AepIdProperty,
        BatteryLevelProperty,
        BatteryLifeProperty,
        PresentProperty,
        DevPkeyIsPresentProperty,
        LastArrivalDateProperty
    ];

    private static readonly string[] AepProperties =
    [
        ConnectedProperty,
        CategoryProperty,
        AppearanceProperty
    ];

    private readonly record struct RenderedTileState(
        string Id,
        string Name,
        BluetoothDeviceKind Kind,
        int BatteryLevel,
        bool IsCharging,
        bool IsStale,
        string? LevelOverrideText,
        int? SecondaryBatteryLevel,
        bool IsLowWarning);

    private readonly StackPanel _deviceTiles = new()
    {
        Orientation = Orientation.Horizontal,
        VerticalAlignment = VerticalAlignment.Center,
        HorizontalAlignment = HorizontalAlignment.Center
    };
    private readonly TextBlock _emptyText = new()
    {
        Text = "—",
        FontSize = 11.5,
        FontWeight = FontWeights.SemiBold,
        VerticalAlignment = VerticalAlignment.Center
    };
    private readonly Button _cardButton;
    private IReadOnlyList<BluetoothDeviceBatteryInfo> _rawDevices = [];
    private IReadOnlyList<BluetoothDeviceBatteryInfo> _devices = [];
    private RenderedTileState[] _lastRenderedTiles = [];
    private bool _lastRenderedCompact;
    private bool _lastRenderedHoverExpanded;
    private int _refreshFlag;
    private volatile bool _disposed;
    private long _lastRefreshTickMs;
    private long _lastDeviceChangeAcceptedTickMs;
    private double _fullWidth = 78;
    private double _compactWidth = 46;

    private HwndSource? _hwndSource;
    private HwndSourceHook? _hwndHook;
    private CancellationTokenSource? _deviceChangeDebounceCts;
    private WeakReference<StackPanel>? _activeFlyoutPanel;
    private readonly Dictionary<string, int> _notifiedThresholdByDevice = new(StringComparer.OrdinalIgnoreCase);

    internal int TileRebuildCount { get; private set; }
    internal int SettingsApplyCount { get; private set; }
    internal long EffectivePollIntervalMs => Math.Clamp(Settings.WirelessBatteryPollSeconds, 15, 300) * 1000L;

    internal Func<Task<IReadOnlyList<BluetoothDeviceBatteryInfo>>> BatteryReader { get; set; }
    internal Action OpenSettingsAction { get; set; } = DefaultOpenBluetoothSettings;
    internal Func<string, string, string, string?> RenamePrompt { get; set; } = TextPrompt.Ask;
    internal Action<string> LowBatteryNotifier { get; set; } = NotificationService.ShowInfoToast;
    internal Func<string> DiagnosticsProvider { get; set; } = WirelessBatteryAggregator.BuildLivePrivacySafeDiagnosticSummary;
    internal Action<string> ClipboardSetter { get; set; } = DefaultSetClipboardText;

    public BluetoothBatteryWidget()
    {
        BatteryReader = () => WirelessBatteryAggregator.ReadAllAsync(Settings.WirelessBatteryListenOnly);
        _emptyText.SetResourceReference(StyleProperty, "WidgetText");
        _emptyText.SetResourceReference(ForegroundProperty, "TextSecondary");

        _cardButton = new Button { Padding = new Thickness(6, 0, 6, 0) };
        _cardButton.SetResourceReference(StyleProperty, "WidgetCardButton");
        _cardButton.Click += (_, _) => OpenSettingsAction();
        Content = _cardButton;
        Loaded += OnWidgetLoaded;
        Unloaded += OnWidgetUnloaded;
        UpdateDisplay();
    }

    protected override void OnActivated()
    {
        _lastRefreshTickMs = 0;
        Interlocked.Exchange(ref _lastDeviceChangeAcceptedTickMs, 0);
        AttachDeviceChangeHook();
        RefreshBatteries();
    }

    protected override void OnDeactivated()
    {
        _activeFlyoutPanel = null;
        DetachDeviceChangeHook();
        CancelDeviceChangeDebounce();
    }

    protected override void OnDisposed() => _disposed = true;

    protected override void OnSettingsChanged()
    {
        SettingsApplyCount++;
        _devices = ApplyUserPreferences(_rawDevices, Settings);
        EvaluateLowBatteryNotifications(_devices);
        UpdateDisplay();
    }

    protected override void OnTick(DateTime now)
    {
        if (Environment.TickCount64 - _lastRefreshTickMs >= EffectivePollIntervalMs)
            RefreshBatteries();
    }

    protected override void OnLanguageChanged()
    {
        _rawDevices = RelocalizeCoarseLevels(_rawDevices);
        _devices = ApplyUserPreferences(_rawDevices, Settings);
        UpdateDisplay();
    }

    internal static IReadOnlyList<BluetoothDeviceBatteryInfo> RelocalizeCoarseLevels(
        IReadOnlyList<BluetoothDeviceBatteryInfo> devices)
    {
        if (devices is null || devices.Count == 0)
            return [];

        var output = new List<BluetoothDeviceBatteryInfo>(devices.Count);
        for (var i = 0; i < devices.Count; i++)
        {
            var dev = devices[i];
            if (!string.IsNullOrWhiteSpace(dev.LevelOverrideText)
                && (string.Equals(dev.SourceLabel, "XInput", StringComparison.OrdinalIgnoreCase)
                    || dev.Id.StartsWith("xinput:", StringComparison.OrdinalIgnoreCase)))
            {
                var localized = XInputBatteryProvider.GetLocalizedLevelTextForBucket(dev.BatteryLevel);
                output.Add(string.Equals(localized, dev.LevelOverrideText, StringComparison.Ordinal)
                    ? dev
                    : dev with { LevelOverrideText = localized });
            }
            else
            {
                output.Add(dev);
            }
        }
        return output;
    }

    protected override void OnCompactChanged(bool compact) => ApplyDensityLayout();
    protected override void OnHoverExpandedChanged(bool expanded) => ApplyDensityLayout();

    private void OnWidgetLoaded(object sender, RoutedEventArgs e)
    {
        if (IsActive && !_disposed)
            AttachDeviceChangeHook();
    }

    private void OnWidgetUnloaded(object sender, RoutedEventArgs e)
    {
        DetachDeviceChangeHook();
    }

    private void AttachDeviceChangeHook()
    {
        if (_hwndSource is not null || _disposed || !IsActive)
            return;

        var source = PresentationSource.FromVisual(this) as HwndSource;
        var application = Application.Current;
        if (source is null && application?.Dispatcher.CheckAccess() == true && application.MainWindow is { } mainWin)
        {
            source = PresentationSource.FromVisual(mainWin) as HwndSource;
        }

        if (source is null || source.IsDisposed)
            return;

        _hwndHook = WndProcDeviceChange;
        _hwndSource = source;
        _hwndSource.AddHook(_hwndHook);
    }

    private void DetachDeviceChangeHook()
    {
        if (_hwndSource is not null && _hwndHook is not null)
        {
            try
            {
                if (!_hwndSource.IsDisposed)
                    _hwndSource.RemoveHook(_hwndHook);
            }
            catch
            {
            }
        }
        _hwndSource = null;
        _hwndHook = null;
    }

    private void CancelDeviceChangeDebounce()
    {
        var cts = Interlocked.Exchange(ref _deviceChangeDebounceCts, null);
        if (cts is not null)
        {
            try { cts.Cancel(); } catch { }
            cts.Dispose();
        }
    }

    private nint WndProcDeviceChange(nint hwnd, int msg, nint wParam, nint lParam, ref bool handled)
    {
        if (msg == WM_DEVICECHANGE && IsActive && !_disposed)
        {
            var eventType = unchecked((int)wParam.ToInt64());
            if (eventType is DBT_DEVNODES_CHANGED or DBT_DEVICEARRIVAL or DBT_DEVICEREMOVECOMPLETE)
            {
                NotifyDeviceChange(1500);
            }
        }
        return nint.Zero;
    }

    internal void NotifyDeviceChange(int debounceMs = 500)
    {
        if (_disposed || !IsActive) return;

        var newCts = new CancellationTokenSource();
        var oldCts = Interlocked.Exchange(ref _deviceChangeDebounceCts, newCts);
        if (oldCts is not null)
        {
            try { oldCts.Cancel(); } catch { }
            oldCts.Dispose();
        }

        var token = newCts.Token;
        var clampedDelay = Math.Clamp(debounceMs, 20, 2000);
        _ = Task.Run(async () =>
        {
            try
            {
                await Task.Delay(clampedDelay, token).ConfigureAwait(false);
                if (token.IsCancellationRequested || _disposed || !IsActive)
                    return;

                if (debounceMs >= 1000)
                {
                    var now = Environment.TickCount64;
                    var last = Interlocked.Read(ref _lastDeviceChangeAcceptedTickMs);
                    if (last != 0 && now - last < DeviceArrivalCooldownMs)
                        return;
                    Interlocked.Exchange(ref _lastDeviceChangeAcceptedTickMs, now);
                }

                RefreshBatteries();
            }
            catch (OperationCanceledException)
            {
            }
            catch
            {
            }
        }, CancellationToken.None);
    }

    private void ApplyDensityLayout()
    {
        var compact = IsCompact && !IsHoverExpanded;
        // Compact keeps only the type icons; percentages (and optional charging bolt) come back in expanded or hover-expanded mode.
        foreach (var child in _deviceTiles.Children)
        {
            if (child is Border { Child: StackPanel stack } && stack.Children.Count > 1)
            {
                for (var i = 1; i < stack.Children.Count; i++)
                    stack.Children[i].Visibility = compact ? Visibility.Collapsed : Visibility.Visible;
            }
        }
        _lastRenderedCompact = IsCompact;
        _lastRenderedHoverExpanded = IsHoverExpanded;
        RaiseContentWidthChanged();
    }

    internal async Task RefreshBatteriesAsync(bool requireActive = false)
    {
        try
        {
            var list = await BatteryReader().ConfigureAwait(false);
            if (_disposed || (requireActive && !IsActive)) return;
            if (Dispatcher.CheckAccess())
            {
                if (_disposed || (requireActive && !IsActive)) return;
                ApplyFreshDeviceList(list);
            }
            else
            {
                if (Dispatcher.HasShutdownStarted || Dispatcher.HasShutdownFinished) return;
                await Dispatcher.InvokeAsync(() =>
                {
                    if (_disposed || (requireActive && !IsActive)) return;
                    ApplyFreshDeviceList(list);
                });
            }
        }
        catch
        {
        }
    }

    private void ApplyFreshDeviceList(IReadOnlyList<BluetoothDeviceBatteryInfo> rawList)
    {
        _rawDevices = rawList ?? [];
        _devices = ApplyUserPreferences(_rawDevices, Settings);
        EvaluateLowBatteryNotifications(_devices);
        UpdateDisplay();
    }

    internal void RefreshBatteries()
    {
        if (_disposed || Interlocked.CompareExchange(ref _refreshFlag, 1, 0) != 0) return;
        _lastRefreshTickMs = Environment.TickCount64;
        _ = Task.Run(async () =>
        {
            try { await RefreshBatteriesAsync(requireActive: true).ConfigureAwait(false); }
            finally { Interlocked.Exchange(ref _refreshFlag, 0); }
        });
    }

    internal static IReadOnlyList<BluetoothDeviceBatteryInfo> ApplyUserPreferences(
        IReadOnlyList<BluetoothDeviceBatteryInfo> rawDevices,
        DockSettings settings)
    {
        if (rawDevices is null || rawDevices.Count == 0)
            return [];

        var output = new List<BluetoothDeviceBatteryInfo>(rawDevices.Count);
        foreach (var device in rawDevices)
        {
            if (IsDeviceIgnored(device.Id, device.Name, settings))
                continue;

            var displayName = ResolveDeviceDisplayName(device.Id, device.Name, settings);
            output.Add(string.Equals(displayName, device.Name, StringComparison.Ordinal)
                ? device
                : device with { Name = displayName });
        }

        return output;
    }

    internal static bool IsDeviceIgnored(string? id, string? name, DockSettings settings)
    {
        if (WirelessBatteryAggregator.IsIgnoredWiredDevice(0, 0, name))
            return true;
        if (!string.IsNullOrWhiteSpace(id)
            && id.Contains("2E3C", StringComparison.OrdinalIgnoreCase)
            && id.Contains("C368", StringComparison.OrdinalIgnoreCase))
        {
            return true;
        }

        if (settings.IgnoredWirelessDevices is not { Count: > 0 } ignored)
            return false;

        for (var i = 0; i < ignored.Count; i++)
        {
            var entry = ignored[i]?.Trim();
            if (string.IsNullOrEmpty(entry)) continue;
            if (!string.IsNullOrWhiteSpace(id) && string.Equals(entry, id.Trim(), StringComparison.OrdinalIgnoreCase))
                return true;
            if (!string.IsNullOrWhiteSpace(name) && string.Equals(entry, name.Trim(), StringComparison.OrdinalIgnoreCase))
                return true;
        }

        return false;
    }

    internal static string ResolveDeviceDisplayName(string? id, string? originalName, DockSettings settings)
    {
        var fallback = string.IsNullOrWhiteSpace(originalName) ? "Wireless Device" : originalName.Trim();
        if (settings.WirelessDeviceAliases is not { Count: > 0 } aliases)
            return fallback;

        if (!string.IsNullOrWhiteSpace(id) && TryLookupAlias(aliases, id.Trim(), out var byId))
            return byId;

        if (!string.IsNullOrWhiteSpace(originalName) && TryLookupAlias(aliases, originalName.Trim(), out var byName))
            return byName;

        return fallback;
    }

    private static bool TryLookupAlias(IReadOnlyDictionary<string, string> aliases, string key, out string alias)
    {
        if (aliases.TryGetValue(key, out var direct) && !string.IsNullOrWhiteSpace(direct))
        {
            alias = direct.Trim();
            return true;
        }

        foreach (var pair in aliases)
        {
            if (string.Equals(pair.Key.Trim(), key, StringComparison.OrdinalIgnoreCase)
                && !string.IsNullOrWhiteSpace(pair.Value))
            {
                alias = pair.Value.Trim();
                return true;
            }
        }

        alias = string.Empty;
        return false;
    }

    internal void EvaluateLowBatteryNotifications(IReadOnlyList<BluetoothDeviceBatteryInfo> currentDevices)
    {
        var threshold = Settings.WirelessBatteryLowThreshold;
        if (threshold is not (10 or 20 or 30))
        {
            _notifiedThresholdByDevice.Clear();
            return;
        }

        if (!IsActive || Settings.FocusModeEnabled)
            return;

        foreach (var device in currentDevices)
        {
            var key = !string.IsNullOrWhiteSpace(device.Id) ? device.Id.Trim() : device.Name.Trim();
            if (string.IsNullOrEmpty(key)) continue;

            // Reset spam guard when device starts charging or recovers above threshold.
            if (device.IsCharging || device.BatteryLevel > threshold)
            {
                _notifiedThresholdByDevice.Remove(key);
                continue;
            }

            if (!device.IsConnected || device.IsStale || device.BatteryLevel < 0)
                continue;

            // Only notify once per threshold breach until the device charges or recovers above threshold.
            if (_notifiedThresholdByDevice.TryGetValue(key, out var alreadyNotifiedThreshold)
                && alreadyNotifiedThreshold == threshold)
            {
                continue;
            }

            _notifiedThresholdByDevice[key] = threshold;
            var message = UiText.Format("WirelessBatteryLowToast", device.Name, device.BatteryLevel);
            try
            {
                LowBatteryNotifier(message);
            }
            catch
            {
            }
        }
    }

    internal void SetDeviceAlias(string deviceIdOrName, string? newAlias)
    {
        if (string.IsNullOrWhiteSpace(deviceIdOrName)) return;
        var key = deviceIdOrName.Trim();
        if (key.Length > 256) key = key[..256];

        var dict = new Dictionary<string, string>(StringComparer.OrdinalIgnoreCase);
        if (Settings.WirelessDeviceAliases is not null)
        {
            foreach (var (k, v) in Settings.WirelessDeviceAliases)
            {
                if (!string.IsNullOrWhiteSpace(k) && !string.IsNullOrWhiteSpace(v) && k.Trim().Length <= 256)
                    dict[k.Trim()] = v.Trim();
            }
        }

        if (string.IsNullOrWhiteSpace(newAlias))
        {
            dict.Remove(key);
        }
        else
        {
            if (!dict.ContainsKey(key) && dict.Count >= 128)
                return;
            var trimmedAlias = newAlias.Trim();
            if (trimmedAlias.Length > 120)
                trimmedAlias = trimmedAlias[..120];
            dict[key] = trimmedAlias;
        }

        var updated = Settings with
        {
            WirelessDeviceAliases = dict.Count > 0 ? dict : null
        };
        CommitUpdatedSettings(updated);
    }

    internal void HideDevice(string deviceIdOrName)
    {
        if (string.IsNullOrWhiteSpace(deviceIdOrName)) return;
        var key = deviceIdOrName.Trim();
        if (key.Length > 256) key = key[..256];

        var list = new List<string>();
        if (Settings.IgnoredWirelessDevices is not null)
        {
            foreach (var item in Settings.IgnoredWirelessDevices)
            {
                if (!string.IsNullOrWhiteSpace(item) && item.Trim().Length <= 256 && !list.Contains(item.Trim(), StringComparer.OrdinalIgnoreCase))
                    list.Add(item.Trim());
            }
        }

        if (!list.Contains(key, StringComparer.OrdinalIgnoreCase))
        {
            if (list.Count >= 128) return;
            list.Add(key);
        }

        var updated = Settings with
        {
            IgnoredWirelessDevices = list.Count > 0 ? list : null
        };
        CommitUpdatedSettings(updated);
    }

    internal void RestoreIgnoredDevices()
    {
        if (Settings.IgnoredWirelessDevices is null || Settings.IgnoredWirelessDevices.Count == 0)
            return;

        var updated = Settings with { IgnoredWirelessDevices = null };
        CommitUpdatedSettings(updated);
    }

    internal void CyclePollInterval()
    {
        var current = Settings.WirelessBatteryPollSeconds;
        var idx = Array.IndexOf(PollSecondPresets, current);
        var next = PollSecondPresets[(idx + 1) % PollSecondPresets.Length];
        CommitUpdatedSettings(Settings with { WirelessBatteryPollSeconds = next });
    }

    internal void CycleLowBatteryThreshold()
    {
        var current = Settings.WirelessBatteryLowThreshold;
        var idx = Array.IndexOf(LowThresholdPresets, current);
        var next = LowThresholdPresets[(idx + 1) % LowThresholdPresets.Length];
        CommitUpdatedSettings(Settings with { WirelessBatteryLowThreshold = next });
    }

    internal void ToggleListenOnly()
    {
        CommitUpdatedSettings(Settings with { WirelessBatteryListenOnly = !Settings.WirelessBatteryListenOnly });
        RefreshBatteries();
    }

    internal string CopyDiagnosticsToClipboard()
    {
        var summary = DiagnosticsProvider();
        try
        {
            ClipboardSetter(summary);
        }
        catch
        {
        }
        return summary;
    }

    private void CommitUpdatedSettings(DockSettings updated)
    {
        if (UpdateSettings is not null)
        {
            var before = SettingsApplyCount;
            UpdateSettings(updated);
            if (SettingsApplyCount == before)
            {
                ApplySettings(updated);
            }
        }
        else
        {
            ApplySettings(updated);
        }
    }

    internal void UpdateDisplay()
    {
        var connected = _devices.Where(device => device.IsConnected).ToList();
        if (connected.Count == 0)
        {
            _lastRenderedTiles = [];
            _cardButton.Content = _emptyText;
            _emptyText.Text = _devices.Count > 0 ? "—" : UiText.Get("BluetoothNoDevicesAbbr");
            _cardButton.ToolTip = UiText.Get("WidgetBluetoothBatteryDescription");
            AutomationProperties.SetName(_cardButton, UiText.Get("WidgetBluetoothBattery"));
            _fullWidth = 78;
            _compactWidth = 46;
            RefreshOpenFlyout();
            RaiseContentWidthChanged();
            return;
        }

        var threshold = Settings.WirelessBatteryLowThreshold;
        if (!MatchesRenderedSignature(connected, threshold))
        {
            _deviceTiles.Children.Clear();
            var nextSignature = new RenderedTileState[connected.Count];
            for (var i = 0; i < connected.Count; i++)
            {
                var dev = connected[i];
                var isLow = IsLowBatteryWarning(dev.BatteryLevel, dev.IsCharging, threshold);
                var levelText = FormatLevelText(dev);
                nextSignature[i] = new RenderedTileState(
                    dev.Id,
                    dev.Name,
                    dev.Kind,
                    dev.BatteryLevel,
                    dev.IsCharging,
                    dev.IsStale,
                    levelText,
                    dev.SecondaryBatteryLevel,
                    isLow);
                _deviceTiles.Children.Add(CreateDeviceTile(dev, isLast: i == connected.Count - 1));
            }
            _lastRenderedTiles = nextSignature;
            _lastRenderedCompact = IsCompact;
            _lastRenderedHoverExpanded = IsHoverExpanded;
            TileRebuildCount++;
        }
        else
        {
            // Keep per-tile tooltips fresh on language change without recreating any WPF elements.
            for (var i = 0; i < connected.Count && i < _deviceTiles.Children.Count; i++)
            {
                if (_deviceTiles.Children[i] is Border tileBorder)
                    tileBorder.ToolTip = FormatDeviceTooltip(connected[i]);
            }
        }

        _cardButton.Content = _deviceTiles;
        _cardButton.ToolTip = string.Join("\n", connected.Select(FormatDeviceTooltip));
        AutomationProperties.SetName(_cardButton,
            $"{UiText.Get("WidgetBluetoothBattery")}: {string.Join(", ", connected.Select(device => device.Name))}");
        _fullWidth = Math.Max(78, connected.Count * 62 + 12);
        _compactWidth = Math.Max(46, connected.Count * 24 + 8);
        RefreshOpenFlyout();
        RaiseContentWidthChanged();
    }

    private void RefreshOpenFlyout()
    {
        if (_activeFlyoutPanel is not null && _activeFlyoutPanel.TryGetTarget(out var panel))
        {
            PopulateHoverFlyoutPanel(panel);
        }
    }

    private bool MatchesRenderedSignature(IReadOnlyList<BluetoothDeviceBatteryInfo> connected, int lowThreshold)
    {
        if (!ReferenceEquals(_cardButton.Content, _deviceTiles))
            return false;
        if (_lastRenderedTiles.Length != connected.Count || _deviceTiles.Children.Count != connected.Count)
            return false;
        if (_lastRenderedCompact != IsCompact || _lastRenderedHoverExpanded != IsHoverExpanded)
            return false;

        for (var i = 0; i < connected.Count; i++)
        {
            var dev = connected[i];
            var prev = _lastRenderedTiles[i];
            var isLow = IsLowBatteryWarning(dev.BatteryLevel, dev.IsCharging, lowThreshold);
            var levelText = FormatLevelText(dev);
            if (!string.Equals(prev.Id, dev.Id, StringComparison.Ordinal)
                || !string.Equals(prev.Name, dev.Name, StringComparison.Ordinal)
                || prev.Kind != dev.Kind
                || prev.BatteryLevel != dev.BatteryLevel
                || prev.IsCharging != dev.IsCharging
                || prev.IsStale != dev.IsStale
                || !string.Equals(prev.LevelOverrideText, levelText, StringComparison.Ordinal)
                || prev.SecondaryBatteryLevel != dev.SecondaryBatteryLevel
                || prev.IsLowWarning != isLow)
            {
                return false;
            }
        }

        return true;
    }

    internal static bool IsFullChargeHighlight(int batteryLevel, bool isCharging) =>
        isCharging && batteryLevel >= 100;

    internal static bool IsLowBatteryWarning(int batteryLevel, bool isCharging, int configuredThreshold = 0)
    {
        if (batteryLevel < 0) return false;
        var effectiveThreshold = configuredThreshold is 10 or 20 or 30 ? configuredThreshold : 20;
        return batteryLevel <= 15 || (!isCharging && batteryLevel <= effectiveThreshold);
    }

    internal static string FormatLevelText(BluetoothDeviceBatteryInfo device)
    {
        if (!string.IsNullOrWhiteSpace(device.LevelOverrideText))
        {
            if (device.LevelOverrideText.StartsWith("WirelessBatteryLevel", StringComparison.Ordinal)
                && UiText.TryGet(device.LevelOverrideText, out var localized)
                && !string.IsNullOrWhiteSpace(localized))
            {
                return localized;
            }
            return device.LevelOverrideText!;
        }
        return device.BatteryLevel >= 0
            ? $"{device.BatteryLevel.ToString(CultureInfo.InvariantCulture)}%"
            : "—";
    }

    private static string FormatDeviceTooltip(BluetoothDeviceBatteryInfo device)
    {
        var levelText = FormatLevelText(device);
        var summary = $"{device.Name}: {levelText}";
        if (device.IsCharging)
            summary += $" ({UiText.Get("WirelessBatteryCharging")})";
        else if (device.IsStale && device.SampledAt is { } sampledAt)
        {
            var elapsed = DateTimeOffset.UtcNow - sampledAt;
            var minutes = Math.Max(1, (int)Math.Round(elapsed.TotalMinutes));
            summary += $" ({UiText.Format("WirelessBatteryLastReading", UiText.Format("MinutesShort", minutes))})";
        }
        if (device.SecondaryBatteryLevel is >= 0 and <= 100)
            summary += $" · {UiText.Format("WirelessBatterySpareDock", device.SecondaryBatteryLevel.Value)}";
        return summary;
    }

    private FrameworkElement CreateDeviceTile(BluetoothDeviceBatteryInfo device, bool isLast)
    {
        var icon = CreateDeviceIcon(device.Kind);
        var compact = IsCompact && !IsHoverExpanded;
        var levelText = FormatLevelText(device);
        var isFullCharge = IsFullChargeHighlight(device.BatteryLevel, device.IsCharging);
        var isLow = !isFullCharge && IsLowBatteryWarning(device.BatteryLevel, device.IsCharging, Settings.WirelessBatteryLowThreshold);

        var percent = new TextBlock
        {
            Text = levelText,
            FontSize = 12,
            FontWeight = FontWeights.SemiBold,
            Margin = new Thickness(5, 0, 0, 0),
            VerticalAlignment = VerticalAlignment.Center,
            Typography = { NumeralAlignment = FontNumeralAlignment.Tabular },
            Visibility = compact ? Visibility.Collapsed : Visibility.Visible
        };
        percent.SetResourceReference(StyleProperty, "WidgetText");
        if (isFullCharge)
            percent.Foreground = FullChargeBrush;
        else
            percent.SetResourceReference(ForegroundProperty, isLow ? "WidgetWarning" : "TextPrimary");

        var stack = new StackPanel { Orientation = Orientation.Horizontal, VerticalAlignment = VerticalAlignment.Center };
        stack.Children.Add(icon);
        stack.Children.Add(percent); // Index 1 is strictly preserved as the percentage/level TextBlock.

        if (device.IsCharging)
        {
            var bolt = new ContentControl
            {
                Content = Application.Current?.TryFindResource("IconZap"),
                Width = 10,
                Height = 10,
                Margin = new Thickness(3, 0, 0, 0),
                VerticalAlignment = VerticalAlignment.Center,
                IsHitTestVisible = false,
                Visibility = compact ? Visibility.Collapsed : Visibility.Visible
            };
            bolt.SetResourceReference(StyleProperty, "WidgetGlyph");
            if (isFullCharge)
                bolt.Foreground = FullChargeBrush;
            else
                bolt.SetResourceReference(ForegroundProperty, "WidgetAccent");
            stack.Children.Add(bolt);
        }

        var border = new Border
        {
            Child = stack,
            Height = 20,
            Opacity = device.IsStale ? 0.55 : 1.0,
            Padding = new Thickness(0, 0, isLast ? 0 : 9, 0),
            Margin = new Thickness(0, 0, isLast ? 0 : 9, 0),
            BorderThickness = new Thickness(0, 0, isLast ? 0 : 1, 0),
            ToolTip = FormatDeviceTooltip(device)
        };
        if (!isLast) border.SetResourceReference(Border.BorderBrushProperty, "StrokeSubtle");
        return border;
    }

    private static ContentControl CreateDeviceIcon(BluetoothDeviceKind kind)
    {
        var icon = new ContentControl
        {
            Content = Application.Current?.TryFindResource(IconKey(kind))
                      ?? Application.Current?.TryFindResource("IconBluetooth"),
            Width = 15,
            Height = 15,
            VerticalAlignment = VerticalAlignment.Center,
            IsHitTestVisible = false
        };
        icon.SetResourceReference(StyleProperty, "WidgetIcon");
        icon.SetResourceReference(ForegroundProperty, "TextSecondary");
        return icon;
    }

    private static string IconKey(BluetoothDeviceKind kind) => kind switch
    {
        BluetoothDeviceKind.Headphones => "IconHeadphones",
        BluetoothDeviceKind.Mouse => "IconMouse",
        BluetoothDeviceKind.Keyboard => "IconKeyboard",
        BluetoothDeviceKind.Speaker => "IconVolume",
        BluetoothDeviceKind.Watch => "IconWatch",
        BluetoothDeviceKind.Phone => "IconPhone",
        BluetoothDeviceKind.Gamepad => "IconGamepad",
        _ => "IconBluetooth"
    };

    public override UIElement? BuildHoverFlyoutContent()
    {
        var panel = new StackPanel { Width = 296 };
        _activeFlyoutPanel = new WeakReference<StackPanel>(panel);
        PopulateHoverFlyoutPanel(panel);
        return panel;
    }

    private void PopulateHoverFlyoutPanel(StackPanel panel)
    {
        panel.Children.Clear();

        var refreshLabel = UiText.Get("WirelessBatteryRefresh");
        var refreshBtn = DockFlyout.FlatButton(
            new TextBlock
            {
                Text = refreshLabel,
                FontSize = 11,
                FontWeight = FontWeights.SemiBold
            },
            (_, _) => RefreshBatteries(),
            refreshLabel);
        refreshBtn.Padding = new Thickness(7, 3, 7, 3);
        refreshBtn.VerticalAlignment = VerticalAlignment.Center;
        panel.Children.Add(DockFlyout.Header(UiText.Get("WidgetBluetoothBattery"), refreshBtn, bottomMargin: 6));

        if (_devices.Count == 0)
        {
            panel.Children.Add(DockFlyout.Caption(UiText.Get("BluetoothNoDevices")));
            AppendFlyoutFooterControls(panel);
            return;
        }

        var listPanel = new StackPanel { Margin = new Thickness(0, 2, 0, 2) };
        foreach (var device in _devices)
        {
            var capturedDevice = device;
            var deviceKey = !string.IsNullOrWhiteSpace(capturedDevice.Id) ? capturedDevice.Id : capturedDevice.Name;

            var rowStack = new StackPanel
            {
                Margin = new Thickness(0, 4, 0, 6),
                Opacity = device.IsStale ? 0.65 : 1.0,
                ToolTip = string.IsNullOrWhiteSpace(device.SourceLabel) ? device.Name : $"{device.Name} · {device.SourceLabel}"
            };

            var itemGrid = new Grid();
            itemGrid.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
            itemGrid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
            itemGrid.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });

            var icon = CreateDeviceIcon(device.Kind);
            Grid.SetColumn(icon, 0);
            itemGrid.Children.Add(icon);

            var nameStack = new StackPanel { Orientation = Orientation.Vertical, Margin = new Thickness(8, 0, 8, 0) };

            nameStack.Children.Add(new TextBlock
            {
                Text = device.Name,
                FontSize = 12,
                FontWeight = FontWeights.SemiBold,
                Foreground = (Brush)FindResource("TextPrimary"),
                TextTrimming = TextTrimming.CharacterEllipsis,
                VerticalAlignment = VerticalAlignment.Center
            });

            var subtitleText = BuildDeviceSubtitle(device);
            if (device.SecondaryBatteryLevel is >= 0 and <= 100)
            {
                subtitleText += " · " + UiText.Format("WirelessBatterySpareDock", device.SecondaryBatteryLevel.Value);
            }
            nameStack.Children.Add(new TextBlock
            {
                Text = subtitleText,
                FontSize = 11,
                Margin = new Thickness(0, 1, 0, 0),
                TextTrimming = TextTrimming.CharacterEllipsis,
                Foreground = device.IsCharging || (device.IsConnected && !device.IsStale)
                    ? (Brush)FindResource("WidgetAccent")
                    : (Brush)FindResource("TextSecondary")
            });

            Grid.SetColumn(nameStack, 1);
            itemGrid.Children.Add(nameStack);

            var valueRow = new StackPanel
            {
                Orientation = Orientation.Horizontal,
                VerticalAlignment = VerticalAlignment.Center,
                Margin = new Thickness(4, 0, 0, 0)
            };

            var isFullCharge = IsFullChargeHighlight(device.BatteryLevel, device.IsCharging);
            if (device.IsCharging)
            {
                var bolt = new ContentControl
                {
                    Content = Application.Current?.TryFindResource("IconZap"),
                    Width = 10,
                    Height = 10,
                    Margin = new Thickness(0, 0, 3, 0),
                    VerticalAlignment = VerticalAlignment.Center,
                    IsHitTestVisible = false
                };
                bolt.SetResourceReference(StyleProperty, "WidgetGlyph");
                if (isFullCharge)
                    bolt.Foreground = FullChargeBrush;
                else
                    bolt.SetResourceReference(ForegroundProperty, "WidgetAccent");
                valueRow.Children.Add(bolt);
            }

            var isLow = !isFullCharge && IsLowBatteryWarning(device.BatteryLevel, device.IsCharging, Settings.WirelessBatteryLowThreshold);
            var batteryText = new TextBlock
            {
                Text = FormatLevelText(device),
                FontSize = 11.5,
                FontWeight = FontWeights.SemiBold,
                VerticalAlignment = VerticalAlignment.Center,
                Typography = { NumeralAlignment = FontNumeralAlignment.Tabular }
            };
            batteryText.SetResourceReference(StyleProperty, "WidgetText");
            if (isFullCharge)
                batteryText.Foreground = FullChargeBrush;
            else
                batteryText.SetResourceReference(ForegroundProperty, isLow ? "WidgetWarning" : "TextPrimary");
            valueRow.Children.Add(batteryText);

            var renameTip = UiText.Get("WirelessBatteryRename");
            var renameBtn = CreateRowActionButton("IconEdit", renameTip, (_, _) =>
            {
                var promptMsg = UiText.Format("WirelessBatteryRenamePrompt", capturedDevice.Name);
                var answer = RenamePrompt(renameTip, promptMsg, capturedDevice.Name);
                if (answer is not null)
                {
                    SetDeviceAlias(deviceKey, answer);
                }
            });
            renameBtn.Margin = new Thickness(4, 0, 0, 0);
            valueRow.Children.Add(renameBtn);

            var hideTip = UiText.Get("WirelessBatteryHideDevice");
            var hideBtn = CreateRowActionButton("IconEyeOff", hideTip, (_, _) => HideDevice(deviceKey));
            hideBtn.Margin = new Thickness(2, 0, 0, 0);
            valueRow.Children.Add(hideBtn);

            Grid.SetColumn(valueRow, 2);
            itemGrid.Children.Add(valueRow);
            rowStack.Children.Add(itemGrid);

            if (device.BatteryLevel >= 0)
            {
                const double barTrackWidth = 260.0;
                var clampedLevel = Math.Clamp(device.BatteryLevel, 0, 100);
                var barFill = new Border
                {
                    Height = 3.5,
                    CornerRadius = new CornerRadius(1.75),
                    HorizontalAlignment = HorizontalAlignment.Left,
                    Width = Math.Max(4.0, barTrackWidth * clampedLevel / 100.0)
                };
                barFill.SetResourceReference(
                    Border.BackgroundProperty,
                    isLow ? "WidgetWarning" : "WidgetAccent");

                var barTrack = new Border
                {
                    Height = 3.5,
                    CornerRadius = new CornerRadius(1.75),
                    Margin = new Thickness(0, 5, 0, 0),
                    Child = barFill
                };
                barTrack.SetResourceReference(Border.BackgroundProperty, "WidgetTrack");
                rowStack.Children.Add(barTrack);
            }

            listPanel.Children.Add(rowStack);
        }

        panel.Children.Add(listPanel);
        AppendFlyoutFooterControls(panel);
    }

    private void AppendFlyoutFooterControls(StackPanel panel)
    {
        if (Settings.IgnoredWirelessDevices is { Count: > 0 } ignored)
        {
            var restoreLabel = UiText.Format("WirelessBatteryRestoreHidden", ignored.Count);
            var restoreBtn = DockFlyout.FlatButton(
                new TextBlock
                {
                    Text = restoreLabel,
                    FontSize = 10.5,
                    FontWeight = FontWeights.Medium,
                    Foreground = (Brush)FindResource("WidgetAccent")
                },
                (_, _) => RestoreIgnoredDevices(),
                restoreLabel);
            restoreBtn.Margin = new Thickness(0, 2, 0, 2);
            panel.Children.Add(restoreBtn);
        }

        var optionsGrid = new Grid { Margin = new Thickness(0, 2, 0, 2) };
        optionsGrid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        optionsGrid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });

        var pollLabel = UiText.Format("WirelessBatteryPollInterval", Math.Clamp(Settings.WirelessBatteryPollSeconds, 15, 300));
        var pollBtn = DockFlyout.FlatButton(
            new TextBlock
            {
                Text = pollLabel,
                FontSize = 10,
                FontWeight = FontWeights.Medium,
                Foreground = (Brush)FindResource("TextSecondary")
            },
            (_, _) => CyclePollInterval(),
            pollLabel);
        pollBtn.Padding = new Thickness(6, 3, 6, 3);
        Grid.SetColumn(pollBtn, 0);
        optionsGrid.Children.Add(pollBtn);

        var thresholdValueText = Settings.WirelessBatteryLowThreshold is 10 or 20 or 30
            ? $"{Settings.WirelessBatteryLowThreshold}%"
            : UiText.Get("WirelessBatteryAlertOff");
        var thresholdLabel = UiText.Format("WirelessBatteryAlertThreshold", thresholdValueText);
        var thresholdBtn = DockFlyout.FlatButton(
            new TextBlock
            {
                Text = thresholdLabel,
                FontSize = 10,
                FontWeight = FontWeights.Medium,
                Foreground = (Brush)FindResource("TextSecondary")
            },
            (_, _) => CycleLowBatteryThreshold(),
            thresholdLabel);
        thresholdBtn.Padding = new Thickness(6, 3, 6, 3);
        Grid.SetColumn(thresholdBtn, 1);
        optionsGrid.Children.Add(thresholdBtn);

        panel.Children.Add(optionsGrid);

        var listenStateText = Settings.WirelessBatteryListenOnly
            ? UiText.Get("WirelessBatteryListenOnlyOn")
            : UiText.Get("WirelessBatteryListenOnlyOff");
        var listenLabel = UiText.Format("WirelessBatteryListenOnly", listenStateText);
        var listenBtn = DockFlyout.FlatButton(
            new TextBlock
            {
                Text = listenLabel,
                FontSize = 10,
                FontWeight = FontWeights.Medium,
                Foreground = Settings.WirelessBatteryListenOnly
                    ? (Brush)FindResource("WidgetAccent")
                    : (Brush)FindResource("TextSecondary")
            },
            (_, _) => ToggleListenOnly(),
            listenLabel);
        listenBtn.Padding = new Thickness(6, 3, 6, 3);
        listenBtn.Margin = new Thickness(0, 1, 0, 0);
        panel.Children.Add(listenBtn);

        var diagLabel = UiText.Get("WirelessBatteryCopyDiagnostics");
        var diagTextBlock = new TextBlock
        {
            Text = diagLabel,
            FontSize = 10.5,
            FontWeight = FontWeights.Medium,
            Foreground = (Brush)FindResource("TextSecondary")
        };
        var diagBtn = DockFlyout.FlatButton(
            diagTextBlock,
            (_, _) =>
            {
                CopyDiagnosticsToClipboard();
                diagTextBlock.Text = UiText.Get("WirelessBatteryDiagnosticsCopied");
            },
            diagLabel);
        diagBtn.Margin = new Thickness(0, 1, 0, 0);
        panel.Children.Add(diagBtn);

        panel.Children.Add(BuildSettingsFooterButton());
    }

    private static Button CreateRowActionButton(string iconResourceKey, string toolTip, RoutedEventHandler onClick)
    {
        var glyph = new ContentControl
        {
            Content = Application.Current?.TryFindResource(iconResourceKey),
            Width = 11,
            Height = 11,
            VerticalAlignment = VerticalAlignment.Center,
            HorizontalAlignment = HorizontalAlignment.Center,
            IsHitTestVisible = false
        };
        glyph.SetResourceReference(StyleProperty, "WidgetIcon");
        glyph.SetResourceReference(ForegroundProperty, "TextSecondary");

        var btn = DockFlyout.FlatButton(glyph, onClick, toolTip);
        btn.Padding = new Thickness(4, 3, 4, 3);
        btn.VerticalAlignment = VerticalAlignment.Center;
        return btn;
    }

    private Button BuildSettingsFooterButton()
    {
        var settingsLabel = UiText.Format("RadioSettings", UiText.Get("Bluetooth"));
        var button = DockFlyout.FlatButton(
            new TextBlock
            {
                Text = settingsLabel,
                FontSize = 11,
                FontWeight = FontWeights.Medium,
                Foreground = (Brush)FindResource("TextSecondary")
            },
            (_, _) => OpenSettingsAction(),
            settingsLabel);
        button.Margin = new Thickness(0, 2, 0, 0);
        return button;
    }

    private static string BuildDeviceSubtitle(BluetoothDeviceBatteryInfo device)
    {
        if (device.IsCharging)
            return UiText.Get("WirelessBatteryCharging");

        if (device.IsStale)
        {
            if (device.SampledAt is { } sampledAt)
            {
                var elapsed = DateTimeOffset.UtcNow - sampledAt;
                var minutes = Math.Max(1, (int)Math.Round(elapsed.TotalMinutes));
                var timeLabel = UiText.Format("MinutesShort", minutes);
                return UiText.Format("WirelessBatteryLastReading", timeLabel);
            }
            return UiText.Format("WirelessBatteryLastReading", "—");
        }

        return device.IsConnected ? UiText.Get("BluetoothConnected") : UiText.Get("BluetoothDisconnected");
    }

    private sealed class BatteryCandidate
    {
        public required string Name { get; set; }
        public required string Id { get; set; }
        public required Guid Container { get; init; }
        public required int Battery { get; set; }
        public bool IsPresent { get; set; }
        public DateTimeOffset LastArrival { get; set; }
        public string? AepId { get; set; }
        public BluetoothDeviceKind Kind { get; set; } = BluetoothDeviceKind.Generic;
        public bool Connected { get; set; }
    }

    /// <summary>
    /// Decides whether a newly enumerated PnP node in the same device container should replace
    /// the previously recorded battery reading. Active (Present=true) nodes always supersede
    /// disconnected ghost nodes carrying stale battery values; ties prefer the latest arrival.
    /// </summary>
    internal static bool ShouldPreferNode(
        bool existingPresent, DateTimeOffset existingArrival,
        bool candidatePresent, DateTimeOffset candidateArrival)
    {
        if (candidatePresent != existingPresent) return candidatePresent;
        return candidateArrival > existingArrival;
    }

    internal static async Task<IReadOnlyList<WirelessDeviceReading>> ReadLiveBluetoothReadingsAsync(CancellationToken cancellationToken = default)
    {
        if (cancellationToken.IsCancellationRequested) return [];
        var list = await ReadLiveBluetoothBatteriesAsync().ConfigureAwait(false);
        var now = DateTimeOffset.UtcNow;
        return list.Select(info => new WirelessDeviceReading(
            Id: info.Id,
            Name: info.Name,
            Kind: info.Kind,
            BatteryLevel: info.BatteryLevel,
            IsConnected: info.IsConnected,
            IsCharging: info.IsCharging,
            IsStale: info.IsStale,
            Source: WirelessBatterySource.Bluetooth,
            SampledAt: info.SampledAt ?? now)).ToList();
    }

    /// <summary>
    /// Reads battery straight from PnP device nodes (~150 ms). Enumerating Bluetooth association endpoints
    /// instead takes ~30 s per protocol on Windows because it starts a fresh discovery every time.
    /// </summary>
    internal static async Task<IReadOnlyList<BluetoothDeviceBatteryInfo>> ReadLiveBluetoothBatteriesAsync()
    {
        DeviceInformationCollection nodes;
        try
        {
            nodes = await DeviceInformation
                .FindAllAsync(string.Empty, NodeProperties, DeviceInformationKind.Device)
                .AsTask().ConfigureAwait(false);
        }
        catch
        {
            return [];
        }

        var byContainer = new Dictionary<Guid, BatteryCandidate>();
        foreach (var node in nodes)
        {
            var battery = ReadPercentage(node.Properties);
            if (battery < 0) continue;
            var container = ReadContainer(node.Properties);
            if (container == Guid.Empty) continue;
            var aepId = node.Properties.TryGetValue(AepIdProperty, out var aepValue) ? aepValue?.ToString() : null;
            var isPresent = ReadIsPresent(node.Properties);
            var lastArrival = ReadLastArrival(node.Properties);

            if (byContainer.TryGetValue(container, out var existing))
            {
                // A headset can leave behind disconnected ghost PnP nodes (Present=false) with stale
                // battery levels alongside the live active node (Present=true) in the same container.
                if (ShouldPreferNode(existing.IsPresent, existing.LastArrival, isPresent, lastArrival))
                {
                    existing.Name = DisplayName(node.Name);
                    existing.Id = node.Id;
                    existing.Battery = battery;
                    existing.IsPresent = isPresent;
                    existing.LastArrival = lastArrival;
                    if (!string.IsNullOrEmpty(aepId)) existing.AepId = aepId;
                }
                else if (existing.AepId is null && !string.IsNullOrEmpty(aepId))
                {
                    existing.AepId = aepId;
                }
                continue;
            }

            byContainer[container] = new BatteryCandidate
            {
                Name = DisplayName(node.Name),
                Id = node.Id,
                Container = container,
                Battery = battery,
                IsPresent = isPresent,
                LastArrival = lastArrival,
                AepId = aepId
            };
        }

        // Direct per-id AEP lookups are fast (~60 ms in parallel) and give the authoritative
        // connection state, category and appearance without the 30 s discovery scan.
        var candidates = byContainer.Values.Where(candidate => !string.IsNullOrEmpty(candidate.AepId)).ToList();
        if (candidates.Count > 0)
        {
            var lookups = await Task.WhenAll(candidates.Select(async candidate =>
            {
                try
                {
                    return await DeviceInformation
                        .CreateFromIdAsync(candidate.AepId!, AepProperties, DeviceInformationKind.AssociationEndpoint)
                        .AsTask().ConfigureAwait(false);
                }
                catch
                {
                    return null;
                }
            })).ConfigureAwait(false);

            for (var i = 0; i < candidates.Count; i++)
            {
                if (lookups[i] is not { } info) continue;
                var candidate = candidates[i];
                candidate.Connected = info.Properties.TryGetValue(ConnectedProperty, out var connected) && connected is true;
                var classified = Classify(ReadCategory(info.Properties), ReadAppearance(info.Properties), candidate.Name);
                if (classified != BluetoothDeviceKind.Generic) candidate.Kind = classified;
                if (!string.IsNullOrWhiteSpace(info.Name)) candidate.Name = info.Name;
                candidate.Id = info.Id;
            }
        }

        var now = DateTimeOffset.UtcNow;
        return candidates
            .Where(candidate => candidate.Connected)
            .Select(candidate => new BluetoothDeviceBatteryInfo(
                Name: candidate.Name,
                Id: candidate.Id,
                IsConnected: true,
                BatteryLevel: candidate.Battery,
                Kind: candidate.Kind,
                SourceLabel: "Bluetooth",
                SampledAt: now))
            .ToList();
    }

    /// <summary>Removes the protocol suffix Windows appends, e.g. "Headset Hands-Free AG" -> "Headset".</summary>
    private static string DisplayName(string name)
    {
        var text = (name ?? string.Empty).Trim();
        foreach (var suffix in new[] { " Hands-Free AG", " Headset", " Audio Gateway", " Stereo", " A2DP" })
        {
            var index = text.IndexOf(suffix, StringComparison.OrdinalIgnoreCase);
            if (index > 0) return text[..index].Trim();
        }
        return text;
    }

    /// <summary>Generic type detection from the node name and Bluetooth service UUID — no model names.</summary>
    internal static BluetoothDeviceKind ClassifyNode(string name, string instanceId)
    {
        var text = name ?? string.Empty;
        bool Has(params string[] words) => words.Any(word => text.Contains(word, StringComparison.OrdinalIgnoreCase));
        if (Has("hands-free", "headset", "headphone", "earbud", "stereo", "audio gateway")) return BluetoothDeviceKind.Headphones;
        if (Has("mouse", "trackball")) return BluetoothDeviceKind.Mouse;
        if (Has("keyboard", "keypad")) return BluetoothDeviceKind.Keyboard;
        if (Has("gamepad", "controller", "joystick")) return BluetoothDeviceKind.Gamepad;
        if (Has("speaker")) return BluetoothDeviceKind.Speaker;
        if (Has("watch")) return BluetoothDeviceKind.Watch;
        if (Has("phone")) return BluetoothDeviceKind.Phone;

        var id = instanceId ?? string.Empty;
        bool HasUuid(params string[] uuids) => uuids.Any(uuid => id.Contains(uuid, StringComparison.OrdinalIgnoreCase));
        // HFP, A2DP and Advanced Audio Distribution are all headset/audio profiles.
        if (HasUuid("0000111e", "0000110b", "0000110d")) return BluetoothDeviceKind.Headphones;
        return BluetoothDeviceKind.Generic;
    }

    /// <summary>Fully generic classification: Windows category first, then SIG appearance, then name words.</summary>
    private static BluetoothDeviceKind Classify(string? category, ushort? appearance, string name)
    {
        if (!string.IsNullOrEmpty(category))
        {
            var segments = category.Split('.', StringSplitOptions.RemoveEmptyEntries);
            bool HasCategory(params string[] tokens) => segments.Any(segment =>
                tokens.Any(token => segment.Contains(token, StringComparison.OrdinalIgnoreCase)));

            if (HasCategory("headset", "headphone", "earbud", "audio")) return BluetoothDeviceKind.Headphones;
            if (HasCategory("mouse", "trackball")) return BluetoothDeviceKind.Mouse;
            if (HasCategory("keyboard", "keypad")) return BluetoothDeviceKind.Keyboard;
            if (HasCategory("gamepad", "gaming", "joystick")) return BluetoothDeviceKind.Gamepad;
            if (HasCategory("speaker")) return BluetoothDeviceKind.Speaker;
            if (HasCategory("watch", "wearable")) return BluetoothDeviceKind.Watch;
            if (HasCategory("phone")) return BluetoothDeviceKind.Phone;
        }

        switch (appearance)
        {
            case 961: return BluetoothDeviceKind.Keyboard;
            case 962: return BluetoothDeviceKind.Mouse;
            case 963 or 964: return BluetoothDeviceKind.Gamepad;
            case 192 or 193: return BluetoothDeviceKind.Watch;
            case 64 or 65: return BluetoothDeviceKind.Phone;
        }

        var text = name ?? string.Empty;
        bool Has(params string[] words) => words.Any(word => text.Contains(word, StringComparison.OrdinalIgnoreCase));
        if (Has("headphone", "headset", "earbud", "kulaklık", "kulaklik")) return BluetoothDeviceKind.Headphones;
        if (Has("mouse", "fare")) return BluetoothDeviceKind.Mouse;
        if (Has("keyboard", "klavye")) return BluetoothDeviceKind.Keyboard;
        return BluetoothDeviceKind.Generic;
    }

    private static string? ReadCategory(IReadOnlyDictionary<string, object> properties)
    {
        if (!properties.TryGetValue(CategoryProperty, out var value) || value is null) return null;
        return value switch
        {
            string text => text,
            string[] array => string.Join('.', array),
            _ => value.ToString()
        };
    }

    private static ushort? ReadAppearance(IReadOnlyDictionary<string, object> properties)
    {
        if (!properties.TryGetValue(AppearanceProperty, out var value)) return null;
        if (value is ushort shortValue) return shortValue;
        if (value is int intValue) return (ushort)intValue;
        return null;
    }

    private static Guid ReadContainer(IReadOnlyDictionary<string, object> properties)
    {
        if (!properties.TryGetValue(ContainerProperty, out var value) || value is null) return Guid.Empty;
        if (value is Guid guid) return guid;
        return Guid.TryParse(value.ToString(), out var parsed) ? parsed : Guid.Empty;
    }

    private static bool ReadIsPresent(IReadOnlyDictionary<string, object> properties)
    {
        if (properties.TryGetValue(PresentProperty, out var present) && present is bool isPresent) return isPresent;
        if (properties.TryGetValue(DevPkeyIsPresentProperty, out var pkeyPresent) && pkeyPresent is bool isPkeyPresent) return isPkeyPresent;
        return false;
    }

    private static DateTimeOffset ReadLastArrival(IReadOnlyDictionary<string, object> properties)
    {
        if (properties.TryGetValue(LastArrivalDateProperty, out var value))
        {
            if (value is DateTimeOffset dto) return dto;
            if (value is DateTime dt) return new DateTimeOffset(dt);
        }
        return DateTimeOffset.MinValue;
    }

    private static int ReadPercentage(IReadOnlyDictionary<string, object> properties)
    {
        if (properties.TryGetValue(BatteryLevelProperty, out var raw))
        {
            var level = AsPercentage(raw);
            if (level >= 0) return level;
        }
        if (properties.TryGetValue(BatteryLifeProperty, out var life))
        {
            var level = AsPercentage(life);
            if (level >= 0) return level;
        }
        return -1;
    }

    private static int AsPercentage(object? value)
    {
        if (value is null) return -1;
        try
        {
            var level = Convert.ToInt32(value);
            return level is >= 0 and <= 100 ? level : -1;
        }
        catch
        {
            return -1;
        }
    }

    private static void DefaultSetClipboardText(string text)
    {
        try
        {
            Clipboard.SetText(text ?? string.Empty);
        }
        catch
        {
        }
    }

    private static void DefaultOpenBluetoothSettings()
    {
        DockFlyout.CloseAll();
        try
        {
            Process.Start(new ProcessStartInfo("ms-settings:bluetooth") { UseShellExecute = true });
        }
        catch
        {
        }
    }
}
