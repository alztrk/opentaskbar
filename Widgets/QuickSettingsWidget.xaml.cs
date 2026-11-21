using System.ComponentModel;
using System.Runtime.InteropServices;
using System.Windows;
using System.Windows.Automation;
using System.Windows.Controls;
using System.Windows.Controls.Primitives;
using System.Windows.Threading;
using OpenTaskbar.Models;
using OpenTaskbar.Services;
using Windows.Devices.Bluetooth;
using Windows.Devices.Enumeration;
using Windows.Devices.Radios;

namespace OpenTaskbar.Widgets;

/// <summary>
/// Wi-Fi and Bluetooth toggles plus shortcuts to Windows' own flyouts. These replace the tray area that disappears
/// with the native taskbar. When Windows denies radio access, the toggles fall back to opening the settings page.
/// </summary>
public partial class QuickSettingsWidget : DockWidget
{
    private readonly record struct RadioRenderState(bool Visible, bool On, bool HasRadio, AppLanguage Language);

    private Radio? _wifi;
    private Radio? _bluetooth;
    private bool _radioAccess;
    private int _activationVersion;
    private int _bluetoothRequestVersion;
    private readonly HashSet<Radio> _pendingRadioChanges = [];
    private RadioRenderState? _lastWifiState;
    private RadioRenderState? _lastBluetoothState;
    private readonly TwoStepConfirm _confirm;
    private readonly DockFlyout _detailFlyout = new();

    public override bool SupportsCompactMode => true;
    public override int ShrinkPriority => 20;
    public override double FullWidthEstimate => 282;
    public override double CompactWidthEstimate => 98;

    public QuickSettingsWidget()
    {
        InitializeComponent();
        _confirm = new TwoStepConfirm(Dispatcher);
        Render();
    }

    protected override void OnCompactChanged(bool compact) => ApplyDensityLayout();
    protected override void OnHoverExpandedChanged(bool expanded) => ApplyDensityLayout();

    private void ApplyDensityLayout()
    {
        var compact = IsCompact && !IsHoverExpanded;
        var extraVis = compact ? Visibility.Collapsed : Visibility.Visible;
        WifiSettingsDivider.Visibility = extraVis;
        WifiSettings.Visibility = extraVis;
        DisplayButton.Visibility = extraVis;
        BluetoothSettingsDivider.Visibility = extraVis;
        BluetoothSettings.Visibility = extraVis;
        HotspotButton.Visibility = extraVis;
        ActionsDivider.Visibility = extraVis;
        NotificationsButton.Visibility = extraVis;
    }

    public override UIElement? BuildHoverFlyoutContent()
    {
        var root = new StackPanel { Width = 248 };
        root.Children.Add(DockFlyout.Heading(UiText.Get("QuickSettingsPanel")));

        var row1 = new WrapPanel { Margin = new Thickness(0, 2, 0, 4) };
        if (_wifi is not null || !_radioAccess)
        {
            row1.Children.Add(MakeActionChip("IconWifi", UiText.Get("WiFi"), () =>
                ConfirmAndToggle(_wifi, WifiPill, WifiToggle, "WiFi")));
            row1.Children.Add(MakeActionChip("IconChevronRight", UiText.Format("RadioSettings", UiText.Get("WiFi")),
                () => OpenWifiDetails(this)));
        }
        if (_bluetooth is not null || !_radioAccess)
        {
            row1.Children.Add(MakeActionChip("IconBluetooth", UiText.Get("Bluetooth"), () =>
                ConfirmAndToggle(_bluetooth, BluetoothPill, BluetoothToggle, "Bluetooth")));
            row1.Children.Add(MakeActionChip("IconChevronRight", UiText.Format("RadioSettings", UiText.Get("Bluetooth")),
                () => _ = OpenBluetoothDetailsAsync(this)));
        }
        row1.Children.Add(MakeActionChip("IconCast", UiText.Get("Display"), () => Report(ShellShortcuts.OpenProjection(this))));
        row1.Children.Add(MakeActionChip("IconHotspot", UiText.Get("Hotspot"), () => Report(ShellShortcuts.OpenQuickSettings(this))));
        row1.Children.Add(MakeActionChip("IconSettings", UiText.Get("QuickSettingsPanel"), () => Report(ShellShortcuts.OpenQuickSettings(this))));
        row1.Children.Add(MakeActionChip("IconBell", UiText.Get("NotificationCenter"), () => Report(ShellShortcuts.OpenNotificationCenter())));

        root.Children.Add(row1);
        return root;

        Button MakeActionChip(string iconKey, string label, Action onClick)
        {
            var icon = new ContentControl { Content = FindResource(iconKey), Width = 14, Height = 14, Margin = new Thickness(0, 0, 6, 0) };
            icon.SetResourceReference(StyleProperty, "WidgetIcon");
            var text = new TextBlock { Text = label, VerticalAlignment = VerticalAlignment.Center, FontSize = 11.5 };
            text.SetResourceReference(StyleProperty, "WidgetText");
            var inner = new StackPanel { Orientation = Orientation.Horizontal };
            inner.Children.Add(icon);
            inner.Children.Add(text);
            var btn = new Button
            {
                Content = inner,
                Height = 30,
                Width = double.NaN,
                Padding = new Thickness(8, 0, 8, 0),
                Margin = new Thickness(0, 3, 6, 3),
                ToolTip = label
            };
            btn.SetResourceReference(StyleProperty, "WidgetIconButton");
            btn.Width = double.NaN;
            btn.Click += (_, _) => onClick();
            return btn;
        }
    }

    protected override async void OnActivated()
    {
        var activationVersion = ++_activationVersion;
        UiText.Current.PropertyChanged += UiText_PropertyChanged;
        _lastWifiState = null;
        _lastBluetoothState = null;

        try
        {
            var access = await Radio.RequestAccessAsync() == RadioAccessStatus.Allowed;
            if (!IsActive || activationVersion != _activationVersion) return;
            _radioAccess = access;
            if (!_radioAccess)
            {
                RenderForActivation(activationVersion);
                return;
            }

            var radios = await Radio.GetRadiosAsync();
            if (!IsActive || activationVersion != _activationVersion) return;
            _wifi = radios.FirstOrDefault(radio => radio.Kind == RadioKind.WiFi);
            _bluetooth = radios.FirstOrDefault(radio => radio.Kind == RadioKind.Bluetooth);
            if (_wifi is not null) _wifi.StateChanged += Radio_StateChanged;
            if (_bluetooth is not null) _bluetooth.StateChanged += Radio_StateChanged;
        }
        catch (Exception ex) when (ex is COMException or UnauthorizedAccessException)
        {
            if (activationVersion != _activationVersion) return;
            AppLog.Warning($"Radio access unavailable: {ex.Message}");
            _radioAccess = false;
        }

        RenderForActivation(activationVersion);
    }

    private void RenderForActivation(int activationVersion)
    {
        if (Dispatcher.CheckAccess())
        {
            if (IsActive && activationVersion == _activationVersion) Render();
            return;
        }
        Dispatcher.BeginInvoke(DispatcherPriority.Normal, () =>
        {
            if (IsActive && activationVersion == _activationVersion) Render();
        });
    }

    protected override void OnDeactivated()
    {
        _confirm.Disarm();
        _detailFlyout.Close();
        _activationVersion++;
        _bluetoothRequestVersion++;
        _pendingRadioChanges.Clear();
        UiText.Current.PropertyChanged -= UiText_PropertyChanged;
        if (_wifi is not null) _wifi.StateChanged -= Radio_StateChanged;
        if (_bluetooth is not null) _bluetooth.StateChanged -= Radio_StateChanged;
        _wifi = null;
        _bluetooth = null;
        _lastWifiState = null;
        _lastBluetoothState = null;
    }

    private void UiText_PropertyChanged(object? sender, PropertyChangedEventArgs e) => Render();

    private void Radio_StateChanged(Radio sender, object args) =>
        Dispatcher.BeginInvoke(DispatcherPriority.Background, () =>
        {
            if (IsActive) Render();
        });

    private void Render()
    {
        RenderRadio(_wifi, WifiPill, WifiToggle, WifiSettings, "WiFi", ref _lastWifiState);
        RenderRadio(_bluetooth, BluetoothPill, BluetoothToggle, BluetoothSettings, "Bluetooth", ref _lastBluetoothState);
    }

    private void RenderRadio(Radio? radio, Border pill, ToggleButton toggle, Button settings, string nameKey, ref RadioRenderState? lastState)
    {
        var visible = !(_radioAccess && radio is null && IsActive);
        var on = radio?.State == RadioState.On;
        var hasRadio = radio is not null;
        var lang = UiText.Current.Language;
        var nextState = new RadioRenderState(visible, on, hasRadio, lang);
        if (lastState == nextState) return;
        lastState = nextState;

        pill.Visibility = visible ? Visibility.Visible : Visibility.Collapsed;
        toggle.IsChecked = on;
        pill.SetResourceReference(Border.BackgroundProperty, on ? "SurfaceSelectedStrong" : "SurfaceSelected");
        pill.SetResourceReference(Border.BorderBrushProperty, on ? "WidgetOnStroke" : "StrokeSubtle");
        toggle.SetResourceReference(ForegroundProperty, on ? "WidgetAccent" : "TextPrimary");

        var name = UiText.Get(nameKey);
        var toggleLabel = hasRadio ? UiText.Format("RadioToggle", name) : UiText.Format("RadioSettings", name);
        toggle.ToolTip = toggleLabel;
        AutomationProperties.SetName(toggle, toggleLabel);
        var settingsLabel = UiText.Format("RadioSettings", name);
        settings.ToolTip = settingsLabel;
        AutomationProperties.SetName(settings, settingsLabel);
    }

    private async void ToggleRadio(Radio? radio)
    {
        if (radio is null)
        {
            Report(ShellShortcuts.OpenQuickSettings(this));
            Render();
            return;
        }

        if (!_pendingRadioChanges.Add(radio)) return;
        var activationVersion = _activationVersion;
        try
        {
            var target = radio.State == RadioState.On ? RadioState.Off : RadioState.On;
            var result = await radio.SetStateAsync(target);
            if (!IsActive || activationVersion != _activationVersion) return;
            if (result != RadioAccessStatus.Allowed)
            {
                AppLog.Warning($"Radio {radio.Kind} state change denied: {result}");
                Report(ShellShortcuts.OpenQuickSettings(this));
            }
        }
        catch (Exception ex) when (ex is COMException or UnauthorizedAccessException or InvalidOperationException)
        {
            AppLog.Warning($"Radio {radio.Kind} state change failed: {ex.Message}");
            if (IsActive && activationVersion == _activationVersion)
                Report(ShellShortcuts.OpenQuickSettings(this));
        }
        finally
        {
            _pendingRadioChanges.Remove(radio);
        }

        if (IsActive && activationVersion == _activationVersion) Render();
    }

    private void WifiToggle_Click(object sender, RoutedEventArgs e) =>
        ConfirmAndToggle(_wifi, WifiPill, WifiToggle, "WiFi");

    private void BluetoothToggle_Click(object sender, RoutedEventArgs e) =>
        ConfirmAndToggle(_bluetooth, BluetoothPill, BluetoothToggle, "Bluetooth");

    private void ConfirmAndToggle(Radio? radio, Border pill, ToggleButton toggle, string nameKey)
    {
        if (radio is null)
        {
            _confirm.Disarm();
            Report(ShellShortcuts.OpenQuickSettings(this));
            Render();
            return;
        }

        // Keep the visual checked state reflecting the real hardware state until confirmed.
        toggle.IsChecked = radio.State == RadioState.On;
        if (!_confirm.ConsumeOrArm(
            pill,
            onArm: () =>
            {
                pill.BorderThickness = new Thickness(2);
                pill.SetResourceReference(Border.BorderBrushProperty, "AccentBrush");
                var confirmText = UiText.Format("RadioConfirmToggle", UiText.Get(nameKey));
                toggle.ToolTip = confirmText;
                AutomationProperties.SetName(toggle, confirmText);
            },
            onDisarm: () =>
            {
                pill.BorderThickness = new Thickness(1.5);
                if (pill == WifiPill) _lastWifiState = null;
                else _lastBluetoothState = null;
                if (IsActive) Render();
            }))
        {
            return;
        }

        ToggleRadio(radio);
    }

    private void OpenWifiDetails(FrameworkElement anchor)
    {
        var error = ShellShortcuts.OpenAvailableNetworks(anchor);
        if (error is not null)
        {
            Report(ShellShortcuts.OpenQuickSettings(anchor));
        }
    }

    private async Task OpenBluetoothDetailsAsync(FrameworkElement anchor)
    {
        var requestVersion = ++_bluetoothRequestVersion;
        var activationVersion = _activationVersion;
        if (_detailFlyout.IsOpen || _detailFlyout.RecentlyClosed(anchor))
        {
            _detailFlyout.Close();
            return;
        }

        IReadOnlyList<(string Name, bool Connected)> devices = [];
        try
        {
            var found = await DeviceInformation.FindAllAsync(
                BluetoothDevice.GetDeviceSelectorFromPairingState(true),
                ["System.Devices.Aep.IsConnected"]);
            devices = found
                .Select(d => (
                    Name: string.IsNullOrWhiteSpace(d.Name) ? UiText.Get("Bluetooth") : d.Name,
                    Connected: d.Properties.TryGetValue("System.Devices.Aep.IsConnected", out var val) && val is true))
                .OrderByDescending(d => d.Connected)
                .ThenBy(d => d.Name, StringComparer.CurrentCultureIgnoreCase)
                .Take(8)
                .ToList();
        }
        catch (Exception ex) when (ex is COMException or UnauthorizedAccessException or InvalidOperationException)
        {
            AppLog.Warning($"Bluetooth device listing unavailable: {ex.Message}");
        }

        if (!IsActive || activationVersion != _activationVersion || requestVersion != _bluetoothRequestVersion) return;

        var root = new StackPanel { Width = 264 };
        root.Children.Add(DockFlyout.Heading(UiText.Get("BluetoothDevices")));

        if (devices.Count == 0)
        {
            root.Children.Add(DockFlyout.Caption(UiText.Get("BluetoothNoDevices")));
        }
        else
        {
            foreach (var device in devices)
            {
                var row = new DockPanel { Margin = new Thickness(0, 3, 0, 3) };
                var status = new TextBlock
                {
                    Text = UiText.Get(device.Connected ? "BluetoothConnected" : "BluetoothPaired"),
                    FontSize = 11,
                    VerticalAlignment = VerticalAlignment.Center,
                    Margin = new Thickness(8, 0, 0, 0)
                };
                status.SetResourceReference(TextBlock.ForegroundProperty, device.Connected ? "AccentBrush" : "TextSecondary");
                DockPanel.SetDock(status, Dock.Right);
                row.Children.Add(status);

                var label = new TextBlock
                {
                    Text = device.Name,
                    VerticalAlignment = VerticalAlignment.Center,
                    TextTrimming = TextTrimming.CharacterEllipsis
                };
                label.SetResourceReference(StyleProperty, "WidgetText");
                row.Children.Add(label);
                root.Children.Add(DockFlyout.FlatButton(row, (_, _) =>
                {
                    _detailFlyout.Close();
                    Report(ShellShortcuts.OpenQuickSettings(this));
                }, device.Name));
            }
        }

        var quickAction = DockFlyout.FlatButton(
            new TextBlock { Text = UiText.Get("QuickSettingsPanel"), FontWeight = FontWeights.SemiBold },
            (_, _) =>
            {
                _detailFlyout.Close();
                Report(ShellShortcuts.OpenQuickSettings(this));
            });
        quickAction.Margin = new Thickness(0, 6, 0, 0);
        root.Children.Add(quickAction);

        var openSettings = DockFlyout.FlatButton(
            DockFlyout.Caption(UiText.Format("RadioSettings", UiText.Get("Bluetooth"))),
            (_, _) =>
            {
                _detailFlyout.Close();
                Report(ShellShortcuts.OpenUri("ms-settings:bluetooth"));
            });
        root.Children.Add(openSettings);

        _detailFlyout.Show(anchor, root);
    }

    protected override void OnDisposed()
    {
        _detailFlyout.Close();
        _confirm.Dispose();
        base.OnDisposed();
    }
    private void WifiSettings_Click(object sender, RoutedEventArgs e) => OpenWifiDetails(sender as FrameworkElement ?? this);
    private void BluetoothSettings_Click(object sender, RoutedEventArgs e)
    {
        var anchor = sender as FrameworkElement ?? this;
        if (_detailFlyout.IsOpen || _detailFlyout.RecentlyClosed(anchor))
        {
            _bluetoothRequestVersion++;
            _detailFlyout.Close();
            return;
        }
        _ = OpenBluetoothDetailsAsync(anchor);
    }
    private void Display_Click(object sender, RoutedEventArgs e) => Report(ShellShortcuts.OpenProjection(sender as FrameworkElement ?? this));
    private void Hotspot_Click(object sender, RoutedEventArgs e) => Report(ShellShortcuts.OpenQuickSettings(sender as FrameworkElement ?? this));
    private void QuickSettings_Click(object sender, RoutedEventArgs e) => Report(ShellShortcuts.OpenQuickSettings(sender as FrameworkElement ?? this));
    private void Notifications_Click(object sender, RoutedEventArgs e) => Report(ShellShortcuts.OpenNotificationCenter());
}
