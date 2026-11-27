using System.ComponentModel;
using System.Runtime.InteropServices;
using System.Windows;
using System.Windows.Automation;
using System.Windows.Controls;
using System.Windows.Input;
using System.Windows.Media;
using System.Windows.Threading;
using OpenTaskbar.Models;
using OpenTaskbar.Services;

namespace OpenTaskbar.Widgets;

/// <summary>
/// Minimal Audio widget: dock card shows speaker icon and output volume percentage;
/// clicking or hovering opens the 3-section vertical mixer flyout (Output, Input, Applications).
/// </summary>
public partial class AudioWidget : DockWidget
{
    private readonly DockFlyout _flyout = new();
    private AudioEndpointMonitor.SharedHandle? _monitor;
    private bool _updating;
    private bool _updatingMic;
    private int _wheelRemainder;
    private bool? _lastHasDevice;
    private int _lastLevel = -1;
    private bool _lastMuted;
    private string? _lastDeviceName;
    private string? _lastOutputIconKey;
    private AppLanguage _lastLanguage;

    private Border? _flyoutContainer;
    private Slider? _flyoutSlider;
    private TextBlock? _flyoutLevelText;
    private ContentControl? _flyoutMuteIcon;
    private Button? _flyoutMuteButton;
    private Slider? _flyoutMicSlider;
    private TextBlock? _flyoutMicLevelText;
    private ContentControl? _flyoutMicMuteIcon;
    private Button? _flyoutMicMuteButton;
    private readonly List<AppAudioSession> _appSessions = [];

    // Allows the mixer controls to be tested without real audio hardware.
    internal Func<IReadOnlyList<AppAudioSession>>? AppSessionSource { get; set; }

    private void ReleaseAppSessions()
    {
        foreach (var session in _appSessions) session.Dispose();
        _appSessions.Clear();
    }

    public override bool SupportsCompactMode => true;
    public override int ShrinkPriority => 18;
    public override double FullWidthEstimate => 76;
    public override double CompactWidthEstimate => 42;

    public AudioWidget()
    {
        InitializeComponent();
        ApplyCardForeground(mutedOrUnavailable: false);
    }

    protected override void OnCompactChanged(bool compact) => ApplyDensityLayout();
    protected override void OnHoverExpandedChanged(bool expanded) => ApplyDensityLayout();
    protected override void OnDynamicIslandChanged(bool dynamicIsland) => ApplyDensityLayout();

    private void ApplyDensityLayout()
    {
        var compact = IsCompact && !IsHoverExpanded;
        ComboButton.Visibility = IsDynamicIsland ? Visibility.Collapsed : Visibility.Visible;
        IslandPanel.Visibility = IsDynamicIsland ? Visibility.Visible : Visibility.Collapsed;
        VolumeText.Visibility = compact ? Visibility.Collapsed : Visibility.Visible;
        OutputIcon.Margin = compact ? new Thickness(0) : new Thickness(0, 0, 7, 0);
        UpdateDeviceTooltip();
        RaiseContentWidthChanged();
    }

    private void ApplyCardForeground(bool mutedOrUnavailable)
    {
        var key = mutedOrUnavailable ? "TextSecondary" : "TextPrimary";
        OutputIcon.SetResourceReference(ForegroundProperty, key);
        VolumeText.SetResourceReference(TextBlock.ForegroundProperty, key);
    }

    public override UIElement? BuildHoverFlyoutContent() => BuildFlyoutContent();

    public UIElement BuildFlyoutContent()
    {
        var container = new Border { Width = 296 };
        _flyoutContainer = container;
        container.Unloaded += (_, _) =>
        {
            if (!ReferenceEquals(_flyoutContainer, container)) return;
            ReleaseAppSessions();
            _flyoutContainer = null;
            _flyoutSlider = null;
            _flyoutLevelText = null;
            _flyoutMuteIcon = null;
            _flyoutMuteButton = null;
            _flyoutMicSlider = null;
            _flyoutMicLevelText = null;
            _flyoutMicMuteIcon = null;
            _flyoutMicMuteButton = null;
        };

        PopulateFlyout(container);
        return container;
    }

    private void PopulateFlyout(Border container)
    {
        ReleaseAppSessions();
        var root = new StackPanel();

        // Unified top header
        root.Children.Add(DockFlyout.Header(UiText.Get("WidgetAudio")));

        // 1. Output section (vertical)
        root.Children.Add(BuildOutputSection());
        root.Children.Add(DockFlyout.Divider(11));

        // 2. Input section (vertical, stacked directly below Output)
        root.Children.Add(BuildInputSection());
        root.Children.Add(DockFlyout.Divider(11));

        // 3. Applications section
        root.Children.Add(BuildApplicationsSection());

        container.Child = root;
    }

    private StackPanel BuildOutputSection()
    {
        var devices = _monitor?.GetPlaybackDevices() ?? [];
        var active = devices.FirstOrDefault(d => d.IsDefault) ?? (devices.Count > 0 ? devices[0] : null);
        var hasDevice = _monitor?.HasDevice == true;
        var level = hasDevice ? (int)Math.Round(_monitor!.Volume * 100) : 0;
        var muted = _monitor?.Muted == true;
        var headingIconKey = IsHeadphoneDevice(active?.Name) ? "IconHeadphones" : "IconVolume";
        var muteIconKey = muted || level == 0 ? "IconVolumeMute" : level < 50 ? "IconVolumeLow" : "IconVolume";

        return BuildEndpointSection(
            devices, active, hasDevice, level, muted,
            headingIconKey, "AudioOutput", "AudioOutputDevices", "NoAudioDevice", muteIconKey, "MuteToggle",
            onDeviceSelected: devId =>
            {
                _monitor!.SelectDefaultDevice(devId);
                _monitor.BindDefaultDevice();
                Render();
                if (_flyoutContainer is not null) PopulateFlyout(_flyoutContainer);
            },
            onMuteClick: MuteButton_Click,
            onVolumeChanged: value =>
            {
                if (_updating || _monitor is null) return;
                if (_monitor.SetVolume(value)) Render();
                else Report(UiText.Get("ErrorVolumeChange"));
            },
            out _flyoutMuteIcon, out _flyoutMuteButton, out _flyoutSlider, out _flyoutLevelText);
    }

    private StackPanel BuildInputSection()
    {
        var devices = _monitor?.GetRecordingDevices() ?? [];
        var active = devices.FirstOrDefault(d => d.IsDefault) ?? (devices.Count > 0 ? devices[0] : null);
        var hasDevice = _monitor?.HasMicDevice == true;
        var level = hasDevice ? (int)Math.Round(_monitor!.MicVolume * 100) : 0;
        var muted = _monitor?.MicMuted == true;

        return BuildEndpointSection(
            devices, active, hasDevice, level, muted,
            "IconMic", "AudioInput", "AudioInputDevices", "NoMicDevice", muted ? "IconMicMute" : "IconMic", "MicMuteToggle",
            onDeviceSelected: devId =>
            {
                _monitor!.SelectDefaultDevice(devId);
                _monitor.BindDefaultMicDevice();
                RenderMic();
                Render();
                if (_flyoutContainer is not null) PopulateFlyout(_flyoutContainer);
            },
            onMuteClick: (_, _) =>
            {
                if (_monitor is null) return;
                if (_monitor.ToggleMicMute()) RenderMic();
            },
            onVolumeChanged: value =>
            {
                if (_updatingMic || _monitor is null) return;
                if (_monitor.SetMicVolume(value)) RenderMic();
            },
            out _flyoutMicMuteIcon, out _flyoutMicMuteButton, out _flyoutMicSlider, out _flyoutMicLevelText);
    }

    private StackPanel BuildEndpointSection(
        IReadOnlyList<AudioDeviceInfo> devices,
        AudioDeviceInfo? activeDevice,
        bool hasDevice,
        int level,
        bool muted,
        string headingIconKey,
        string titleKey,
        string devicesLabelKey,
        string emptyDeviceKey,
        string muteIconKey,
        string muteToggleKey,
        Action<string> onDeviceSelected,
        RoutedEventHandler onMuteClick,
        Action<float> onVolumeChanged,
        out ContentControl muteIconOut,
        out Button muteButtonOut,
        out Slider sliderOut,
        out TextBlock levelTextOut)
    {
        var section = new StackPanel();
        var headingRow = new DockPanel { Margin = new Thickness(0, 0, 0, 8) };
        var levelText = new TextBlock
        {
            Text = hasDevice ? (muted ? "0%" : UiText.Format("Percent", level)) : "—",
            FontSize = 11.5,
            FontWeight = FontWeights.SemiBold,
            VerticalAlignment = VerticalAlignment.Center,
            Typography = { NumeralAlignment = FontNumeralAlignment.Tabular }
        };
        levelText.SetResourceReference(TextBlock.ForegroundProperty, "TextSecondary");
        DockPanel.SetDock(levelText, Dock.Right);
        headingRow.Children.Add(levelText);

        var headingLeft = new StackPanel { Orientation = Orientation.Horizontal, VerticalAlignment = VerticalAlignment.Center };
        var headingIcon = new ContentControl
        {
            Width = 15,
            Height = 15,
            Content = FindResource(headingIconKey),
            Margin = new Thickness(0, 0, 7, 0),
            VerticalAlignment = VerticalAlignment.Center
        };
        headingIcon.SetResourceReference(StyleProperty, "WidgetIcon");
        headingIcon.SetResourceReference(ForegroundProperty, "TextSecondary");

        var headingLabel = new TextBlock
        {
            Text = UiText.Get(titleKey),
            FontSize = 12,
            FontWeight = FontWeights.SemiBold,
            VerticalAlignment = VerticalAlignment.Center
        };
        headingLabel.SetResourceReference(TextBlock.ForegroundProperty, "TextPrimary");
        headingLeft.Children.Add(headingIcon);
        headingLeft.Children.Add(headingLabel);
        headingRow.Children.Add(headingLeft);
        section.Children.Add(headingRow);

        var controlRow = new Grid { Margin = new Thickness(0, 0, 0, 8) };
        controlRow.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        controlRow.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });

        var combo = new ComboBox
        {
            IsEnabled = devices.Count > 0,
            Margin = new Thickness(0, 0, 8, 0)
        };
        combo.SetResourceReference(StyleProperty, "WidgetComboBox");
        AutomationProperties.SetName(combo, UiText.Get(devicesLabelKey));

        if (devices.Count == 0)
        {
            combo.Items.Add(new ComboBoxItem { Content = UiText.Get(emptyDeviceKey), IsSelected = true });
        }
        else
        {
            var selectedIdx = 0;
            for (var i = 0; i < devices.Count; i++)
            {
                var dev = devices[i];
                combo.Items.Add(new ComboBoxItem { Content = dev.Name, Tag = dev.Id });
                if (dev.IsDefault) selectedIdx = i;
            }
            combo.SelectedIndex = selectedIdx;
        }

        combo.SelectionChanged += (_, _) =>
        {
            if (_monitor is null || combo.SelectedItem is not ComboBoxItem { Tag: string devId }) return;
            if (activeDevice is not null && string.Equals(activeDevice.Id, devId, StringComparison.Ordinal)) return;
            onDeviceSelected(devId);
        };

        var muteIcon = new ContentControl { Width = 15, Height = 15, Content = FindResource(muteIconKey) };
        muteIcon.SetResourceReference(StyleProperty, "WidgetIcon");

        var muteBtn = CreateMuteButton(muteIcon, muted, hasDevice, UiText.Get(muteToggleKey));
        muteBtn.Click += onMuteClick;

        Grid.SetColumn(combo, 0);
        Grid.SetColumn(muteBtn, 1);
        controlRow.Children.Add(combo);
        controlRow.Children.Add(muteBtn);
        section.Children.Add(controlRow);

        var slider = new Slider
        {
            Minimum = 0,
            Maximum = 100,
            Value = muted ? 0 : level,
            IsEnabled = hasDevice,
            VerticalAlignment = VerticalAlignment.Center
        };
        slider.SetResourceReference(StyleProperty, "WidgetSlider");
        AutomationProperties.SetName(slider, UiText.Get(titleKey));
        slider.ValueChanged += (_, args) => onVolumeChanged((float)(args.NewValue / 100.0));
        section.Children.Add(slider);

        muteIconOut = muteIcon;
        muteButtonOut = muteBtn;
        sliderOut = slider;
        levelTextOut = levelText;
        return section;
    }

    private StackPanel BuildApplicationsSection()
    {
        var section = new StackPanel();
        var appHeader = new TextBlock
        {
            Text = UiText.Get("AudioApps"),
            FontSize = 12,
            FontWeight = FontWeights.SemiBold,
            Margin = new Thickness(0, 0, 0, 8)
        };
        appHeader.SetResourceReference(TextBlock.ForegroundProperty, "TextPrimary");
        section.Children.Add(appHeader);

        var sessions = AppSessionSource?.Invoke() ?? _monitor?.GetAppSessions() ?? [];
        _appSessions.AddRange(sessions);
        if (sessions.Count == 0)
        {
            var emptyText = new TextBlock
            {
                Text = UiText.Get("NoActiveAudioApps"),
                FontSize = 11.5,
                Margin = new Thickness(0, 2, 0, 2)
            };
            emptyText.SetResourceReference(TextBlock.ForegroundProperty, "TextSecondary");
            section.Children.Add(emptyText);
            return section;
        }

        var appList = new StackPanel();
        foreach (var session in sessions)
        {
            var appRow = new Grid { Margin = new Thickness(0, 3, 0, 3), MinHeight = 27 };
            appRow.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(24, GridUnitType.Pixel) });
            appRow.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(82, GridUnitType.Pixel) });
            appRow.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(24, GridUnitType.Pixel) });
            appRow.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
            appRow.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(34, GridUnitType.Pixel) });

            UIElement appIconEl;
            if (session.Icon is not null)
            {
                var img = new Image
                {
                    Source = session.Icon,
                    Width = 18,
                    Height = 18,
                    VerticalAlignment = VerticalAlignment.Center,
                    HorizontalAlignment = HorizontalAlignment.Left
                };
                RenderOptions.SetBitmapScalingMode(img, BitmapScalingMode.HighQuality);
                appIconEl = img;
            }
            else
            {
                var ic = new ContentControl
                {
                    Width = 15,
                    Height = 15,
                    Content = FindResource("IconSliders"),
                    VerticalAlignment = VerticalAlignment.Center,
                    HorizontalAlignment = HorizontalAlignment.Left
                };
                ic.SetResourceReference(StyleProperty, "WidgetIcon");
                ic.SetResourceReference(ForegroundProperty, "TextSecondary");
                appIconEl = ic;
            }

            var nameBlock = new TextBlock
            {
                Text = session.Name,
                FontSize = 11.5,
                FontWeight = FontWeights.Medium,
                TextTrimming = TextTrimming.CharacterEllipsis,
                VerticalAlignment = VerticalAlignment.Center,
                ToolTip = session.Name,
                Margin = new Thickness(2, 0, 4, 0)
            };
            nameBlock.SetResourceReference(TextBlock.ForegroundProperty, "TextPrimary");

            var currentMuted = session.IsMuted;
            var appVol = (int)Math.Round(session.Volume * 100);

            var appIcon = new ContentControl
            {
                Width = 13,
                Height = 13,
                Content = FindResource(currentMuted || appVol == 0 ? "IconVolumeMute" : "IconVolume")
            };
            appIcon.SetResourceReference(StyleProperty, "WidgetIcon");

            var appMuteBtn = new Button
            {
                Width = 22,
                Height = 22,
                Content = appIcon,
                ToolTip = UiText.Get("MuteToggle")
            };
            appMuteBtn.SetResourceReference(StyleProperty, "WidgetIconButton");

            var appPercent = new TextBlock
            {
                Text = currentMuted ? "—" : UiText.Format("Percent", appVol),
                FontSize = 11,
                FontWeight = FontWeights.SemiBold,
                VerticalAlignment = VerticalAlignment.Center,
                TextAlignment = TextAlignment.Right,
                Typography = { NumeralAlignment = FontNumeralAlignment.Tabular }
            };
            appPercent.SetResourceReference(StyleProperty, "WidgetCaption");

            var appSlider = new Slider
            {
                Minimum = 0,
                Maximum = 100,
                Value = appVol,
                VerticalAlignment = VerticalAlignment.Center,
                Margin = new Thickness(4, 0, 4, 0)
            };
            appSlider.SetResourceReference(StyleProperty, "WidgetSlider");

            var localSession = session;
            var appUpdating = false;
            var confirmedLevel = appVol;

            appSlider.ValueChanged += (_, args) =>
            {
                if (appUpdating) return;
                var newLevel = (int)Math.Round(args.NewValue);
                if (!localSession.SetVolume(newLevel / 100f))
                {
                    appUpdating = true;
                    try { appSlider.Value = confirmedLevel; }
                    finally { appUpdating = false; }
                    Report(UiText.Get("ErrorVolumeChange"));
                    return;
                }

                confirmedLevel = newLevel;
                if (currentMuted && newLevel > 0)
                {
                    if (localSession.SetMute(false)) currentMuted = false;
                    else Report(UiText.Get("ErrorVolumeChange"));
                }
                appIcon.Content = FindResource(currentMuted || newLevel == 0 ? "IconVolumeMute" : "IconVolume");
                appPercent.Text = currentMuted ? "—" : UiText.Format("Percent", newLevel);
            };

            appMuteBtn.Click += (_, _) =>
            {
                var mute = !currentMuted;
                if (!localSession.SetMute(mute))
                {
                    Report(UiText.Get("ErrorVolumeChange"));
                    return;
                }
                currentMuted = mute;
                appIcon.Content = FindResource(currentMuted || (int)appSlider.Value == 0 ? "IconVolumeMute" : "IconVolume");
                appPercent.Text = currentMuted ? "—" : UiText.Format("Percent", (int)Math.Round(appSlider.Value));
            };

            Grid.SetColumn(appIconEl, 0);
            Grid.SetColumn(nameBlock, 1);
            Grid.SetColumn(appMuteBtn, 2);
            Grid.SetColumn(appSlider, 3);
            Grid.SetColumn(appPercent, 4);

            appRow.Children.Add(appIconEl);
            appRow.Children.Add(nameBlock);
            appRow.Children.Add(appMuteBtn);
            appRow.Children.Add(appSlider);
            appRow.Children.Add(appPercent);
            appList.Children.Add(appRow);
        }

        var scrollViewer = new ScrollViewer
        {
            Content = appList,
            MaxHeight = 160,
            VerticalScrollBarVisibility = ScrollBarVisibility.Auto,
            HorizontalScrollBarVisibility = ScrollBarVisibility.Disabled,
            Focusable = false
        };
        section.Children.Add(scrollViewer);
        return section;
    }

    private static Button CreateMuteButton(ContentControl icon, bool muted, bool enabled, string toolTip)
    {
        var btn = new Button
        {
            Width = 32,
            Height = 32,
            Content = icon,
            IsEnabled = enabled,
            ToolTip = toolTip,
            Padding = new Thickness(0)
        };
        btn.SetResourceReference(StyleProperty, "WidgetFlyoutActionButton");
        ApplyMuteButtonVisual(btn, muted);
        AutomationProperties.SetName(btn, toolTip);
        return btn;
    }

    private static void ApplyMuteButtonVisual(Button button, bool muted)
    {
        button.SetResourceReference(ForegroundProperty, muted ? "WidgetDangerForeground" : "TextSecondary");
        button.SetResourceReference(BorderBrushProperty, muted ? "WidgetDangerStroke" : "StrokeSubtle");
        button.SetResourceReference(BackgroundProperty, muted ? "WidgetDangerHover" : "SurfaceSelected");
    }

    private static bool IsHeadphoneDevice(string? name) => name is not null && (
        name.Contains("headphone", StringComparison.OrdinalIgnoreCase)
        || name.Contains("kulaklık", StringComparison.OrdinalIgnoreCase)
        || name.Contains("headset", StringComparison.OrdinalIgnoreCase)
        || name.Contains("buds", StringComparison.OrdinalIgnoreCase)
        || name.Contains("airpod", StringComparison.OrdinalIgnoreCase));

    private void UpdateDeviceTooltip()
    {
        ComboButton.ToolTip = _monitor?.HasDevice == true
            ? _monitor.DeviceName ?? UiText.Get("AudioOutput")
            : UiText.Get("NoAudioDevice");
    }

    protected override void OnActivated()
    {
        try
        {
            _monitor = AudioEndpointMonitor.AcquireShared();
            _monitor.VolumeChanged += Monitor_VolumeChanged;
            _monitor.DefaultDeviceChanged += Monitor_DefaultDeviceChanged;
            _monitor.MicVolumeChanged += Monitor_MicVolumeChanged;
            _monitor.DefaultMicChanged += Monitor_DefaultMicChanged;
        }
        catch (COMException ex)
        {
            AppLog.Warning($"Core Audio unavailable: {ex.Message}");
            _monitor = null;
        }

        InvalidateCache();
        Render();
        RenderMic();
    }

    protected override void OnDeactivated()
    {
        _flyout.Close();
        ReleaseAppSessions();
        if (_monitor is null) return;
        _monitor.VolumeChanged -= Monitor_VolumeChanged;
        _monitor.DefaultDeviceChanged -= Monitor_DefaultDeviceChanged;
        _monitor.MicVolumeChanged -= Monitor_MicVolumeChanged;
        _monitor.DefaultMicChanged -= Monitor_DefaultMicChanged;
        _monitor.Dispose();
        _monitor = null;
    }

    protected override void OnDisposed()
    {
        _flyout.Close(restoreFocus: false);
        ReleaseAppSessions();
    }

    protected override void OnLanguageChanged()
    {
        Render();
        RenderMic();
        if (_flyoutContainer is not null) PopulateFlyout(_flyoutContainer);
    }

    private void InvalidateCache()
    {
        _lastHasDevice = null;
        _lastLevel = -1;
        _lastOutputIconKey = null;
        _lastDeviceName = null;
    }

    private void Monitor_VolumeChanged() => Dispatcher.BeginInvoke(DispatcherPriority.Background, () =>
    {
        if (IsActive) Render();
    });

    private void Monitor_DefaultDeviceChanged() => Dispatcher.BeginInvoke(DispatcherPriority.Background, () =>
    {
        if (!IsActive || _monitor is null) return;
        _monitor.BindDefaultDevice();
        AppLog.Info($"Default audio device: {_monitor.DeviceName}");
        Render();
        if (_flyoutContainer is not null) PopulateFlyout(_flyoutContainer);
    });

    private void Monitor_MicVolumeChanged() => Dispatcher.BeginInvoke(DispatcherPriority.Background, () =>
    {
        if (IsActive) RenderMic();
    });

    private void Monitor_DefaultMicChanged() => Dispatcher.BeginInvoke(DispatcherPriority.Background, () =>
    {
        if (!IsActive || _monitor is null) return;
        _monitor.BindDefaultMicDevice();
        AppLog.Info($"Default mic device: {_monitor.MicDeviceName}");
        RenderMic();
        Render();
        if (_flyoutContainer is not null) PopulateFlyout(_flyoutContainer);
    });

    private void RenderMic()
    {
        if (_flyoutMicSlider is null || _flyoutMicLevelText is null || _flyoutMicMuteIcon is null) return;
        var hasMic = _monitor?.HasMicDevice == true;
        var micMuted = _monitor?.MicMuted == true;
        var micLevel = hasMic ? (int)Math.Round(_monitor!.MicVolume * 100) : 0;

        _updatingMic = true;
        _flyoutMicSlider.Value = micMuted ? 0 : micLevel;
        _flyoutMicLevelText.Text = hasMic ? (micMuted ? "0%" : UiText.Format("Percent", micLevel)) : "—";
        _flyoutMicMuteIcon.Content = FindResource(micMuted ? "IconMicMute" : "IconMic");
        if (_flyoutMicMuteButton is not null) ApplyMuteButtonVisual(_flyoutMicMuteButton, micMuted);
        _updatingMic = false;
    }

    private void Render()
    {
        var hasDevice = _monitor?.HasDevice == true;
        var lang = UiText.Current.Language;
        if (!hasDevice)
        {
            if (_lastHasDevice == false && _lastLanguage == lang)
            {
                UpdateIslandOutputSlider(hasDevice: false, level: 0);
                return;
            }
            _lastHasDevice = false;
            _lastLanguage = lang;
            _lastLevel = -1;
            VolumeText.Text = "—";
            ApplyCardForeground(mutedOrUnavailable: true);
            UpdateDeviceTooltip();
            SetOutputIcon("IconVolumeMute");
            if (_flyoutSlider is not null) _flyoutSlider.IsEnabled = false;
            if (_flyoutLevelText is not null) _flyoutLevelText.Text = "—";
            if (_flyoutMuteIcon is not null) _flyoutMuteIcon.Content = FindResource("IconVolumeMute");
            UpdateIslandOutputSlider(hasDevice: false, level: 0);
            return;
        }

        var level = (int)Math.Round(_monitor!.Volume * 100);
        var muted = _monitor.Muted;
        var deviceName = _monitor.DeviceName ?? UiText.Get("AudioOutput");
        if (_lastHasDevice == true && _lastLevel == level && _lastMuted == muted
            && _lastDeviceName == deviceName && _lastLanguage == lang)
        {
            return;
        }

        _lastHasDevice = true;
        _lastLevel = level;
        _lastMuted = muted;
        _lastDeviceName = deviceName;
        _lastLanguage = lang;

        var isSilent = muted || level == 0;
        VolumeText.Text = muted ? "0%" : UiText.Format("Percent", level);
        ApplyCardForeground(mutedOrUnavailable: isSilent);
        UpdateDeviceTooltip();

        var iconKey = isSilent ? "IconVolumeMute" : level < 50 ? "IconVolumeLow" : "IconVolume";
        SetOutputIcon(iconKey);

        _updating = true;
        UpdateIslandOutputSlider(hasDevice: true, level: muted ? 0 : level);
        if (_flyoutSlider is not null)
        {
            _flyoutSlider.IsEnabled = true;
            _flyoutSlider.Value = muted ? 0 : level;
        }
        if (_flyoutLevelText is not null) _flyoutLevelText.Text = muted ? "0%" : UiText.Format("Percent", level);
        if (_flyoutMuteIcon is not null)
        {
            _flyoutMuteIcon.Content = FindResource(iconKey);
        }
        if (_flyoutMuteButton is not null) ApplyMuteButtonVisual(_flyoutMuteButton, muted);
        _updating = false;
    }

    private void UpdateIslandOutputSlider(bool hasDevice, int level)
    {
        _updating = true;
        IslandOutputSlider.IsEnabled = hasDevice;
        IslandOutputSlider.Value = level;
        _updating = false;
    }

    private void SetOutputIcon(string key)
    {
        if (_lastOutputIconKey == key) return;
        _lastOutputIconKey = key;
        OutputIcon.Content = FindResource(key);
    }

    private void MuteButton_Click(object sender, RoutedEventArgs e)
    {
        if (_monitor is null) return;
        if (_monitor.ToggleMute()) Render();
        else Report(UiText.Get("ErrorVolumeChange"));
    }

    private void ComboButton_Click(object sender, RoutedEventArgs e)
    {
        _flyout.Toggle(ComboButton, BuildFlyoutContent, animate: true, focusable: false);
    }

    private void IslandOutputSlider_ValueChanged(object sender, RoutedPropertyChangedEventArgs<double> e)
    {
        if (_updating || _monitor is null) return;
        if (_monitor.SetVolume((float)(e.NewValue / 100.0))) Render();
        else Report(UiText.Get("ErrorVolumeChange"));
    }

    private void AudioWidget_PreviewMouseWheel(object sender, MouseWheelEventArgs e)
    {
        if (e.Handled || e.Delta == 0 || _monitor?.HasDevice != true) return;

        e.Handled = true;
        _wheelRemainder += e.Delta;
        var notches = _wheelRemainder / Mouse.MouseWheelDeltaForOneLine;
        if (notches == 0) return;

        _wheelRemainder -= notches * Mouse.MouseWheelDeltaForOneLine;
        var level = Math.Clamp(_monitor.Volume + notches * 0.02f, 0f, 1f);
        if (!_monitor.SetVolume(level)) Report(UiText.Get("ErrorVolumeChange"));
    }

}
