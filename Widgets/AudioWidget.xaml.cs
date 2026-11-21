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
/// V19 combined Audio widget: one dock trigger showing output + input devices and chevron;
/// clicking opens the 3-section vertical mixer flyout (Output, Input, Applications).
/// </summary>
public partial class AudioWidget : DockWidget
{
    private readonly DockFlyout _flyout = new();
    private AudioEndpointMonitor.SharedHandle? _monitor;
    private bool _updating;
    private bool _updatingMic;
    private bool? _lastHasDevice;
    private int _lastLevel = -1;
    private bool _lastMuted;
    private string? _lastDeviceName;
    private string? _lastMicDeviceName;
    private bool _lastMicMuted;
    private string? _lastOutputIconKey;
    private string? _lastInputIconKey;
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
    public override double FullWidthEstimate => 210;
    public override double CompactWidthEstimate => 112;

    public AudioWidget()
    {
        InitializeComponent();
        OutputDeviceText.Text = UiText.Get("AudioOutput");
        InputDeviceText.Text = UiText.Get("AudioInput");
    }

    protected override void OnCompactChanged(bool compact) => ApplyDensityLayout();
    protected override void OnHoverExpandedChanged(bool expanded) => ApplyDensityLayout();

    private void ApplyDensityLayout()
    {
        var compact = IsCompact && !IsHoverExpanded;
        ComboButton.Padding = compact ? new Thickness(8, 0, 8, 0) : new Thickness(11, 0, 11, 0);
        DeviceSeparator.Visibility = compact ? Visibility.Collapsed : Visibility.Visible;
        InputIcon.Visibility = compact ? Visibility.Collapsed : Visibility.Visible;
        InputDeviceText.Visibility = compact ? Visibility.Collapsed : Visibility.Visible;
    }

    public override UIElement? BuildHoverFlyoutContent() => BuildFlyoutContent();

    public UIElement BuildFlyoutContent()
    {
        var container = new Border { Width = 314 };
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

        // Top title bar
        var title = new TextBlock
        {
            Text = UiText.Get("WidgetAudio"),
            FontWeight = FontWeights.Bold,
            FontSize = 12.5,
            Margin = new Thickness(0, 0, 0, 10)
        };
        title.SetResourceReference(TextBlock.ForegroundProperty, "TextPrimary");
        root.Children.Add(title);

        // 1. Output section (vertical)
        root.Children.Add(BuildOutputSection());
        root.Children.Add(CreateSectionDivider());

        // 2. Input section (vertical, stacked directly below Output)
        root.Children.Add(BuildInputSection());
        root.Children.Add(CreateSectionDivider());

        // 3. Applications section
        root.Children.Add(BuildApplicationsSection());

        container.Child = root;
    }

    private StackPanel BuildOutputSection()
    {
        var section = new StackPanel();
        var outDevices = _monitor?.GetPlaybackDevices() ?? [];
        var activeOutDev = outDevices.FirstOrDefault(d => d.IsDefault) ?? (outDevices.Count > 0 ? outDevices[0] : null);
        var hasOutDevice = _monitor?.HasDevice == true;
        var outLevel = hasOutDevice ? (int)Math.Round(_monitor!.Volume * 100) : 0;
        var outMuted = _monitor?.Muted == true;

        // Heading row: icon + "Output" + live percentage
        var headingRow = new DockPanel { Margin = new Thickness(0, 0, 0, 8) };
        var outLevelText = new TextBlock
        {
            Text = hasOutDevice ? (outMuted ? "0%" : UiText.Format("Percent", outLevel)) : "—",
            FontSize = 11.5,
            FontWeight = FontWeights.SemiBold,
            VerticalAlignment = VerticalAlignment.Center,
            Typography = { NumeralAlignment = FontNumeralAlignment.Tabular }
        };
        outLevelText.SetResourceReference(TextBlock.ForegroundProperty, "TextSecondary");
        DockPanel.SetDock(outLevelText, Dock.Right);
        headingRow.Children.Add(outLevelText);

        var headingLeft = new StackPanel { Orientation = Orientation.Horizontal, VerticalAlignment = VerticalAlignment.Center };
        var headingIcon = new ContentControl
        {
            Width = 15,
            Height = 15,
            Content = FindResource(IsHeadphoneDevice(activeOutDev?.Name) ? "IconHeadphones" : "IconVolume"),
            Margin = new Thickness(0, 0, 7, 0),
            VerticalAlignment = VerticalAlignment.Center
        };
        headingIcon.SetResourceReference(StyleProperty, "WidgetIcon");
        headingIcon.SetResourceReference(ForegroundProperty, "TextSecondary");

        var headingLabel = new TextBlock
        {
            Text = UiText.Get("AudioOutput"),
            FontSize = 12,
            FontWeight = FontWeights.SemiBold,
            VerticalAlignment = VerticalAlignment.Center
        };
        headingLabel.SetResourceReference(TextBlock.ForegroundProperty, "TextPrimary");
        headingLeft.Children.Add(headingIcon);
        headingLeft.Children.Add(headingLabel);
        headingRow.Children.Add(headingLeft);
        section.Children.Add(headingRow);

        // Control row: single-chevron ComboBox selector + mute button
        var controlRow = new Grid { Margin = new Thickness(0, 0, 0, 8) };
        controlRow.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        controlRow.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });

        var outCombo = new ComboBox
        {
            IsEnabled = outDevices.Count > 0,
            Margin = new Thickness(0, 0, 8, 0)
        };
        outCombo.SetResourceReference(StyleProperty, "WidgetComboBox");
        AutomationProperties.SetName(outCombo, UiText.Get("AudioOutputDevices"));

        if (outDevices.Count == 0)
        {
            outCombo.Items.Add(new ComboBoxItem { Content = UiText.Get("NoAudioDevice"), IsSelected = true });
        }
        else
        {
            var selectedIdx = 0;
            for (var i = 0; i < outDevices.Count; i++)
            {
                var dev = outDevices[i];
                outCombo.Items.Add(new ComboBoxItem { Content = dev.Name, Tag = dev.Id });
                if (dev.IsDefault) selectedIdx = i;
            }
            outCombo.SelectedIndex = selectedIdx;
        }

        outCombo.SelectionChanged += (_, _) =>
        {
            if (_monitor is null || outCombo.SelectedItem is not ComboBoxItem { Tag: string devId }) return;
            if (activeOutDev is not null && string.Equals(activeOutDev.Id, devId, StringComparison.Ordinal)) return;
            _monitor.SelectDefaultDevice(devId);
            _monitor.BindDefaultDevice();
            Render();
            if (_flyoutContainer is not null) PopulateFlyout(_flyoutContainer);
        };

        var outIconKey = outMuted || outLevel == 0 ? "IconVolumeMute" : outLevel < 50 ? "IconVolumeLow" : "IconVolume";
        var outMuteIcon = new ContentControl { Width = 15, Height = 15, Content = FindResource(outIconKey) };
        outMuteIcon.SetResourceReference(StyleProperty, "WidgetIcon");

        var outMuteBtn = CreateMuteButton(outMuteIcon, outMuted, hasOutDevice, UiText.Get("MuteToggle"));
        outMuteBtn.Click += MuteButton_Click;

        Grid.SetColumn(outCombo, 0);
        Grid.SetColumn(outMuteBtn, 1);
        controlRow.Children.Add(outCombo);
        controlRow.Children.Add(outMuteBtn);
        section.Children.Add(controlRow);

        // Output volume slider
        var outSlider = new Slider
        {
            Minimum = 0,
            Maximum = 100,
            Value = outMuted ? 0 : outLevel,
            IsEnabled = hasOutDevice,
            VerticalAlignment = VerticalAlignment.Center
        };
        outSlider.SetResourceReference(StyleProperty, "WidgetSlider");
        AutomationProperties.SetName(outSlider, UiText.Get("AudioOutput"));
        outSlider.ValueChanged += (_, args) =>
        {
            if (_updating || _monitor is null) return;
            if (_monitor.SetVolume((float)(args.NewValue / 100.0))) Render();
            else Report(UiText.Get("ErrorVolumeChange"));
        };
        section.Children.Add(outSlider);

        _flyoutMuteIcon = outMuteIcon;
        _flyoutMuteButton = outMuteBtn;
        _flyoutSlider = outSlider;
        _flyoutLevelText = outLevelText;

        return section;
    }

    private StackPanel BuildInputSection()
    {
        var section = new StackPanel();
        var inDevices = _monitor?.GetRecordingDevices() ?? [];
        var activeInDev = inDevices.FirstOrDefault(d => d.IsDefault) ?? (inDevices.Count > 0 ? inDevices[0] : null);
        var hasMic = _monitor?.HasMicDevice == true;
        var micLevel = hasMic ? (int)Math.Round(_monitor!.MicVolume * 100) : 0;
        var micMuted = _monitor?.MicMuted == true;

        // Heading row: mic icon + "Input" + live percentage
        var headingRow = new DockPanel { Margin = new Thickness(0, 0, 0, 8) };
        var micLevelText = new TextBlock
        {
            Text = hasMic ? (micMuted ? "0%" : UiText.Format("Percent", micLevel)) : "—",
            FontSize = 11.5,
            FontWeight = FontWeights.SemiBold,
            VerticalAlignment = VerticalAlignment.Center,
            Typography = { NumeralAlignment = FontNumeralAlignment.Tabular }
        };
        micLevelText.SetResourceReference(TextBlock.ForegroundProperty, "TextSecondary");
        DockPanel.SetDock(micLevelText, Dock.Right);
        headingRow.Children.Add(micLevelText);

        var headingLeft = new StackPanel { Orientation = Orientation.Horizontal, VerticalAlignment = VerticalAlignment.Center };
        var headingIcon = new ContentControl
        {
            Width = 15,
            Height = 15,
            Content = FindResource("IconMic"),
            Margin = new Thickness(0, 0, 7, 0),
            VerticalAlignment = VerticalAlignment.Center
        };
        headingIcon.SetResourceReference(StyleProperty, "WidgetIcon");
        headingIcon.SetResourceReference(ForegroundProperty, "TextSecondary");

        var headingLabel = new TextBlock
        {
            Text = UiText.Get("AudioInput"),
            FontSize = 12,
            FontWeight = FontWeights.SemiBold,
            VerticalAlignment = VerticalAlignment.Center
        };
        headingLabel.SetResourceReference(TextBlock.ForegroundProperty, "TextPrimary");
        headingLeft.Children.Add(headingIcon);
        headingLeft.Children.Add(headingLabel);
        headingRow.Children.Add(headingLeft);
        section.Children.Add(headingRow);

        // Control row: single-chevron ComboBox selector + mic mute button
        var controlRow = new Grid { Margin = new Thickness(0, 0, 0, 8) };
        controlRow.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        controlRow.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });

        var inCombo = new ComboBox
        {
            IsEnabled = inDevices.Count > 0,
            Margin = new Thickness(0, 0, 8, 0)
        };
        inCombo.SetResourceReference(StyleProperty, "WidgetComboBox");
        AutomationProperties.SetName(inCombo, UiText.Get("AudioInputDevices"));

        if (inDevices.Count == 0)
        {
            inCombo.Items.Add(new ComboBoxItem { Content = UiText.Get("NoMicDevice"), IsSelected = true });
        }
        else
        {
            var selectedIdx = 0;
            for (var i = 0; i < inDevices.Count; i++)
            {
                var dev = inDevices[i];
                inCombo.Items.Add(new ComboBoxItem { Content = dev.Name, Tag = dev.Id });
                if (dev.IsDefault) selectedIdx = i;
            }
            inCombo.SelectedIndex = selectedIdx;
        }

        inCombo.SelectionChanged += (_, _) =>
        {
            if (_monitor is null || inCombo.SelectedItem is not ComboBoxItem { Tag: string devId }) return;
            if (activeInDev is not null && string.Equals(activeInDev.Id, devId, StringComparison.Ordinal)) return;
            _monitor.SelectDefaultDevice(devId);
            _monitor.BindDefaultMicDevice();
            RenderMic();
            Render();
            if (_flyoutContainer is not null) PopulateFlyout(_flyoutContainer);
        };

        var micIcon = new ContentControl { Width = 15, Height = 15, Content = FindResource(micMuted ? "IconMicMute" : "IconMic") };
        micIcon.SetResourceReference(StyleProperty, "WidgetIcon");

        var micMuteBtn = CreateMuteButton(micIcon, micMuted, hasMic, UiText.Get("MicMuteToggle"));
        micMuteBtn.Click += (_, _) =>
        {
            if (_monitor is null) return;
            if (_monitor.ToggleMicMute()) RenderMic();
        };

        Grid.SetColumn(inCombo, 0);
        Grid.SetColumn(micMuteBtn, 1);
        controlRow.Children.Add(inCombo);
        controlRow.Children.Add(micMuteBtn);
        section.Children.Add(controlRow);

        // Input volume slider
        var micSlider = new Slider
        {
            Minimum = 0,
            Maximum = 100,
            Value = micMuted ? 0 : micLevel,
            IsEnabled = hasMic,
            VerticalAlignment = VerticalAlignment.Center
        };
        micSlider.SetResourceReference(StyleProperty, "WidgetSlider");
        AutomationProperties.SetName(micSlider, UiText.Get("AudioInput"));
        micSlider.ValueChanged += (_, args) =>
        {
            if (_updatingMic || _monitor is null) return;
            if (_monitor.SetMicVolume((float)(args.NewValue / 100.0))) RenderMic();
        };
        section.Children.Add(micSlider);

        _flyoutMicMuteIcon = micIcon;
        _flyoutMicMuteButton = micMuteBtn;
        _flyoutMicSlider = micSlider;
        _flyoutMicLevelText = micLevelText;

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

    private static Border CreateSectionDivider()
    {
        var divider = new Border
        {
            Height = 1,
            Margin = new Thickness(0, 11, 0, 11)
        };
        divider.SetResourceReference(Border.BackgroundProperty, "StrokeSubtle");
        return divider;
    }

    private static bool IsHeadphoneDevice(string? name) => name is not null && (
        name.Contains("headphone", StringComparison.OrdinalIgnoreCase)
        || name.Contains("kulaklık", StringComparison.OrdinalIgnoreCase)
        || name.Contains("headset", StringComparison.OrdinalIgnoreCase)
        || name.Contains("buds", StringComparison.OrdinalIgnoreCase)
        || name.Contains("airpod", StringComparison.OrdinalIgnoreCase));

    private static string ShortDeviceLabel(string? fullName, string fallback)
    {
        if (string.IsNullOrWhiteSpace(fullName)) return fallback;
        var paren = fullName.IndexOf('(');
        var trimmed = paren > 1 ? fullName[..paren].Trim() : fullName.Trim();
        return string.IsNullOrWhiteSpace(trimmed) ? fullName.Trim() : trimmed;
    }

    protected override void OnActivated()
    {
        UiText.Current.PropertyChanged += UiText_PropertyChanged;
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
        UiText.Current.PropertyChanged -= UiText_PropertyChanged;
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
        _flyout.Close();
        ReleaseAppSessions();
    }

    private void UiText_PropertyChanged(object? sender, PropertyChangedEventArgs e)
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
        _lastInputIconKey = null;
        _lastDeviceName = null;
        _lastMicDeviceName = null;
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
        var hasMic = _monitor?.HasMicDevice == true;
        var micName = hasMic
            ? ShortDeviceLabel(_monitor!.MicDeviceName, UiText.Get("Microphone"))
            : UiText.Get("NoMicDevice");
        var micMuted = _monitor?.MicMuted == true;
        var micLevel = hasMic ? (int)Math.Round(_monitor!.MicVolume * 100) : 0;

        if (_lastMicDeviceName != micName || _lastMicMuted != micMuted)
        {
            _lastMicDeviceName = micName;
            _lastMicMuted = micMuted;
            InputDeviceText.Text = micName;
            var inKey = micMuted ? "IconMicMute" : "IconMic";
            if (_lastInputIconKey != inKey)
            {
                _lastInputIconKey = inKey;
                InputIcon.Content = FindResource(inKey);
            }
        }

        if (_flyoutMicSlider is null || _flyoutMicLevelText is null || _flyoutMicMuteIcon is null) return;
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
            if (_lastHasDevice == false && _lastLanguage == lang) return;
            _lastHasDevice = false;
            _lastLanguage = lang;
            _lastLevel = -1;
            OutputDeviceText.Text = UiText.Get("NoAudioDevice");
            ComboButton.ToolTip = OutputDeviceText.Text;
            SetOutputIcon("IconVolumeMute");
            if (_flyoutSlider is not null) _flyoutSlider.IsEnabled = false;
            if (_flyoutLevelText is not null) _flyoutLevelText.Text = "—";
            if (_flyoutMuteIcon is not null) _flyoutMuteIcon.Content = FindResource("IconVolumeMute");
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

        OutputDeviceText.Text = ShortDeviceLabel(deviceName, UiText.Get("AudioOutput"));
        var micLabel = _monitor.HasMicDevice
            ? ShortDeviceLabel(_monitor.MicDeviceName, UiText.Get("Microphone"))
            : UiText.Get("NoMicDevice");
        InputDeviceText.Text = micLabel;
        ComboButton.ToolTip = $"{deviceName} ({(muted ? "0%" : UiText.Format("Percent", level))}) · {micLabel}";

        var iconKey = muted || level == 0
            ? "IconVolumeMute"
            : IsHeadphoneDevice(deviceName)
                ? "IconHeadphones"
                : level < 50 ? "IconVolumeLow" : "IconVolume";
        SetOutputIcon(iconKey);

        _updating = true;
        if (_flyoutSlider is not null)
        {
            _flyoutSlider.IsEnabled = true;
            _flyoutSlider.Value = muted ? 0 : level;
        }
        if (_flyoutLevelText is not null) _flyoutLevelText.Text = muted ? "0%" : UiText.Format("Percent", level);
        if (_flyoutMuteIcon is not null)
        {
            var flyoutIconKey = muted || level == 0 ? "IconVolumeMute" : level < 50 ? "IconVolumeLow" : "IconVolume";
            _flyoutMuteIcon.Content = FindResource(flyoutIconKey);
        }
        if (_flyoutMuteButton is not null) ApplyMuteButtonVisual(_flyoutMuteButton, muted);
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

    private void Card_MouseWheel(object sender, MouseWheelEventArgs e)
    {
        if (_monitor?.HasDevice != true) return;
        if (_monitor.SetVolume(Math.Clamp(_monitor.Volume + Math.Sign(e.Delta) * 0.02f, 0f, 1f))) Render();
        else Report(UiText.Get("ErrorVolumeChange"));
        e.Handled = true;
    }
}
