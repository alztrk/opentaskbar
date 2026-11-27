using System.IO;
using System.Runtime.InteropServices;
using System.Security;
using System.Windows;
using System.Windows.Automation;
using System.Windows.Controls;
using System.Windows.Controls.Primitives;
using System.Windows.Data;
using System.Windows.Media;
using System.Windows.Threading;
using OpenTaskbar.Models;
using OpenTaskbar.Services;
using Windows.Devices.Radios;

namespace OpenTaskbar.Widgets;

public partial class QuickSettingsWidget : DockWidget
{
    private enum Setting { Wifi, Bluetooth, NightLight }
    private enum ChangeResult { Applied, Unavailable, Denied, Failed }

    private static readonly IReadOnlyDictionary<Setting, string> SettingNames = new Dictionary<Setting, string>
    {
        [Setting.Wifi] = "WiFi",
        [Setting.Bluetooth] = "Bluetooth",
        [Setting.NightLight] = "NightLight"
    };

    private const double ButtonSize = 27;
    private const double FullGap = 4;
    private const double CompactGap = 2;
    private const double FullPadding = 8;
    private const double CompactPadding = 4;

    // Primary and secondary docks share the same physical controls.
    private static readonly Dictionary<Setting, bool> PendingTargets = [];
    private static event Action? TransitionChanged;
    private static Task<RadioAccessStatus>? _radioAccess;

    private readonly Dictionary<Setting, ChangeResult> _errors = [];
    private readonly DockFlyout _flyout = new();
    private IReadOnlyList<Radio> _radios = [];
    private CancellationTokenSource? _activation;
    private bool? _nightLightOn;
    private bool _loadingRadios;
    private int _radioDiscoveryAttempts;

    internal Func<bool?> ReadNightLight { get; init; } = NightLightControl.Read;
    internal Func<bool, bool> WriteNightLight { get; init; } = NightLightControl.Set;

    public override bool SupportsCompactMode => true;
    public override int ShrinkPriority => 20;
    public override double FullWidthEstimate => EstimateWidth(FullGap, FullPadding);
    public override double CompactWidthEstimate => EstimateWidth(CompactGap, CompactPadding);

    public QuickSettingsWidget()
    {
        InitializeComponent();
        AddShortcuts();
        ApplyDensityLayout();
        Render();
    }

    protected override void OnCompactChanged(bool compact) => ApplyDensityLayout();
    protected override void OnHoverExpandedChanged(bool expanded) => ApplyDensityLayout();
    protected override void OnSettingsChanged()
    {
        var orientation = Settings.Edge == DockEdge.Bottom ? Orientation.Horizontal : Orientation.Vertical;
        if (ControlsStrip.Orientation == orientation) return;
        ControlsStrip.Orientation = orientation;
        ApplyDensityLayout();
    }

    private double EstimateWidth(double gap, double padding)
    {
        var buttonCount = ControlsStrip.Children.Count;
        var contentWidth = Settings.Edge == DockEdge.Bottom
            ? ButtonSize * buttonCount + WifiPill.BorderThickness.Left * 2 * SettingNames.Count + gap * (buttonCount - 1)
            : ButtonSize + WifiPill.BorderThickness.Left * 2;
        return contentWidth + (Settings.Edge == DockEdge.Bottom ? 2 * padding : 0);
    }

    private void ApplyDensityLayout()
    {
        var compact = IsCompact && !IsHoverExpanded;
        var gap = compact ? CompactGap : FullGap;
        var padding = compact ? CompactPadding : FullPadding;
        var horizontal = ControlsStrip.Orientation == Orientation.Horizontal;
        Card.Padding = horizontal ? new Thickness(padding, 0, padding, 0) : new Thickness(0, padding, 0, padding);
        if (horizontal) Card.ClearValue(HeightProperty);
        else Card.Height = double.NaN;

        for (var i = 0; i < ControlsStrip.Children.Count; i++)
        {
            if (ControlsStrip.Children[i] is FrameworkElement item)
            {
                var trailingGap = i == ControlsStrip.Children.Count - 1 ? 0 : gap;
                item.Margin = horizontal ? new Thickness(0, 0, trailingGap, 0) : new Thickness(0, 0, 0, trailingGap);
                item.HorizontalAlignment = horizontal ? HorizontalAlignment.Stretch : HorizontalAlignment.Center;
            }
        }
        RaiseContentWidthChanged();
    }

    protected override void OnActivated()
    {
        _activation = new CancellationTokenSource();
        TransitionChanged += RefreshState;
        _radioDiscoveryAttempts = 0;
        RefreshState();
        _ = DiscoverRadiosAsync(_activation.Token);
    }

    private async Task DiscoverRadiosAsync(CancellationToken token)
    {
        if (_loadingRadios) return;
        _loadingRadios = true;
        _radioDiscoveryAttempts++;
        Render();
        try
        {
            using var timeout = CancellationTokenSource.CreateLinkedTokenSource(token);
            timeout.CancelAfter(TimeSpan.FromSeconds(8));
            // WinRT requires RequestAccessAsync before GetRadiosAsync on unpackaged desktop apps so Bluetooth
            // and Wi-Fi radios are enumerated and controllable rather than reported unavailable (#161).
            var access = await EnsureRadioAccessAsync(timeout.Token);
            if (token.IsCancellationRequested) return;
            if (access != RadioAccessStatus.Allowed)
            {
                AppLog.Warning($"Quick settings radio access was {access}.");
            }

            var radios = await Radio.GetRadiosAsync().AsTask(timeout.Token);
            if (token.IsCancellationRequested) return;

            foreach (var oldRadio in _radios)
                oldRadio.StateChanged -= Radio_StateChanged;
            _radios = radios.Where(radio => radio.Kind is RadioKind.WiFi or RadioKind.Bluetooth).ToArray();
            foreach (var radio in _radios)
                radio.StateChanged += Radio_StateChanged;
        }
        catch (OperationCanceledException) when (token.IsCancellationRequested) { }
        catch (Exception ex) when (ex is COMException or UnauthorizedAccessException or InvalidOperationException
            or OperationCanceledException)
        {
            AppLog.Warning($"Quick settings radio discovery failed ({ex.GetType().Name}): {ex.Message}");
        }
        finally
        {
            if (!token.IsCancellationRequested)
            {
                _loadingRadios = false;
                Render();
            }
        }
    }

    protected override void OnDeactivated()
    {
        _flyout.Close();
        _activation?.Cancel();
        _activation?.Dispose();
        _activation = null;
        TransitionChanged -= RefreshState;
        foreach (var radio in _radios)
            radio.StateChanged -= Radio_StateChanged;
        _radios = [];
        _nightLightOn = null;
        _loadingRadios = false;
        _radioDiscoveryAttempts = 0;
        _errors.Clear();
        Render();
    }

    protected override void OnTick(DateTime now)
    {
        if (now.Second % 5 == 0) RefreshState();
        // Retry radio discovery a few times if Bluetooth/Wi-Fi adapters were still initializing at boot.
        if (now.Second % 15 == 0 && !_loadingRadios && _activation is { IsCancellationRequested: false } activation
            && _radioDiscoveryAttempts < 4
            && (RadiosFor(Setting.Bluetooth).Length == 0 || RadiosFor(Setting.Wifi).Length == 0))
        {
            _ = DiscoverRadiosAsync(activation.Token);
        }
    }

    protected override void OnLanguageChanged() => Render();

    private void Radio_StateChanged(Radio sender, object args) =>
        Dispatcher.BeginInvoke(DispatcherPriority.Background, () =>
        {
            if (IsActive && _radios.Contains(sender)) Render();
        });

    private void RefreshState()
    {
        try
        {
            _nightLightOn = ReadNightLight();
        }
        catch (Exception ex) when (ex is UnauthorizedAccessException or IOException or SecurityException)
        {
            _nightLightOn = null;
        }
        Render();
    }

    private Radio[] RadiosFor(Setting setting) =>
        _radios.Where(radio => radio.Kind == (setting == Setting.Wifi ? RadioKind.WiFi : RadioKind.Bluetooth)).ToArray();

    private static bool? RadioIsOn(IReadOnlyList<Radio> radios)
    {
        if (radios.Any(radio => radio.State == RadioState.On)) return true;
        return radios.Any(radio => radio.State == RadioState.Off) ? false : null;
    }

    private void Render()
    {
        RenderSetting(Setting.Wifi, WifiToggle, WifiPill, RadioIsOn(RadiosFor(Setting.Wifi)));
        RenderSetting(Setting.Bluetooth, BluetoothToggle, BluetoothPill, RadioIsOn(RadiosFor(Setting.Bluetooth)));
        RenderSetting(Setting.NightLight, NightLightToggle, NightLightPill, _nightLightOn);
        ControlsStrip.IsEnabled = IsActive;
    }

    private void RenderSetting(Setting setting, ToggleButton button, Border pill, bool? on)
    {
        var pending = PendingTargets.TryGetValue(setting, out var target);
        var label = UiText.Get(SettingNames[setting]);
        var stateKey = pending ? (target ? "QuickTurningOn" : "QuickTurningOff")
            : _loadingRadios && setting != Setting.NightLight ? "QuickLoading"
            : on switch { true => "QuickOn", false => "QuickOff", null => "QuickUnavailable" };
        var state = UiText.Get(stateKey);
        if (!pending && _errors.TryGetValue(setting, out var error))
            state += " · " + UiText.Get(ErrorKey(error));

        var enabled = IsActive && !pending && on.HasValue;
        button.IsChecked = on == true;
        button.IsEnabled = enabled;
        button.ToolTip = $"{label} · {state}";
        ToolTipService.SetShowOnDisabled(button, true);
        AutomationProperties.SetName(button, label);
        AutomationProperties.SetItemStatus(button, state);
        ApplyChipVisual(pill, button, on == true);
    }

    private static void ApplyChipVisual(Border pill, ToggleButton button, bool on)
    {
        if (on)
        {
            pill.SetResourceReference(Border.BackgroundProperty, "WidgetSelectedFill");
            pill.SetResourceReference(Border.BorderBrushProperty, "WidgetSelectedStroke");
            button.SetResourceReference(ForegroundProperty, "WidgetSelectedForeground");
        }
        else
        {
            pill.Background = Brushes.Transparent;
            pill.BorderBrush = Brushes.Transparent;
            button.SetResourceReference(ForegroundProperty, "TextSecondary");
        }
    }

    private void WifiToggle_Click(object sender, RoutedEventArgs e) => Toggle(Setting.Wifi);
    private void BluetoothToggle_Click(object sender, RoutedEventArgs e) => Toggle(Setting.Bluetooth);
    private void NightLightToggle_Click(object sender, RoutedEventArgs e) => Toggle(Setting.NightLight);

    private async void Toggle(Setting setting)
    {
        var activation = _activation;
        if (activation is null) return;
        if (PendingTargets.ContainsKey(setting))
        {
            Render();
            return;
        }

        if (setting == Setting.NightLight) RefreshState();
        var radios = setting == Setting.NightLight ? [] : RadiosFor(setting);
        var on = setting == Setting.NightLight ? _nightLightOn : RadioIsOn(radios);
        if (!on.HasValue)
        {
            Render();
            return;
        }

        var target = !on.Value;
        PendingTargets.Add(setting, target);
        var token = activation.Token;
        var result = ChangeResult.Failed;
        _errors.Remove(setting);
        try
        {
            TransitionChanged?.Invoke();
            using var timeout = CancellationTokenSource.CreateLinkedTokenSource(token);
            timeout.CancelAfter(TimeSpan.FromSeconds(8));
            result = setting == Setting.NightLight
                ? await SetNightLightAsync(target, timeout.Token)
                : await SetRadiosAsync(radios, target, timeout.Token);
        }
        catch (OperationCanceledException) when (token.IsCancellationRequested) { }
        catch (Exception ex) when (ex is COMException or UnauthorizedAccessException or InvalidOperationException
            or IOException or SecurityException or OperationCanceledException)
        {
            AppLog.Warning($"Quick settings {setting} change failed ({ex.GetType().Name}).");
            if (ex is UnauthorizedAccessException or SecurityException) result = ChangeResult.Denied;
        }
        finally
        {
            PendingTargets.Remove(setting);
            if (!token.IsCancellationRequested && result != ChangeResult.Applied)
                _errors[setting] = result;
            TransitionChanged?.Invoke();
        }

        if (!token.IsCancellationRequested && result != ChangeResult.Applied)
            Report(ErrorKey(result));
    }

    private static string ErrorKey(ChangeResult result) => result switch
    {
        ChangeResult.Denied => "QuickPermissionDenied",
        ChangeResult.Unavailable => "QuickUnavailable",
        _ => "QuickChangeFailed"
    };

    private static async Task<RadioAccessStatus> EnsureRadioAccessAsync(CancellationToken token)
    {
        // Share permission discovery while Wi-Fi and Bluetooth requests overlap. A denial is not cached.
        var pendingAccess = _radioAccess ??= RequestRadioAccessAsync();
        RadioAccessStatus access;
        try
        {
            access = await pendingAccess.WaitAsync(token);
        }
        finally
        {
            if (ReferenceEquals(_radioAccess, pendingAccess) && !pendingAccess.IsCompletedSuccessfully)
                _radioAccess = null;
        }

        if (access != RadioAccessStatus.Allowed && ReferenceEquals(_radioAccess, pendingAccess))
            _radioAccess = null;
        return access;
    }

    private static async Task<ChangeResult> SetRadiosAsync(Radio[] radios, bool on, CancellationToken token)
    {
        var access = await EnsureRadioAccessAsync(token);
        if (access != RadioAccessStatus.Allowed)
            return ChangeResult.Denied;
        token.ThrowIfCancellationRequested();

        var target = on ? RadioState.On : RadioState.Off;
        var controllable = radios.Where(radio => radio.State is RadioState.On or RadioState.Off).ToArray();
        if (controllable.Length == 0) return ChangeResult.Unavailable;

        foreach (var radio in controllable)
        {
            if (radio.State == target) continue;
            if (await radio.SetStateAsync(target).AsTask(token) != RadioAccessStatus.Allowed)
                return ChangeResult.Denied;
        }

        // Allowed is permission to change the radio. Drivers may apply it later or override it.
        bool Confirmed() => controllable.All(radio =>
        {
            var state = radio.State;
            return state == target || (!on && state == RadioState.Disabled);
        });
        while (!Confirmed())
            await Task.Delay(100, token);
        return ChangeResult.Applied;
    }

    private static async Task<RadioAccessStatus> RequestRadioAccessAsync()
    {
        using var timeout = new CancellationTokenSource(TimeSpan.FromSeconds(8));
        return await Radio.RequestAccessAsync().AsTask(timeout.Token);
    }

    private async Task<ChangeResult> SetNightLightAsync(bool on, CancellationToken token)
    {
        var written = await Task.Run(() => WriteNightLight(on), token);
        if (!written) return ChangeResult.Unavailable;

        // Give the shell a chance to reconcile CloudStore before checking the persisted state.
        await Task.Delay(300, token);
        return ReadNightLight() == on ? ChangeResult.Applied : ChangeResult.Failed;
    }

    private sealed record ShortcutSpec(string LabelKey, string IconKey, Func<FrameworkElement, string?> Action);

    private static readonly ShortcutSpec[] InlineShortcuts =
    [
        new("QuickAvailableNetworks", "IconSearch", ShellShortcuts.OpenAvailableNetworks),
        new("QuickCast", "IconCast", _ => ShellShortcuts.OpenCast()),
        new("QuickOpenWindowsPanel", "IconSliders", ShellShortcuts.OpenQuickSettings)
    ];

    private static readonly ShortcutSpec[] FlyoutShortcuts =
    [
        new("QuickBluetoothSettings", "IconBluetooth", _ => ShellShortcuts.OpenUri("ms-settings:bluetooth")),
        new("QuickNightLightSettings", "IconMoon", _ => ShellShortcuts.OpenUri("ms-settings:nightlight")),
        new("QuickAirplaneMode", "IconAirplane", _ => ShellShortcuts.OpenUri("ms-settings:network-airplanemode")),
        new("QuickCellular", "IconCellular", _ => ShellShortcuts.OpenUri("ms-settings:network-cellular")),
        new("QuickBrightness", "IconSun", _ => ShellShortcuts.OpenUri("ms-settings:display")),
        new("QuickRotationLock", "IconRotate", _ => ShellShortcuts.OpenUri("ms-settings:screenrotation")),
        new("QuickEnergySaver", "IconBattery", _ => ShellShortcuts.OpenUri("ms-settings:batterysaver")),
        new("QuickNearbySharing", "IconLink", _ => ShellShortcuts.OpenUri("ms-settings:crossdevice")),
        new("QuickLiveCaptions", "IconCaptions", _ => ShellShortcuts.OpenUri("ms-settings:easeofaccess-closedcaptioning")),
        new("QuickNotificationMode", "IconBellOff", _ => ShellShortcuts.OpenUri("ms-settings:quiethours")),
        new("QuickStudioEffects", "IconMonitor", ShellShortcuts.OpenQuickSettings),
        new("QuickHearingDevices", "IconHeadphones", _ => ShellShortcuts.OpenUri("ms-settings:easeofaccess-hearingaids")),
        new("QuickMobileHotspot", "IconHotspot", _ => ShellShortcuts.OpenUri("ms-settings:network-mobilehotspot")),
        new("QuickAccessibility", "IconAccessibility", _ => ShellShortcuts.OpenUri("ms-settings:easeofaccess"))
    ];

    private void AddShortcuts()
    {
        foreach (var shortcut in InlineShortcuts)
            AddShortcut(shortcut.LabelKey, shortcut.IconKey, shortcut.Action);

        var moreIcon = new ContentControl();
        moreIcon.SetResourceReference(StyleProperty, "WidgetIcon");
        moreIcon.SetResourceReference(ContentControl.ContentProperty, "IconChevronUp");
        var more = new Button
        {
            Name = "MoreButton",
            Content = moreIcon,
            Width = ButtonSize,
            Height = ButtonSize
        };
        more.SetResourceReference(StyleProperty, "WidgetIconButton");
        var moreLabel = new Binding("[WidgetQuickSettings]") { Source = UiText.Current };
        more.SetBinding(ToolTipProperty, moreLabel);
        more.SetBinding(AutomationProperties.NameProperty, moreLabel);
        more.Click += (_, _) => _flyout.Toggle(more, BuildMoreFlyoutContent);
        ControlsStrip.Children.Add(more);
    }

    internal UIElement BuildMoreFlyoutContent()
    {
        var root = new StackPanel { Width = 320 };
        root.Children.Add(DockFlyout.Header(UiText.Get("WidgetQuickSettings"), bottomMargin: 8));

        var grid = new UniformGrid { Columns = 2 };
        foreach (var shortcut in FlyoutShortcuts)
        {
            var icon = new ContentControl
            {
                Width = 14,
                Height = 14,
                Margin = new Thickness(0, 0, 8, 0),
                VerticalAlignment = VerticalAlignment.Center
            };
            icon.SetResourceReference(StyleProperty, "WidgetIcon");
            icon.SetResourceReference(ContentControl.ContentProperty, shortcut.IconKey);
            icon.SetResourceReference(ForegroundProperty, "TextSecondary");

            var labelText = UiText.Get(shortcut.LabelKey);
            var text = new TextBlock
            {
                Text = labelText,
                FontSize = 12,
                VerticalAlignment = VerticalAlignment.Center,
                TextTrimming = TextTrimming.CharacterEllipsis
            };
            text.SetResourceReference(TextBlock.ForegroundProperty, "TextPrimary");

            var row = new DockPanel();
            DockPanel.SetDock(icon, Dock.Left);
            row.Children.Add(icon);
            row.Children.Add(text);

            var action = shortcut.Action;
            var item = DockFlyout.FlatButton(row, (s, _) =>
            {
                _flyout.Close();
                if (s is FrameworkElement el) Report(action(el));
            }, labelText);
            item.Padding = new Thickness(8, 6, 8, 6);
            item.Margin = new Thickness(1);
            grid.Children.Add(item);
        }

        root.Children.Add(grid);
        return root;
    }

    public override UIElement? BuildPreviewFlyoutContent() => BuildMoreFlyoutContent();

    private void AddShortcut(string labelKey, string iconKey, Func<FrameworkElement, string?> action)
    {
        var icon = new ContentControl();
        icon.SetResourceReference(StyleProperty, "WidgetIcon");
        icon.SetResourceReference(ContentControl.ContentProperty, iconKey);

        var button = new Button { Content = icon, Width = ButtonSize, Height = ButtonSize };
        button.SetResourceReference(StyleProperty, "WidgetIconButton");
        var label = new Binding($"[{labelKey}]") { Source = UiText.Current };
        button.SetBinding(ToolTipProperty, label);
        button.SetBinding(AutomationProperties.NameProperty, label);
        button.Click += (_, _) => Report(action(button));
        ControlsStrip.Children.Add(button);
    }
}
