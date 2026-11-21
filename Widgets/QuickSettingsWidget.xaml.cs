using System.ComponentModel;
using System.Runtime.InteropServices;
using System.Windows;
using System.Windows.Automation;
using System.Windows.Controls;
using System.Windows.Controls.Primitives;
using System.Windows.Media;
using System.Windows.Threading;
using Microsoft.Win32;
using OpenTaskbar.Models;
using OpenTaskbar.Services;
using Windows.Devices.Radios;

namespace OpenTaskbar.Widgets;

/// <summary>
/// V19 Quick Settings widget: compact direct toggles for Wi-Fi, Bluetooth, and Night Light
/// backed by real Windows Radio APIs and Windows Night Light state.
/// </summary>
public partial class QuickSettingsWidget : DockWidget
{
    private readonly record struct RadioRenderState(bool Visible, bool On, bool HasRadio, AppLanguage Language);

    private const string NightLightStateRegPath =
        @"Software\Microsoft\Windows\CurrentVersion\CloudStore\Store\DefaultAccount\Current\default$windows.data.bluelightreduction.bluelightreductionstate\windows.data.bluelightreduction.bluelightreductionstate";

    private Radio? _wifi;
    private Radio? _bluetooth;
    private bool _radioAccess;
    private bool _nightLightOn;
    private int _activationVersion;
    private readonly HashSet<Radio> _pendingRadioChanges = [];
    private RadioRenderState? _lastWifiState;
    private RadioRenderState? _lastBluetoothState;
    private bool? _lastNightLightState;
    private readonly TwoStepConfirm _confirm;

    public override bool SupportsCompactMode => true;
    public override int ShrinkPriority => 20;
    public override double FullWidthEstimate => 112;
    public override double CompactWidthEstimate => 82;

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
        NightLightPill.Visibility = compact ? Visibility.Collapsed : Visibility.Visible;
    }

    // V19: Direct controls do not open a generic hover flyout.
    public override UIElement? BuildHoverFlyoutContent() => null;

    protected override async void OnActivated()
    {
        var activationVersion = ++_activationVersion;
        UiText.Current.PropertyChanged += UiText_PropertyChanged;
        _lastWifiState = null;
        _lastBluetoothState = null;
        _lastNightLightState = null;
        _nightLightOn = ReadWindowsNightLightEnabled();

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
        _activationVersion++;
        _pendingRadioChanges.Clear();
        UiText.Current.PropertyChanged -= UiText_PropertyChanged;
        if (_wifi is not null) _wifi.StateChanged -= Radio_StateChanged;
        if (_bluetooth is not null) _bluetooth.StateChanged -= Radio_StateChanged;
        _wifi = null;
        _bluetooth = null;
        _lastWifiState = null;
        _lastBluetoothState = null;
        _lastNightLightState = null;
    }

    protected override void OnTick(DateTime now)
    {
        // Keep Night Light indicator synchronized with Windows settings.
        if (now.Second % 5 == 0)
        {
            var current = ReadWindowsNightLightEnabled();
            if (current != _nightLightOn)
            {
                _nightLightOn = current;
                RenderNightLight();
            }
        }
    }

    private void UiText_PropertyChanged(object? sender, PropertyChangedEventArgs e) => Render();

    private void Radio_StateChanged(Radio sender, object args) =>
        Dispatcher.BeginInvoke(DispatcherPriority.Background, () =>
        {
            if (IsActive) Render();
        });

    private void Render()
    {
        RenderRadio(_wifi, WifiPill, WifiToggle, "WiFi", ref _lastWifiState);
        RenderRadio(_bluetooth, BluetoothPill, BluetoothToggle, "Bluetooth", ref _lastBluetoothState);
        RenderNightLight();
    }

    private void RenderRadio(Radio? radio, Border pill, ToggleButton toggle, string nameKey, ref RadioRenderState? lastState)
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
        ApplyChipVisual(pill, toggle, on);

        var name = UiText.Get(nameKey);
        var toggleLabel = hasRadio ? UiText.Format("RadioToggle", name) : UiText.Format("RadioSettings", name);
        toggle.ToolTip = toggleLabel;
        AutomationProperties.SetName(toggle, toggleLabel);
    }

    private void RenderNightLight()
    {
        if (_lastNightLightState == _nightLightOn) return;
        _lastNightLightState = _nightLightOn;
        NightLightToggle.IsChecked = _nightLightOn;
        ApplyChipVisual(NightLightPill, NightLightToggle, _nightLightOn);
        var label = UiText.Get("NightLight");
        NightLightToggle.ToolTip = label;
        AutomationProperties.SetName(NightLightToggle, label);
    }

    private static void ApplyChipVisual(Border pill, ToggleButton toggle, bool isOn)
    {
        if (isOn)
        {
            pill.SetResourceReference(Border.BackgroundProperty, "WidgetSelectedFill");
            pill.SetResourceReference(Border.BorderBrushProperty, "WidgetSelectedStroke");
            toggle.SetResourceReference(ForegroundProperty, "WidgetSelectedForeground");
        }
        else
        {
            pill.Background = Brushes.Transparent;
            pill.BorderBrush = Brushes.Transparent;
            toggle.SetResourceReference(ForegroundProperty, "TextSecondary");
        }
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

    private void NightLightToggle_Click(object sender, RoutedEventArgs e)
    {
        // Keep the visual toggle faithful to real Windows Night Light state; open Windows Night Light settings
        // so the user controls the authoritative OS setting without fake client-only switches.
        NightLightToggle.IsChecked = _nightLightOn;
        Report(ShellShortcuts.OpenUri("ms-settings:nightlight"));
    }

    private static bool ReadWindowsNightLightEnabled()
    {
        try
        {
            using var key = Registry.CurrentUser.OpenSubKey(NightLightStateRegPath);
            if (key?.GetValue("Data") is not byte[] data || data.Length < 25) return false;
            // Windows CloudStore Night Light binary blob: byte 18 is 0x15 when enabled (length >= 43) and 0x13 when disabled.
            return data[18] == 0x15;
        }
        catch
        {
            return false;
        }
    }

    private void ConfirmAndToggle(Radio? radio, Border pill, ToggleButton toggle, string nameKey)
    {
        if (radio is null)
        {
            _confirm.Disarm();
            Report(ShellShortcuts.OpenQuickSettings(this));
            Render();
            return;
        }

        toggle.IsChecked = radio.State == RadioState.On;
        if (!_confirm.ConsumeOrArm(
            pill,
            onArm: () =>
            {
                pill.BorderThickness = new Thickness(1.5);
                pill.SetResourceReference(Border.BorderBrushProperty, "AccentBrush");
                var confirmText = UiText.Format("RadioConfirmToggle", UiText.Get(nameKey));
                toggle.ToolTip = confirmText;
                AutomationProperties.SetName(toggle, confirmText);
            },
            onDisarm: () =>
            {
                pill.BorderThickness = new Thickness(1);
                if (pill == WifiPill) _lastWifiState = null;
                else _lastBluetoothState = null;
                if (IsActive) Render();
            }))
        {
            return;
        }

        ToggleRadio(radio);
    }

    protected override void OnDisposed()
    {
        _confirm.Dispose();
        base.OnDisposed();
    }
}
