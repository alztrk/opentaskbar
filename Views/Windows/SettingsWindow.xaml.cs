using System.Collections.ObjectModel;
using System.ComponentModel;
using System.Windows.Data;
using System.Diagnostics;
using System.Globalization;
using System.Runtime.InteropServices;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Controls.Primitives;
using System.Windows.Input;
using System.Windows.Interop;
using System.Windows.Media;
using System.Windows.Media.Animation;
using System.Windows.Shell;
using System.Windows.Threading;
using OpenTaskbar.Models;
using OpenTaskbar.Services;
using OpenTaskbar.Widgets;

namespace OpenTaskbar;

public partial class SettingsWindow : Window
{
    private readonly ApplicationPreferences _preferences;
    private readonly StartupRegistration _startup = new();
    private readonly ObservableCollection<DockWidgetOption> _widgetOptions = [];
    private readonly ObservableCollection<WidgetCategoryGroup> _widgetGroups = [];
    private readonly Dictionary<string, ComboBox> _widgetDensityBoxes = [];
    private readonly List<ComboBox> _aiRingBoxes = [];
    private readonly DockWidgetService _widgetPreviewService = new(TimeProvider.System);
    private readonly DispatcherTimer _widgetPreviewTimer = new() { Interval = TimeSpan.FromSeconds(15) };
    private readonly List<OpenTaskbar.Widgets.DockWidget> _livePreviewWidgets = [];
    private DispatcherTimer? _livePreviewTimer;
    private readonly DispatcherTimer _autoSaveTimer = new() { Interval = TimeSpan.FromMilliseconds(250) };
    private bool _startupEnabled;
    private bool _startupAvailable;
    private DockSettings _applied;
    private string? _selectedThemePackId;
    private bool _closingApplication;
    private bool _initializing = true;
    private AiUsageRing _aiUsageRing;
    private bool _closed;
    private bool _sourceReady;
    private HwndSource? _source;
    private readonly string _radioGroupScope = Guid.NewGuid().ToString("N");
    private readonly RoutedEventHandler _preferenceEditedHandler;
    private readonly Dictionary<SurfaceKind, Slider> _surfaceSliders = [];
    private readonly Dictionary<SurfaceKind, TextBlock> _surfaceValues = [];
    private readonly Dictionary<SurfaceKind, ComboBox> _surfaceBoxes = [];

    internal bool IsClosingOrClosed => _isClosing || _closed;

    public SettingsWindow(ApplicationPreferences preferences)
    {
        _preferences = preferences;
        _applied = preferences.Current;
        _preferenceEditedHandler = PreferenceEdited;
        InitializeComponent();
        var currentVersion = UpdateChecker.CurrentVersion;
        var versionString = $"v{currentVersion.Major}.{currentVersion.Minor}.{currentVersion.Build}";
        TitleVersionText.Text = versionString;
        AppVersionText.Text = versionString;
        ScopeRadioGroups(this);
        RegisterSurface(SurfaceKind.Dock, DockOpacitySlider, DockOpacityValue, DockSurfaceBox);
        RegisterSurface(SurfaceKind.Menu, MenuOpacitySlider, MenuOpacityValue, MenuSurfaceBox);
        RegisterSurface(SurfaceKind.Widget, WidgetOpacitySlider, WidgetOpacityValue, WidgetSurfaceBox);
        RegisterSurface(SurfaceKind.Settings, SettingsOpacitySlider, SettingsOpacityValue, SettingsSurfaceBox);
        foreach (var option in _widgetPreviewService.CreateOptions(_applied.InstalledWidgets, _applied.Widgets))
        {
            _widgetOptions.Add(option);
        }
        AttachWidgetSettingsPanels();
        // Catalog previews are the very same widget classes the dock runs (activated + ticked),
        // so the cards reflect the real taskbar widgets instead of hand-authored mock tiles.
        AttachLiveWidgetPreviews();
        CustomThemeEditor.ThemeSaved += (_, _) => ReloadThemePacks(_selectedThemePackId);
        CustomThemeEditor.ThemeCreated += (_, id) => ReloadThemePacks(id);
        var widgetView = new ListCollectionView(_widgetOptions);
        widgetView.SortDescriptions.Add(new SortDescription(nameof(DockWidgetOption.CategoryOrder), ListSortDirection.Ascending));
        widgetView.GroupDescriptions.Add(new PropertyGroupDescription(nameof(DockWidgetOption.Category)));
        WidgetOptionsList.ItemsSource = widgetView;
        WeatherCountryBox.ItemsSource = CountryCityService.GetInitialCountries().Select(c => c.Name).ToList();
        _ = InitializeWeatherLocationsAsync(_applied.WeatherCity);
        _preferences.Changed += Preferences_Changed;
        _widgetPreviewTimer.Tick += WidgetPreviewTimer_Tick;
        _autoSaveTimer.Tick += (_, _) =>
        {
            _autoSaveTimer.Stop();
            SaveSettings();
        };
        IsVisibleChanged += Settings_IsVisibleChanged;
        Closed += (_, _) =>
        {
            _closed = true;
            RemoveHandler(ToggleButton.CheckedEvent, _preferenceEditedHandler);
            RemoveHandler(ToggleButton.UncheckedEvent, _preferenceEditedHandler);
            _autoSaveTimer.Stop();
            _widgetPreviewTimer.Stop();
            _widgetPreviewTimer.Tick -= WidgetPreviewTimer_Tick;
            StopAndDisposeLivePreviews();
            IsVisibleChanged -= Settings_IsVisibleChanged;
            _preferences.Changed -= Preferences_Changed;
            if (_source is { IsDisposed: false }) _source.RemoveHook(WindowProc);
            WidgetOptionsList.ItemsSource = null;
            foreach (var option in _widgetOptions)
                if (option.NativePreview is OpenTaskbar.Widgets.DockWidget preview) preview.Dispose();
            _widgetOptions.Clear();
            _widgetGroups.Clear();
            _widgetDensityBoxes.Clear();
            _aiRingBoxes.Clear();
            WeatherCountryBox.ItemsSource = null;
            ThemeManager.Apply(_preferences.Current.Theme, _preferences.Current);
            (Application.Current.MainWindow as MainWindow)?.SetSizePreview(null, null);
            (Application.Current.MainWindow as MainWindow)?.SetGeometryPreview(null);
        };
        SelectChoice(ThemeBox, _applied.Theme.ToString());
        LanguageBox.ItemsSource = UiText.Languages;
        SelectLanguage(_applied.Language);
        SelectAlignment(_applied);
        AnimationsToggle.IsChecked = _applied.AnimationsEnabled;
        PreviewsToggle.IsChecked = _applied.PreviewsEnabled;
        SelectChoice(AutoHideBox, _applied.AutoHide.ToString());
        LoadSurfaceStyles(_applied);
        MagnificationToggle.IsChecked = _applied.MagnificationEnabled;
        SelectChoice(SizeBox, _applied.Size.ToString());
        DockScaleSlider.Value = Math.Round(_applied.DockScale * 100);
        UpdateDockScaleLabel();
        SelectChoice(DockWidthModeBox, _applied.DockWidthMode.ToString());
        UpdateDockWidthControls();
        LoadDockGapControls(_applied);
        var initialProfile = DockMetrics.For(_applied.Size, _applied.DockScale);
        CornerRadiusSlider.Value = _applied.DockCornerRadius ?? (int)Math.Round(initialProfile.Radius);
        UpdateCornerRadiusLabel();
        FocusMinutesSlider.Value = _applied.FocusMinutes;
        BreakMinutesSlider.Value = _applied.FocusBreakMinutes;
        FocusSessionsSlider.Value = _applied.FocusSessions;
        LoadFeatureSelection(_applied);
        LoadThemePacks(_applied.ThemePack);
        UpdateWidgetOptionState();
        var startup = _startup.Read();
        _startupEnabled = startup.Enabled;
        _startupAvailable = startup.Error is null;
        StartupToggle.IsEnabled = _startupAvailable;
        if (startup.Error is not null)
        {
            StartupToggle.ToolTip = UiText.Get(startup.Error);
            System.Windows.Controls.ToolTipService.SetShowOnDisabled(StartupToggle, true);
        }
        StartupToggle.IsChecked = _startupEnabled;
        var errors = new[] { preferences.LoadError, startup.Error }.OfType<string>();
        var message = string.Join(" ", errors.Select(UiText.Get));
        if (message.Length > 0) Trace.TraceWarning("Settings initialization warning: {0}", message);
        var workArea = SystemParameters.WorkArea;
        MinHeight = Math.Min(MinHeight, workArea.Height);
        MinWidth = Math.Min(MinWidth, workArea.Width);
        Height = Math.Min(Height, workArea.Height);
        Width = Math.Min(Width, workArea.Width);
        AddHandler(ToggleButton.CheckedEvent, _preferenceEditedHandler);
        AddHandler(ToggleButton.UncheckedEvent, _preferenceEditedHandler);
        _initializing = false;
    }

    // Segment controls are now ComboBoxes whose items carry the enum name in Tag. These helpers keep the
    // read/write sites terse and avoid sprinkling SelectedItem casts across initialization and save paths.
    private static string? Choice(ComboBox box) =>
        (box.SelectedItem as ComboBoxItem)?.Tag as string;

    private static void SelectChoice(ComboBox box, string? tag)
    {
        foreach (var item in box.Items.OfType<ComboBoxItem>())
            if (string.Equals(item.Tag as string, tag, StringComparison.Ordinal))
            {
                box.SelectedItem = item;
                return;
            }
        box.SelectedIndex = -1;
    }

    // Scope RadioButton.GroupName per window instance so closing one SettingsWindow cannot uncheck
    // another instance's radio buttons via WPF's global group registry.
    private void ScopeRadioGroups(DependencyObject root)
    {
        if (root is Visual)
        {
            for (var i = 0; i < VisualTreeHelper.GetChildrenCount(root); i++)
            {
                var child = VisualTreeHelper.GetChild(root, i);
                if (child is RadioButton radio && !string.IsNullOrEmpty(radio.GroupName)
                    && !radio.GroupName.EndsWith(_radioGroupScope, StringComparison.Ordinal))
                {
                    radio.GroupName = $"{radio.GroupName}_{_radioGroupScope}";
                }
                ScopeRadioGroups(child);
            }
        }
        if (root is FrameworkElement fe)
        {
            foreach (var obj in LogicalTreeHelper.GetChildren(fe))
            {
                if (obj is DependencyObject dep && !IsVisualChild(fe, dep))
                {
                    if (dep is RadioButton radio && !string.IsNullOrEmpty(radio.GroupName)
                        && !radio.GroupName.EndsWith(_radioGroupScope, StringComparison.Ordinal))
                    {
                        radio.GroupName = $"{radio.GroupName}_{_radioGroupScope}";
                    }
                    ScopeRadioGroups(dep);
                }
            }
        }

        static bool IsVisualChild(DependencyObject parent, DependencyObject candidate) =>
            candidate is Visual && VisualTreeHelper.GetParent(candidate) == parent;
    }

    protected override void OnSourceInitialized(EventArgs e)
    {
        base.OnSourceInitialized(e);
        _source = HwndSource.FromHwnd(new WindowInteropHelper(this).Handle);
        _source?.AddHook(WindowProc);
        _sourceReady = true;
        UpdateBackdrop();
        if (_preferences.Current.AnimationsEnabled)
        {
            PlayOpenAnimation();
        }
    }

    private void PlayOpenAnimation()
    {
        var fade = new System.Windows.Media.Animation.DoubleAnimation(0.0, 1.0, TimeSpan.FromMilliseconds(180))
        {
            EasingFunction = new System.Windows.Media.Animation.QuadraticEase { EasingMode = System.Windows.Media.Animation.EasingMode.EaseOut }
        };
        var scaleAnim = new System.Windows.Media.Animation.DoubleAnimation(0.96, 1.0, TimeSpan.FromMilliseconds(180))
        {
            EasingFunction = new System.Windows.Media.Animation.QuadraticEase { EasingMode = System.Windows.Media.Animation.EasingMode.EaseOut }
        };
        var transform = new ScaleTransform(0.96, 0.96);
        SettingsFrame.RenderTransformOrigin = new Point(0.5, 0.5);
        SettingsFrame.RenderTransform = transform;
        SettingsFrame.BeginAnimation(UIElement.OpacityProperty, fade);
        transform.BeginAnimation(ScaleTransform.ScaleXProperty, scaleAnim);
        transform.BeginAnimation(ScaleTransform.ScaleYProperty, scaleAnim);
    }

    private nint WindowProc(nint window, int message, nint wParam, nint lParam, ref bool handled)
    {
        if (message != 0x0024 || lParam == 0) return 0; // WM_GETMINMAXINFO
        var monitor = MonitorFromWindow(window, 2);
        var info = new MonitorInfo { Size = Marshal.SizeOf<MonitorInfo>() };
        if (monitor == 0 || !GetMonitorInfo(monitor, ref info))
        {
            Trace.TraceWarning("Settings monitor work area unavailable: {0}", Marshal.GetLastWin32Error());
            return 0;
        }
        var sizing = Marshal.PtrToStructure<MinMaxInfo>(lParam);
        sizing.MaxPosition = new NativePoint(info.Work.Left - info.Monitor.Left, info.Work.Top - info.Monitor.Top);
        sizing.MaxSize = new NativePoint(info.Work.Right - info.Work.Left, info.Work.Bottom - info.Work.Top);
        var dpi = GetDpiForWindow(window);
        var scale = dpi > 0 ? dpi / 96d : 1;
        sizing.MinTrackSize = new NativePoint((int)Math.Ceiling(MinWidth * scale), (int)Math.Ceiling(MinHeight * scale));
        if (MaxWidth > 0 && !double.IsInfinity(MaxWidth) && !double.IsNaN(MaxWidth))
            sizing.MaxTrackSize = new NativePoint((int)Math.Ceiling(MaxWidth * scale), (int)Math.Ceiling(MaxHeight * scale));
        Marshal.StructureToPtr(sizing, lParam, false);
        handled = true;
        return 0;
    }

    private void Preferences_Changed(object? sender, EventArgs e)
    {
        if (_applied.Language != _preferences.Current.Language)
        {
            var localizedOptions = _widgetPreviewService.CreateOptions([], []).ToDictionary(option => option.Id, StringComparer.Ordinal);
            foreach (var option in _widgetOptions)
            {
                var localized = localizedOptions[option.Id];
                option.UpdateText(localized.Label, localized.Description);
                option.RefreshInstallLabels();
            }
            RefreshWidgetPreviews();
            foreach (var group in _widgetGroups) group.RefreshLabel();
        }
        ThemeManager.Apply(_preferences.Current.Theme, _preferences.Current);
        LoadSurfaceStyles(_preferences.Current);
        ApplySettingsToLivePreviews(_preferences.Current);
        foreach (var option in _widgetOptions)
        {
            if (option.NativePreview is OpenTaskbar.Widgets.DockWidget native
                && !_livePreviewWidgets.Contains(native))
            {
                native.ApplySettings(_preferences.Current);
                native.Visibility = Visibility.Visible;
            }
        }
        CustomThemeEditor.LoadTheme(_selectedThemePackId, ThemeManager.EffectiveTheme);
        BuildThemeGallery(_selectedThemePackId);
        UpdateDockScaleLabel();
        UpdateWidgetOptionState();
        UpdateBackdrop();
    }

    private void UpdateBackdrop()
    {
        if (!_sourceReady) return;
        var chrome = WindowChrome.GetWindowChrome(this);
        // A native WindowChrome window cannot paint a transparent client area without AllowsTransparency,
        // which cannot be switched on after the HWND exists. "None" therefore drops the glass and the DWM
        // acrylic and falls back to the same opaque surface used on unsupported systems.
        if (ThemeManager.GetSurface(SurfaceKind.Settings).Background == SurfaceBackground.None)
        {
            SettingsBackdrop.Disable(this);
            SettingsFrame.SetResourceReference(System.Windows.Controls.Border.BackgroundProperty, "SettingsBackground");
            chrome.GlassFrameThickness = new Thickness(0);
            SettingsFrame.CornerRadius = new CornerRadius(0);
            return;
        }
        var glass = SettingsBackdrop.TryEnable(this, ThemeManager.EffectiveTheme);
        SettingsFrame.SetResourceReference(System.Windows.Controls.Border.BackgroundProperty, glass ? "SettingsGlassMaterial" : "SettingsBackground");
        if (glass)
        {
            WindowChrome.GetWindowChrome(this).GlassFrameThickness = new Thickness(-1);
            SettingsFrame.ClearValue(System.Windows.Controls.Border.CornerRadiusProperty);
        }
        else
        {
            WindowChrome.GetWindowChrome(this).GlassFrameThickness = new Thickness(0);
            SettingsFrame.CornerRadius = new CornerRadius(0);
        }
    }

    private DockSettings ReadSelection()
    {
        var enabledWidgets = _widgetOptions.Where(option => option.IsInstalled && option.IsEnabled).Select(option => option.Id).ToArray();
        var installedWidgets = _widgetOptions.Where(option => option.IsInstalled).Select(option => option.Id).ToArray();
        bool IsEnabled(string id) => enabledWidgets.Contains(id, StringComparer.OrdinalIgnoreCase);
        return _preferences.Current with
        {
            Theme = Enum.TryParse<AppTheme>(Choice(ThemeBox), out var theme) ? theme : AppTheme.Dark,
            Language = LanguageBox.SelectedItem is AppLanguage language ? language : _preferences.Current.Language,
            // Full width keeps the last floating alignment so switching back restores it.
            Position = Choice(PositionBox) is { } alignment && alignment != "FullWidth" && Enum.TryParse<DockPosition>(alignment, out var position)
                ? position : _preferences.Current.Position,
            AnimationsEnabled = AnimationsToggle.IsChecked == true,
            PreviewsEnabled = PreviewsToggle.IsChecked == true,
            NotificationBadgesEnabled = true,
            AutoHide = Enum.TryParse<AutoHideMode>(Choice(AutoHideBox), out var autoHide) ? autoHide : AutoHideMode.Disabled,
            GlobalWidgetDensity = Enum.TryParse<WidgetDensityMode>(Choice(DensityBox), out var density) ? density : WidgetDensityMode.Auto,
            WidgetDensityOverrides = ReadWidgetDensityOverrides(),
            DockBackground = SurfaceBackgroundOf(SurfaceKind.Dock),
            DockOpacity = Math.Round(_surfaceSliders[SurfaceKind.Dock].Value) / 100,
            MenuBackground = SurfaceBackgroundOf(SurfaceKind.Menu),
            MenuOpacity = Math.Round(_surfaceSliders[SurfaceKind.Menu].Value) / 100,
            WidgetBackground = SurfaceBackgroundOf(SurfaceKind.Widget),
            WidgetOpacity = Math.Round(_surfaceSliders[SurfaceKind.Widget].Value) / 100,
            SettingsBackground = SurfaceBackgroundOf(SurfaceKind.Settings),
            SettingsOpacity = Math.Round(_surfaceSliders[SurfaceKind.Settings].Value) / 100,
            Widgets = enabledWidgets,
            InstalledWidgets = installedWidgets,
            MagnificationEnabled = MagnificationToggle.IsChecked == true,
            Layout = Choice(PositionBox) == "FullWidth" ? DockLayout.FullWidth : DockLayout.Floating,
            Size = Enum.TryParse<DockSize>(Choice(SizeBox), out var size) ? size : DockSize.Compact,
            DockScale = Math.Round(DockScaleSlider.Value) / 100,
            DockWidthMode = Enum.TryParse<DockWidthMode>(Choice(DockWidthModeBox), out var widthMode) ? widthMode : DockWidthMode.Auto,
            DockWidthPixels = widthMode == DockWidthMode.Pixels ? (int)Math.Round(DockWidthSlider.Value) : _preferences.Current.DockWidthPixels,
            DockWidthPercent = widthMode == DockWidthMode.Percent ? (int)Math.Round(DockWidthSlider.Value) : _preferences.Current.DockWidthPercent,
            DockTopGapUnit = GapUnit(DockTopGapUnitBox),
            DockTopGapPixels = (int)Math.Round(DockTopGapSlider.Value),
            DockTopGapPercent = (int)Math.Round(DockTopGapSlider.Value),
            DockBottomGapUnit = GapUnit(DockBottomGapUnitBox),
            DockBottomGapPixels = (int)Math.Round(DockBottomGapSlider.Value),
            DockBottomGapPercent = (int)Math.Round(DockBottomGapSlider.Value),
            DockCornerRadius = (int)Math.Round(CornerRadiusSlider.Value),
            ShowClock = IsEnabled("Clock"),
            ShowMedia = IsEnabled("Media"),
            ShowSystemStats = IsEnabled("SystemStats"),
            ShowAudio = IsEnabled("Audio"),
            ShowQuickSettings = IsEnabled("QuickSettings"),
            ShowFocus = IsEnabled("Focus"),
            FocusMinutes = (int)Math.Round(FocusMinutesSlider.Value),
            FocusBreakMinutes = (int)Math.Round(BreakMinutesSlider.Value),
            FocusSessions = (int)Math.Round(FocusSessionsSlider.Value),
            ShowRecycleBin = RecycleBinToggle.IsChecked == true,
            ShowSearchButton = SearchButtonToggle.IsChecked == true,
            ShowTaskViewButton = TaskViewButtonToggle.IsChecked == true,
            ShowTrayButton = TrayButtonToggle.IsChecked == true,
            ShowDesktopButton = ShowDesktopButtonToggle.IsChecked == true,
            TargetDisplay = (TargetDisplayBox.SelectedItem as DisplayChoice)?.DeviceName,
            ShowOnAllMonitors = AllMonitorsToggle.IsChecked == true,
            SecondaryApps = (SecondaryAppsBox.SelectedItem as SecondaryAppChoice)?.Scope ?? SecondaryAppScope.CurrentMonitorOnly,
            SecondaryWidgets = SecondaryWidgetsToggle.IsChecked == true,
            CurrentDesktopOnly = CurrentDesktopToggle.IsChecked == true,
            CheckUpdates = UpdatesToggle.IsChecked == true,
            Edge = Enum.TryParse<DockEdge>(Choice(EdgeBox), out var edge) ? edge : DockEdge.Bottom,
            ShowWeather = IsEnabled("Weather"),
            WeatherCity = WeatherCityBox.Text.Trim(),
            LauncherHotkey = _preferences.Current.LauncherHotkey,
            DockHotkey = Choice(HotkeyBox) == "CtrlAltD" ? HotkeyChoice.CtrlAltD : HotkeyChoice.Disabled,
            StartAction = StartButtonAction.WindowsStart,
            StartPlacement = Enum.TryParse<StartButtonPlacement>(Choice(StartPlacementBox), out var placement)
                ? placement : _preferences.Current.StartPlacement,
            StartWidgetIndex = _preferences.Current.StartWidgetIndex,
            WheelVolume = WheelVolumeToggle.IsChecked == true,
            Clock24Hour = Choice(ClockFormatBox) != "12",
            ClockShowSeconds = ClockSecondsToggle.IsChecked == true,
            DateStyle = Enum.TryParse<ClockDateStyle>(Choice(DateStyleBox), out var dateStyle) ? dateStyle : ClockDateStyle.DayAndShortDate,
            StatsMeters = ReadStatsMeters(),
            AiUsageRing = _aiUsageRing,
            ThemePack = _selectedThemePackId,
            ThemeApplicationIcons = false
        };
    }

    private sealed record ThemePackChoice(string? Id, string Name, string Details, ThemePack? Pack = null);
    private sealed record DisplayChoice(string? DeviceName, string DisplayName);
    private sealed record SecondaryAppChoice(SecondaryAppScope Scope, string DisplayName);

    private void LoadDisplayChoices(string? selectedDevice)
    {
        var primaryLabel = UiText.Get("TargetDisplayPrimary");
        var choices = new List<DisplayChoice> { new(null, primaryLabel) };
        try
        {
            var displays = DisplayService.GetDisplays();
            foreach (var d in displays)
            {
                var suffix = d.IsPrimary ? $" ({primaryLabel})" : "";
                var label = $"{d.Index}: {d.Bounds.Width:0}x{d.Bounds.Height:0}{suffix}";
                choices.Add(new DisplayChoice(d.DeviceName, label));
            }
        }
        catch (Exception ex)
        {
            AppLog.ReportError("ErrorDisplayDimensions", ex);
        }

        TargetDisplayBox.ItemsSource = choices;
        TargetDisplayBox.SelectedItem = !string.IsNullOrWhiteSpace(selectedDevice)
            ? choices.FirstOrDefault(c => string.Equals(c.DeviceName, selectedDevice, StringComparison.OrdinalIgnoreCase)) ?? choices[0]
            : choices[0];
    }

    private void LoadSecondaryAppChoices(SecondaryAppScope selectedScope)
    {
        var choices = new List<SecondaryAppChoice>
        {
            new(SecondaryAppScope.CurrentMonitorOnly, UiText.Get("SecondaryAppsCurrent")),
            new(SecondaryAppScope.AllMonitors, UiText.Get("SecondaryAppsAll"))
        };
        SecondaryAppsBox.ItemsSource = choices;
        SecondaryAppsBox.SelectedItem = choices.FirstOrDefault(c => c.Scope == selectedScope) ?? choices[0];
    }

    private void UpdateMultiMonitorControls()
    {
        var allMonitors = AllMonitorsToggle.IsChecked == true;
        SecondaryAppsRow.IsEnabled = allMonitors;
        SecondaryWidgetsRow.IsEnabled = allMonitors;
    }

    private void TargetDisplayBox_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (_initializing) return;
        PreferenceEdited(sender, e);
    }

    private void SecondaryAppsBox_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (_initializing) return;
        PreferenceEdited(sender, e);
    }

    private void SelectLanguage(AppLanguage language)
    {
        if (LanguageBox.ItemsSource is IEnumerable<AppLanguage> languages)
        {
            var match = languages.FirstOrDefault(cand => string.Equals(cand.CultureName, language.CultureName, StringComparison.OrdinalIgnoreCase));
            if (!string.IsNullOrEmpty(match.CultureName) || match == AppLanguage.English)
            {
                LanguageBox.SelectedItem = match;
            }
        }
        if (LanguageBox.SelectedIndex < 0 && LanguageBox.Items.Count > 0) LanguageBox.SelectedIndex = 0;
    }

    private void LanguageBox_SelectionChanged(object sender, SelectionChangedEventArgs e) => PreferenceEdited(sender, e);

    private void LoadThemePacks(string? selected)
    {
        _selectedThemePackId = string.IsNullOrWhiteSpace(selected) ? null : selected;
        BuildThemeGallery(_selectedThemePackId);
        CustomThemeEditor.LoadTheme(_selectedThemePackId, ThemeManager.EffectiveTheme);
    }

    private void BuildThemeGallery(string? selected)
    {
        var discovered = ThemePacks.Discover();
        var builtIn = new List<ThemePackChoice> { new(null, UiText.Get("ThemePackNone"), string.Empty) };
        var custom = new List<ThemePackChoice>();

        foreach (var pack in discovered)
        {
            var choice = new ThemePackChoice(pack.Id, pack.DisplayName, DescribeThemePack(pack), pack);
            if (ThemePack.IsBuiltInTheme(pack.Id)) builtIn.Add(choice);
            else custom.Add(choice);
        }

        // A stored theme whose folder is gone stays listed so an unrelated edit does not silently clear it.
        if (!string.IsNullOrWhiteSpace(selected)
            && !builtIn.Any(c => string.Equals(c.Id, selected, StringComparison.OrdinalIgnoreCase))
            && !custom.Any(c => string.Equals(c.Id, selected, StringComparison.OrdinalIgnoreCase)))
        {
            custom.Add(new ThemePackChoice(selected, selected + " (?)", UiText.Get("ThemePackMissing")));
        }

        ThemeGalleryBuiltIn.Children.Clear();
        foreach (var choice in builtIn)
            ThemeGalleryBuiltIn.Children.Add(CreateThemeEntryButton(choice, string.Equals(choice.Id, selected, StringComparison.OrdinalIgnoreCase)));

        ThemeGalleryCustom.Children.Clear();
        foreach (var choice in custom)
            ThemeGalleryCustom.Children.Add(CreateThemeEntryButton(choice, string.Equals(choice.Id, selected, StringComparison.OrdinalIgnoreCase)));

        ThemeGalleryCustomEmpty.Visibility = custom.Count == 0 ? Visibility.Visible : Visibility.Collapsed;

        var activeChoice = builtIn.Concat(custom).FirstOrDefault(c => string.Equals(c.Id, selected, StringComparison.OrdinalIgnoreCase)) ?? builtIn[0];
        ThemePackDetails.Text = activeChoice.Details;
        ThemePackDetails.Visibility = activeChoice.Details.Length == 0 ? Visibility.Collapsed : Visibility.Visible;

        var hasSelectedPack = !string.IsNullOrWhiteSpace(selected);
        ThemePackDuplicateButton.IsEnabled = hasSelectedPack;
        ThemePackExportButton.IsEnabled = hasSelectedPack;
        ThemePackDeleteButton.IsEnabled = hasSelectedPack && !ThemePack.IsBuiltInTheme(selected);
    }

    private Button CreateThemeEntryButton(ThemePackChoice choice, bool isActive)
    {
        var (bgColor, dockColor, accentColor, textColor) = ResolveThemePreviewColors(choice.Pack);

        // Miniature desktop backdrop with a floating dock pill previewing the theme's real dock surface and accent.
        var backdropBrush = new LinearGradientBrush
        {
            StartPoint = new Point(0, 0),
            EndPoint = new Point(1, 1),
            GradientStops = new GradientStopCollection
            {
                new GradientStop(BlendColors(bgColor, accentColor, 0.14), 0),
                new GradientStop(bgColor, 0.65),
                new GradientStop(BlendColors(bgColor, Colors.Black, ThemeManager.EffectiveTheme == AppTheme.Dark ? 0.25 : 0.06), 1)
            }
        };

        var previewStage = new Border
        {
            Height = 76,
            Background = backdropBrush,
            CornerRadius = new CornerRadius(8, 8, 0, 0),
            ClipToBounds = true
        };

        // Subtle miniature app tiles + active accent dot + mini clock inside a floating dock pill.
        var dockItems = new StackPanel
        {
            Orientation = Orientation.Horizontal,
            VerticalAlignment = VerticalAlignment.Center,
            HorizontalAlignment = HorizontalAlignment.Center
        };

        // Start / accent tile
        dockItems.Children.Add(new Border
        {
            Width = 10,
            Height = 10,
            CornerRadius = new CornerRadius(3),
            Margin = new Thickness(0, 0, 5, 0),
            Background = new SolidColorBrush(Color.FromArgb(0xCC, accentColor.R, accentColor.G, accentColor.B))
        });

        // Active app tile with accent indicator dot underneath
        var activeAppStack = new StackPanel
        {
            Margin = new Thickness(0, 1, 5, 0),
            VerticalAlignment = VerticalAlignment.Center
        };
        activeAppStack.Children.Add(new Border
        {
            Width = 10,
            Height = 10,
            CornerRadius = new CornerRadius(3),
            Background = new SolidColorBrush(Color.FromArgb(0x3D, textColor.R, textColor.G, textColor.B))
        });
        activeAppStack.Children.Add(new Border
        {
            Width = 3,
            Height = 2,
            CornerRadius = new CornerRadius(1),
            Margin = new Thickness(0, 1.5, 0, 0),
            HorizontalAlignment = HorizontalAlignment.Center,
            Background = new SolidColorBrush(accentColor)
        });
        dockItems.Children.Add(activeAppStack);

        // Two resting app tiles
        for (var i = 0; i < 2; i++)
        {
            dockItems.Children.Add(new Border
            {
                Width = 10,
                Height = 10,
                CornerRadius = new CornerRadius(3),
                Margin = new Thickness(0, 0, 5, 0),
                Background = new SolidColorBrush(Color.FromArgb(0x28, textColor.R, textColor.G, textColor.B))
            });
        }

        // Divider + mini clock widget placeholder
        dockItems.Children.Add(new Border
        {
            Width = 1,
            Height = 11,
            Margin = new Thickness(1, 0, 6, 0),
            Background = new SolidColorBrush(Color.FromArgb(0x24, textColor.R, textColor.G, textColor.B))
        });
        var miniClock = new StackPanel { VerticalAlignment = VerticalAlignment.Center };
        miniClock.Children.Add(new Border
        {
            Width = 18,
            Height = 3.5,
            CornerRadius = new CornerRadius(1.5),
            Background = new SolidColorBrush(Color.FromArgb(0x99, textColor.R, textColor.G, textColor.B))
        });
        miniClock.Children.Add(new Border
        {
            Width = 12,
            Height = 2.5,
            CornerRadius = new CornerRadius(1),
            Margin = new Thickness(0, 2, 0, 0),
            HorizontalAlignment = HorizontalAlignment.Left,
            Background = new SolidColorBrush(Color.FromArgb(0x4D, textColor.R, textColor.G, textColor.B))
        });
        dockItems.Children.Add(miniClock);

        var dockPill = new Border
        {
            Height = 26,
            CornerRadius = new CornerRadius(7),
            Padding = new Thickness(8, 0, 8, 0),
            Margin = new Thickness(10, 0, 10, 10),
            VerticalAlignment = VerticalAlignment.Bottom,
            HorizontalAlignment = HorizontalAlignment.Center,
            Background = new SolidColorBrush(Color.FromArgb(0xEB, dockColor.R, dockColor.G, dockColor.B)),
            BorderBrush = new SolidColorBrush(Color.FromArgb(0x44, accentColor.R, accentColor.G, accentColor.B)),
            BorderThickness = new Thickness(1),
            Child = dockItems
        };
        previewStage.Child = dockPill;

        // Footer: left-aligned theme name + right-aligned active checkmark badge or palette dots.
        var footer = new Grid { Margin = new Thickness(10, 8, 10, 9) };
        footer.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        footer.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });

        var nameText = new TextBlock
        {
            Text = choice.Name,
            FontSize = 12,
            FontWeight = isActive ? FontWeights.SemiBold : FontWeights.Medium,
            TextTrimming = TextTrimming.CharacterEllipsis,
            VerticalAlignment = VerticalAlignment.Center,
            ToolTip = choice.Name
        };
        nameText.SetResourceReference(TextBlock.ForegroundProperty, "TextPrimary");
        Grid.SetColumn(nameText, 0);
        footer.Children.Add(nameText);

        if (isActive)
        {
            var checkIcon = new ContentControl
            {
                Width = 10,
                Height = 10,
                HorizontalAlignment = HorizontalAlignment.Center,
                VerticalAlignment = VerticalAlignment.Center,
                Content = Application.Current.TryFindResource("IconCheck")
            };
            checkIcon.SetResourceReference(StyleProperty, "WidgetIcon");
            checkIcon.SetResourceReference(Control.ForegroundProperty, "AccentText");

            var badge = new Border
            {
                Width = 18,
                Height = 18,
                CornerRadius = new CornerRadius(9),
                Margin = new Thickness(8, 0, 0, 0),
                VerticalAlignment = VerticalAlignment.Center,
                Child = checkIcon
            };
            badge.SetResourceReference(Border.BackgroundProperty, "AccentBrush");
            Grid.SetColumn(badge, 1);
            footer.Children.Add(badge);
        }
        else
        {
            var dots = new StackPanel
            {
                Orientation = Orientation.Horizontal,
                VerticalAlignment = VerticalAlignment.Center,
                Margin = new Thickness(8, 0, 0, 0)
            };
            dots.Children.Add(new Border
            {
                Width = 8,
                Height = 8,
                CornerRadius = new CornerRadius(4),
                Margin = new Thickness(0, 0, 3, 0),
                Background = new SolidColorBrush(dockColor),
                BorderBrush = new SolidColorBrush(Color.FromArgb(0x40, textColor.R, textColor.G, textColor.B)),
                BorderThickness = new Thickness(1)
            });
            dots.Children.Add(new Border
            {
                Width = 8,
                Height = 8,
                CornerRadius = new CornerRadius(4),
                Background = new SolidColorBrush(accentColor)
            });
            Grid.SetColumn(dots, 1);
            footer.Children.Add(dots);
        }

        var divider = new Border { Height = 1 };
        divider.SetResourceReference(Border.BackgroundProperty, "StrokeSubtle");

        var cardBody = new StackPanel { HorizontalAlignment = HorizontalAlignment.Stretch };
        cardBody.Children.Add(previewStage);
        cardBody.Children.Add(divider);
        cardBody.Children.Add(footer);

        var cardBorder = new Border
        {
            Child = cardBody,
            CornerRadius = new CornerRadius(9),
            BorderThickness = new Thickness(isActive ? 1.5 : 1),
            ClipToBounds = true,
            SnapsToDevicePixels = true
        };
        cardBorder.SetResourceReference(Border.BackgroundProperty, "SettingsControl");
        cardBorder.SetResourceReference(Border.BorderBrushProperty, isActive ? "AccentBrush" : "StrokeSubtle");

        var btn = new Button
        {
            Content = cardBorder,
            Padding = new Thickness(0),
            Margin = new Thickness(5, 0, 5, 10),
            HorizontalAlignment = HorizontalAlignment.Stretch,
            HorizontalContentAlignment = HorizontalAlignment.Stretch,
            VerticalContentAlignment = VerticalAlignment.Top,
            Cursor = Cursors.Hand,
            Background = Brushes.Transparent,
            BorderThickness = new Thickness(0),
            FocusVisualStyle = null
        };
        btn.Template = CreateThemeCardButtonTemplate();
        System.Windows.Automation.AutomationProperties.SetName(btn, choice.Name);
        btn.Click += (_, e) =>
        {
            if (string.Equals(_selectedThemePackId, choice.Id, StringComparison.OrdinalIgnoreCase)) return;
            _selectedThemePackId = choice.Id;
            ThemePacks.Select(_selectedThemePackId, reload: true);
            ThemePacks.ApplyWidgets(_preferences.Current.Theme);
            BuildThemeGallery(_selectedThemePackId);
            CustomThemeEditor.LoadTheme(_selectedThemePackId, ThemeManager.EffectiveTheme);
            PreferenceEdited(btn, e);
        };
        return btn;
    }

    private static ControlTemplate CreateThemeCardButtonTemplate()
    {
        var template = new ControlTemplate(typeof(Button));
        var grid = new FrameworkElementFactory(typeof(Grid));
        var presenter = new FrameworkElementFactory(typeof(ContentPresenter));
        presenter.SetValue(ContentPresenter.HorizontalAlignmentProperty, HorizontalAlignment.Stretch);
        presenter.SetValue(ContentPresenter.VerticalAlignmentProperty, VerticalAlignment.Stretch);

        var hoverOverlay = new FrameworkElementFactory(typeof(Border));
        hoverOverlay.Name = "HoverOverlay";
        hoverOverlay.SetValue(Border.CornerRadiusProperty, new CornerRadius(9));
        hoverOverlay.SetResourceReference(Border.BackgroundProperty, "SurfaceHover");
        hoverOverlay.SetValue(UIElement.OpacityProperty, 0.0);
        hoverOverlay.SetValue(UIElement.IsHitTestVisibleProperty, false);

        grid.AppendChild(presenter);
        grid.AppendChild(hoverOverlay);
        template.VisualTree = grid;

        var hoverTrigger = new Trigger { Property = UIElement.IsMouseOverProperty, Value = true };
        hoverTrigger.Setters.Add(new Setter(UIElement.OpacityProperty, 1.0, "HoverOverlay"));
        template.Triggers.Add(hoverTrigger);
        return template;
    }

    private static Color BlendColors(Color baseColor, Color tint, double amount)
    {
        var t = Math.Clamp(amount, 0, 1);
        var inv = 1 - t;
        return Color.FromRgb(
            (byte)Math.Round(baseColor.R * inv + tint.R * t),
            (byte)Math.Round(baseColor.G * inv + tint.G * t),
            (byte)Math.Round(baseColor.B * inv + tint.B * t));
    }

    private static (Color Bg, Color Dock, Color Accent, Color Text) ResolveThemePreviewColors(ThemePack? pack)
    {
        var dark = ThemeManager.EffectiveTheme == AppTheme.Dark;
        var defaultBg = dark ? Color.FromRgb(0x1B, 0x20, 0x29) : Color.FromRgb(0xEB, 0xEE, 0xF3);
        var defaultDock = dark ? Color.FromRgb(0x25, 0x2B, 0x37) : Color.FromRgb(0xFF, 0xFF, 0xFF);
        var defaultAccent = dark ? Color.FromRgb(0x38, 0xBD, 0xF8) : Color.FromRgb(0x03, 0x69, 0xA1);
        var defaultText = dark ? Color.FromRgb(0xF3, 0xF4, 0xF6) : Color.FromRgb(0x1B, 0x1B, 0x1B);
        if (pack?.Widgets?.Manifest is not { } manifest) return (defaultBg, defaultDock, defaultAccent, defaultText);

        var map = dark ? manifest.Dark : manifest.Light;
        var bg = TryReadColor(map, manifest.Tokens, "SettingsBackground", defaultBg);
        var dock = TryReadColor(map, manifest.Tokens, "DockMaterial", bg);
        var accent = TryReadColor(map, manifest.Tokens, "AccentBrush",
            TryReadColor(map, manifest.Tokens, "WidgetAccent", defaultAccent));
        var text = TryReadColor(map, manifest.Tokens, "TextPrimary", defaultText);
        return (bg, dock, accent, text);

        static Color TryReadColor(IReadOnlyDictionary<string, System.Text.Json.JsonElement>? primary,
            IReadOnlyDictionary<string, System.Text.Json.JsonElement>? fallback, string key, Color defaultColor)
        {
            if ((primary is not null && primary.TryGetValue(key, out var elem))
                || (fallback is not null && fallback.TryGetValue(key, out elem)))
            {
                string? hex = null;
                if (elem.ValueKind == System.Text.Json.JsonValueKind.String)
                {
                    hex = elem.GetString();
                }
                else if (elem.ValueKind == System.Text.Json.JsonValueKind.Object
                    && elem.TryGetProperty("stops", out var stops)
                    && stops.ValueKind == System.Text.Json.JsonValueKind.Array
                    && stops.GetArrayLength() > 0
                    && stops[0].ValueKind == System.Text.Json.JsonValueKind.String)
                {
                    hex = stops[0].GetString();
                }

                if (hex is not null && hex.StartsWith('#'))
                {
                    try
                    {
                        if (ColorConverter.ConvertFromString(hex) is Color c) return Color.FromRgb(c.R, c.G, c.B);
                    }
                    catch (FormatException) { }
                }
            }
            return defaultColor;
        }
    }

    private static string DescribeThemePack(ThemePack pack)
    {
        var parts = new List<string>();
        if (pack.Icons is not null) parts.Add(UiText.Get("ThemePackIcons"));
        if (pack.Widgets is not null) parts.Add(UiText.Get("ThemePackWidgets"));
        var contents = string.Join(" · ", parts);
        var author = string.IsNullOrWhiteSpace(pack.Manifest.Author) ? string.Empty : " — " + pack.Manifest.Author;
        return contents + author;
    }

    private void OpenGithub_Click(object sender, RoutedEventArgs e)
    {
        try
        {
            using var _ = Process.Start(new ProcessStartInfo("https://github.com/alztrk/opentaskbar")
            {
                UseShellExecute = true
            });
        }
        catch (Exception ex) when (ex is System.IO.IOException or UnauthorizedAccessException or System.ComponentModel.Win32Exception)
        {
            AppLog.Warning("Cannot open the OpenTaskbar GitHub page: " + ex.Message);
        }
    }

    private void ThemePackOpenFolder_Click(object sender, RoutedEventArgs e)
    {
        try
        {
            System.IO.Directory.CreateDirectory(ThemePacks.Root);
            using var _ = Process.Start(new ProcessStartInfo(ThemePacks.Root) { UseShellExecute = true });
        }
        catch (Exception ex) when (ex is System.IO.IOException or UnauthorizedAccessException or System.ComponentModel.Win32Exception)
        {
            AppLog.ReportError("ErrorThemeFolderOpen", ex);
        }
    }

    private void ThemePackReload_Click(object sender, RoutedEventArgs e)
    {
        var selected = _selectedThemePackId;
        _initializing = true;
        LoadThemePacks(selected);
        _initializing = false;
        ThemePacks.Select(selected, reload: true);
        ThemePacks.ApplyWidgets(_preferences.Current.Theme);
    }

    private void ThemePackNew_Click(object sender, RoutedEventArgs e)
    {
        var name = TextPrompt.Ask(UiText.Get("ThemePackNew"), UiText.Get("ThemePackNew"), UiText.Get("ThemePackNewDefault"));
        if (string.IsNullOrWhiteSpace(name)) return;
        var id = ThemePackTransfer.CreateTheme(name, null, null);
        ReloadThemePacks(id);
        MessageBox.Show(this, UiText.Get("ThemePackCreated"), "OpenTaskbar", MessageBoxButton.OK, MessageBoxImage.Information);
    }

    private void ThemePackDuplicate_Click(object sender, RoutedEventArgs e)
    {
        var selected = _selectedThemePackId;
        if (string.IsNullOrWhiteSpace(selected)) { AppLog.ReportError("ErrorThemeMissing"); return; }
        var pack = ThemePacks.Discover().FirstOrDefault(p => string.Equals(p.Id, selected, StringComparison.OrdinalIgnoreCase));
        var name = TextPrompt.Ask(UiText.Get("ThemePackDuplicate"), UiText.Get("ThemePackDuplicate"),
            (pack?.DisplayName ?? selected) + " " + UiText.Get("ThemePackCopySuffix"));
        if (string.IsNullOrWhiteSpace(name)) return;
        var id = ThemePackTransfer.CreateTheme(name, null, selected);
        ReloadThemePacks(id);
        MessageBox.Show(this, UiText.Get("ThemePackDuplicated"), "OpenTaskbar", MessageBoxButton.OK, MessageBoxImage.Information);
    }

    private void ThemePackExport_Click(object sender, RoutedEventArgs e)
    {
        var selected = _selectedThemePackId;
        if (string.IsNullOrWhiteSpace(selected)) { AppLog.ReportError("ErrorThemeMissing"); return; }
        var dialog = new Microsoft.Win32.SaveFileDialog
        {
            Title = UiText.Get("ThemePackSaveFile"),
            Filter = UiText.Get("ThemePackFileFilter"),
            FileName = selected + ThemePackTransfer.ExportExtension,
            DefaultExt = ThemePackTransfer.ExportExtension
        };
        if (dialog.ShowDialog(this) != true) return;
        var error = ThemePackTransfer.ExportTheme(selected, dialog.FileName);
        if (error is not null) { AppLog.ReportError(error); return; }
        MessageBox.Show(this, UiText.Get("ThemePackExported"), "OpenTaskbar", MessageBoxButton.OK, MessageBoxImage.Information);
    }

    private void ThemePackImport_Click(object sender, RoutedEventArgs e)
    {
        var dialog = new Microsoft.Win32.OpenFileDialog
        {
            Title = UiText.Get("ThemePackChooseFile"),
            Filter = UiText.Get("ThemePackFileFilter"),
            CheckFileExists = true
        };
        if (dialog.ShowDialog(this) != true) return;
        var (id, error) = ThemePackTransfer.ImportTheme(dialog.FileName);
        if (error is not null) { AppLog.ReportError(error); return; }
        ReloadThemePacks(id);
        MessageBox.Show(this, UiText.Get("ThemePackImported"), "OpenTaskbar", MessageBoxButton.OK, MessageBoxImage.Information);
    }

    private void ThemePackDelete_Click(object sender, RoutedEventArgs e)
    {
        var selected = _selectedThemePackId;
        if (string.IsNullOrWhiteSpace(selected)) { AppLog.ReportError("ErrorThemeMissing"); return; }
        var result = MessageBox.Show(this, UiText.Format("ThemePackDeleteConfirm", selected), "OpenTaskbar",
            MessageBoxButton.YesNo, MessageBoxImage.Question);
        if (result != MessageBoxResult.Yes) return;
        var error = ThemePackTransfer.DeleteTheme(selected);
        if (error is not null) { AppLog.ReportError(error); return; }
        ReloadThemePacks(null);
        MessageBox.Show(this, UiText.Get("ThemePackDeleted"), "OpenTaskbar", MessageBoxButton.OK, MessageBoxImage.Information);
    }

    private void ReloadThemePacks(string? selected)
    {
        var wasInitializing = _initializing;
        _initializing = true;
        try { LoadThemePacks(selected); }
        finally { _initializing = wasInitializing; }
        ThemePacks.Select(selected, reload: true);
        ThemePacks.ApplyWidgets(_preferences.Current.Theme);
        // New, duplicated, and imported packs should remain selected after restart.
        if (!wasInitializing) TriggerAutoSave(immediate: true);
    }

    private SystemMeters ReadStatsMeters() =>
        (StatsCpuToggle.IsChecked == true ? SystemMeters.Cpu : 0)
        | (StatsMemoryToggle.IsChecked == true ? SystemMeters.Memory : 0)
        | (StatsCpuTemperatureToggle.IsChecked == true ? SystemMeters.CpuTemperature : 0)
        | (StatsGpuToggle.IsChecked == true ? SystemMeters.Gpu : 0)
        | (StatsGpuTemperatureToggle.IsChecked == true ? SystemMeters.GpuTemperature : 0);

    private void RefreshWidgetPreviews() => DockWidgetService.Refresh(
        _widgetOptions.Select(option => option.Preview), _preferences.Current.Language);

    private void WidgetPreviewTimer_Tick(object? sender, EventArgs e) => RefreshWidgetPreviews();

    private void Settings_IsVisibleChanged(object sender, DependencyPropertyChangedEventArgs e) => UpdateWidgetPreviewRefresh();

    private void SettingsTabs_SelectionChanged(object sender, System.Windows.Controls.SelectionChangedEventArgs e)
    {
        if (!_initializing && ReferenceEquals(e.Source, SettingsTabs))
        {
            UpdateWidgetPreviewRefresh();
            if (_preferences.Current.AnimationsEnabled && SettingsTabs.Template?.FindName("PART_SelectedContentHost", SettingsTabs) is ContentPresenter host)
            {
                var easing = new CubicEase { EasingMode = EasingMode.EaseOut };
                easing.Freeze();

                var fade = new DoubleAnimation(0, 1, TimeSpan.FromMilliseconds(180)) { EasingFunction = easing };
                fade.Completed += (_, _) => host.BeginAnimation(UIElement.OpacityProperty, null);
                host.BeginAnimation(UIElement.OpacityProperty, fade);

                // A short upward slide gives page changes a sense of direction without moving the sidebar.
                var transform = host.RenderTransform as TranslateTransform ?? new TranslateTransform();
                host.RenderTransform = transform;
                var slide = new DoubleAnimation(12, 0, TimeSpan.FromMilliseconds(220)) { EasingFunction = easing };
                slide.Completed += (_, _) =>
                {
                    transform.BeginAnimation(TranslateTransform.YProperty, null);
                    transform.Y = 0;
                };
                transform.BeginAnimation(TranslateTransform.YProperty, slide);
            }
        }
    }

    private void UpdateWidgetPreviewRefresh()
    {
        if (IsVisible && WidgetsTab.IsSelected)
        {
            RefreshWidgetPreviews();
            _widgetPreviewTimer.Start();
            _livePreviewTimer?.Start();
        }
        else
        {
            _widgetPreviewTimer.Stop();
            _livePreviewTimer?.Stop();
        }
    }

    /// <summary>
    /// Replaces the catalog's static mock visuals with real, running dock widgets. Each preview is a genuine
    /// widget instance with current settings applied, so the cards show exactly what the taskbar shows.
    /// </summary>
    private void AttachLiveWidgetPreviews()
    {
        StopAndDisposeLivePreviews();
        foreach (var option in _widgetOptions)
        {
            if (!WidgetCoordinator.HandlesWidget(option.Id)) continue;
            try
            {
                var widget = WidgetCoordinator.CreateWidget(option.Id);
                widget.ReportError = message => AppLog.Warning($"Widget preview '{option.Id}': {message}");
                widget.ApplySettings(_applied);
                widget.Activate();
                widget.Tick(DateTime.Now);
                // Reflect hardware availability (e.g. a desktop without a battery) in the catalog.
                option.IsHardwareAvailable = widget.IsHardwareAvailable;
                widget.HardwareAvailabilityChanged += (_, _) => option.IsHardwareAvailable = widget.IsHardwareAvailable;
                // An inactive data source (no media session, no providers signed in) must not blank the card.
                widget.Visibility = widget.IsHardwareAvailable ? Visibility.Visible : Visibility.Collapsed;
                option.LivePreview = widget;
                option.NativePreview = widget;
                // Build the flyout panel once; widget settings changes refresh it below.
                option.PreviewFlyoutContent = widget.BuildPreviewFlyoutContent();
                _livePreviewWidgets.Add(widget);
            }
            catch (Exception ex)
            {
                AppLog.Warning($"Widget preview '{option.Id}' unavailable: {ex.Message}");
            }
        }

        _livePreviewTimer = new DispatcherTimer(DispatcherPriority.Background, Dispatcher) { Interval = TimeSpan.FromSeconds(1) };
        _livePreviewTimer.Tick += (_, _) =>
        {
            var now = DateTime.Now;
            foreach (var widget in _livePreviewWidgets)
            {
                widget.Tick(now);
                if (widget.IsHardwareAvailable && widget.Visibility != Visibility.Visible)
                    widget.Visibility = Visibility.Visible;
            }
        };
        _livePreviewTimer.Start();
    }

    /// <summary>
    /// Pushes the current settings into every catalog preview immediately. The previews are genuine
    /// widget instances, so this re-renders them exactly as the taskbar widgets would after a save.
    /// </summary>
    private void ApplySettingsToLivePreviews(DockSettings settings)
    {
        foreach (var option in _widgetOptions)
        {
            if (option.LivePreview is not OpenTaskbar.Widgets.DockWidget widget) continue;
            widget.ApplySettings(settings);
            // Some widgets hide themselves when their data source is inactive (e.g. media with no
            // session). A preview must stay visible so the card never collapses to an empty tile.
            if (widget.IsHardwareAvailable && widget.Visibility != Visibility.Visible)
                widget.Visibility = Visibility.Visible;
            option.IsHardwareAvailable = widget.IsHardwareAvailable;
            // Rebuild the inline flyout preview so it reflects the just-applied settings.
            if (option.ShowFlyoutPreview) option.PreviewFlyoutContent = widget.BuildPreviewFlyoutContent();
        }
    }

    private void StopAndDisposeLivePreviews()
    {
        _livePreviewTimer?.Stop();
        _livePreviewTimer = null;
        var liveWidgets = new HashSet<OpenTaskbar.Widgets.DockWidget>(_livePreviewWidgets);
        foreach (var widget in _livePreviewWidgets) widget.Dispose();
        _livePreviewWidgets.Clear();
        foreach (var option in _widgetOptions)
        {
            option.LivePreview = null;
            // Detach the disposed live widget so callers (and the Closed handler) never touch it again.
            // Any other standalone preview widget is ours to dispose here.
            if (option.NativePreview is OpenTaskbar.Widgets.DockWidget standalone && !liveWidgets.Contains(standalone))
                standalone.Dispose();
            if (option.NativePreview is OpenTaskbar.Widgets.DockWidget) option.NativePreview = option.Preview;
        }
    }

    /// <summary>A localized category label followed by the widgets that belong to it.</summary>
    internal sealed class WidgetCategoryGroup : INotifyPropertyChanged
    {
        public WidgetCategoryGroup(DockWidgetCategory category, IReadOnlyList<DockWidgetOption> widgets)
        {
            Category = category;
            Widgets = widgets;
        }

        public DockWidgetCategory Category { get; }
        public IReadOnlyList<DockWidgetOption> Widgets { get; }

        public string Label => Category switch
        {
            DockWidgetCategory.TimeAndWeather => UiText.Get("WidgetCategoryTime"),
            DockWidgetCategory.SystemAndHardware => UiText.Get("WidgetCategorySystem"),
            DockWidgetCategory.AiQuotas => UiText.Get("WidgetCategoryAi"),
            DockWidgetCategory.MediaAndControls => UiText.Get("WidgetCategoryMedia"),
            DockWidgetCategory.Productivity => UiText.Get("WidgetCategoryProductivity"),
            _ => string.Empty
        };

        public event PropertyChangedEventHandler? PropertyChanged;

        public void RefreshLabel() => PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(Label)));
    }

    private void RebuildWidgetGroups()
    {
        _widgetGroups.Clear();
        foreach (var category in Enum.GetValues<DockWidgetCategory>())
        {
            var widgets = _widgetOptions.Where(option => option.CategoryEnum == category).ToList();
            if (widgets.Count > 0) _widgetGroups.Add(new WidgetCategoryGroup(category, widgets));
        }
    }

    private void WidgetInstallation_Click(object sender, RoutedEventArgs e)
    {
        if (e.OriginalSource is not FrameworkElement { DataContext: DockWidgetOption option }) return;
        _initializing = true;
        if (option.IsInstalled)
        {
            option.IsInstalled = false;
            option.IsEnabled = false;
            option.IsSettingsExpanded = false;
        }
        else
        {
            option.IsInstalled = true;
            option.IsEnabled = true;
        }
        _initializing = false;
        UpdateWidgetOptionState();
        SaveSettings();
    }

    private void WidgetSettings_Click(object sender, RoutedEventArgs e)
    {
        if (e.OriginalSource is not FrameworkElement { DataContext: DockWidgetOption option }) return;
        var expand = !option.IsSettingsExpanded;
        foreach (var other in _widgetOptions)
            other.IsSettingsExpanded = ReferenceEquals(other, option) && expand;
        if (expand && ReferenceEquals(option.NativePreview, option.Preview))
        {
            // Prefer the already-running live widget so the drawer shows exactly what the dock shows.
            var live = option.LivePreview as OpenTaskbar.Widgets.DockWidget;
            var native = live ?? WidgetCoordinator.CreatePreviewWidget(option.Id);
            if (native is not null)
            {
                native.ApplySettings(ReadSelection());
                native.Visibility = Visibility.Visible;
                // Only widgets with local-only rendering paint an extra frame when they are not
                // already activated; the rest keep their constructor's honest placeholder state.
                if (live is null && native.SupportsStaticPreview) native.RenderPreview();
                option.NativePreview = native;
            }
            else
            {
                option.NativePreview = option.Preview;
            }
        }
        UpdateWidgetOptionState();
        WidgetOptionsList.InvalidateMeasure();
    }

    private void CollapseClockOptions_Click(object sender, RoutedEventArgs e)
    {
        if (_widgetOptions.FirstOrDefault(o => o.Id == "Clock") is { } option)
            option.IsSettingsExpanded = false;
        UpdateWidgetOptionState();
    }

    private void CollapseWeatherOptions_Click(object sender, RoutedEventArgs e)
    {
        if (_widgetOptions.FirstOrDefault(o => o.Id == "Weather") is { } option)
            option.IsSettingsExpanded = false;
        UpdateWidgetOptionState();
    }

    private void CollapseFocusOptions_Click(object sender, RoutedEventArgs e)
    {
        if (_widgetOptions.FirstOrDefault(o => o.Id == "Focus") is { } option)
            option.IsSettingsExpanded = false;
        UpdateWidgetOptionState();
    }

    private void CollapseStatsOptions_Click(object sender, RoutedEventArgs e)
    {
        if (_widgetOptions.FirstOrDefault(o => o.Id == "SystemStats") is { } option)
            option.IsSettingsExpanded = false;
        UpdateWidgetOptionState();
    }

    private void SelectAlignment(DockSettings settings)
    {
        SelectChoice(PositionBox, settings.Layout == DockLayout.FullWidth ? "FullWidth" : settings.Position.ToString());
    }

    private static void OpenActions(object sender)
    {
        if (sender is Button button && button.ContextMenu is { } menu)
        {
            menu.PlacementTarget = button;
            menu.IsOpen = true;
        }
    }

    private void ThemePackActions_Click(object sender, RoutedEventArgs e) => OpenActions(sender);

    private void BackupActions_Click(object sender, RoutedEventArgs e) => OpenActions(sender);

    private void LoadFeatureSelection(DockSettings settings)
    {
        RecycleBinToggle.IsChecked = settings.ShowRecycleBin;
        SearchButtonToggle.IsChecked = settings.ShowSearchButton;
        TaskViewButtonToggle.IsChecked = settings.ShowTaskViewButton;
        TrayButtonToggle.IsChecked = settings.ShowTrayButton;
        ShowDesktopButtonToggle.IsChecked = settings.ShowDesktopButton;
        SelectChoice(DensityBox, settings.GlobalWidgetDensity.ToString());
        foreach (var (id, combo) in _widgetDensityBoxes)
        {
            var tag = settings.WidgetDensityOverrides is not null
                && settings.WidgetDensityOverrides.TryGetValue(id, out var mode)
                    ? mode.ToString()
                    : "Default";
            SelectDensityTag(combo, tag);
        }
        LoadDisplayChoices(settings.TargetDisplay);
        AllMonitorsToggle.IsChecked = settings.ShowOnAllMonitors;
        LoadSecondaryAppChoices(settings.SecondaryApps);
        SecondaryWidgetsToggle.IsChecked = settings.SecondaryWidgets;
        UpdateMultiMonitorControls();
        CurrentDesktopToggle.IsChecked = settings.CurrentDesktopOnly;
        UpdatesToggle.IsChecked = settings.CheckUpdates;
        SelectChoice(EdgeBox, settings.Edge.ToString());
        SelectChoice(ClockFormatBox, settings.Clock24Hour ? "24" : "12");
        ClockSecondsToggle.IsChecked = settings.ClockShowSeconds;
        SelectChoice(DateStyleBox, settings.DateStyle.ToString());
        WeatherCityBox.Text = settings.WeatherCity;
        _aiUsageRing = settings.AiUsageRing;
        foreach (var combo in _aiRingBoxes)
        {
            foreach (var item in combo.Items)
            {
                if (item is ComboBoxItem cbi && string.Equals(cbi.Tag as string, _aiUsageRing.ToString(), StringComparison.Ordinal))
                {
                    combo.SelectedItem = cbi;
                    break;
                }
            }
        }
        StatsCpuToggle.IsChecked = settings.StatsMeters.HasFlag(SystemMeters.Cpu);
        StatsMemoryToggle.IsChecked = settings.StatsMeters.HasFlag(SystemMeters.Memory);
        StatsCpuTemperatureToggle.IsChecked = settings.StatsMeters.HasFlag(SystemMeters.CpuTemperature);
        StatsGpuToggle.IsChecked = settings.StatsMeters.HasFlag(SystemMeters.Gpu);
        StatsGpuTemperatureToggle.IsChecked = settings.StatsMeters.HasFlag(SystemMeters.GpuTemperature);
        // WinAltD is retired: existing selections fall back to the equivalent Ctrl+Alt+D.
        SelectChoice(HotkeyBox, settings.DockHotkey is HotkeyChoice.CtrlAltD or HotkeyChoice.WinAltD ? "CtrlAltD" : "Disabled");
        SelectChoice(StartPlacementBox, settings.StartPlacement.ToString());
        WheelVolumeToggle.IsChecked = settings.WheelVolume;
        _ = InitializeWeatherLocationsAsync(settings.WeatherCity);
    }

    private void AttachWidgetSettingsPanels()
    {
        WidgetPanelsPool.Children.Clear();
        _widgetDensityBoxes.Clear();
        _aiRingBoxes.Clear();
        if (_widgetOptions.FirstOrDefault(o => o.Id == "Clock") is { } clock) clock.SettingsView = ClockOptions;
        if (_widgetOptions.FirstOrDefault(o => o.Id == "Weather") is { } weather) weather.SettingsView = WeatherOptions;
        if (_widgetOptions.FirstOrDefault(o => o.Id == "Focus") is { } focus)
        {
            AttachDensityRow("Focus", FocusOptions);
            focus.SettingsView = FocusOptions;
        }
        if (_widgetOptions.FirstOrDefault(o => o.Id == "SystemStats") is { } stats)
        {
            AttachDensityRow("SystemStats", StatsOptions);
            stats.SettingsView = StatsOptions;
        }
        var aiUsageIds = new HashSet<string>(DockWidgetService.AiUsageWidgets.Select(w => w.Id), StringComparer.Ordinal);
        foreach (var option in _widgetOptions)
        {
            if (option.SettingsView is not null) continue;
            var panel = new StackPanel();
            AttachDensityRow(option.Id, panel);
            if (aiUsageIds.Contains(option.Id))
            {
                AttachAiRingRow(panel);
            }
            option.SettingsView = panel;
        }
    }

    private void AttachAiRingRow(Panel panel)
    {
        var row = new Grid { Margin = new Thickness(0, 0, 0, panel.Children.Count > 0 ? 8 : 0) };
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });

        var label = new TextBlock
        {
            Text = UiText.Get("AiUsageRingLabel"),
            FontSize = 12,
            VerticalAlignment = VerticalAlignment.Center,
            Margin = new Thickness(0, 0, 12, 0)
        };
        label.SetResourceReference(TextBlock.ForegroundProperty, "TextSecondary");
        Grid.SetColumn(label, 0);
        row.Children.Add(label);

        var combo = new ComboBox { MinWidth = 128 };
        combo.Items.Add(new ComboBoxItem { Content = UiText.Get("AiUsageRingHighest"), Tag = nameof(AiUsageRing.Highest) });
        combo.Items.Add(new ComboBoxItem { Content = UiText.Get("AiUsageRingSession"), Tag = nameof(AiUsageRing.Session) });
        combo.Items.Add(new ComboBoxItem { Content = UiText.Get("AiUsageRingWeekly"), Tag = nameof(AiUsageRing.Weekly) });

        combo.SelectedItem = combo.Items.OfType<ComboBoxItem>().FirstOrDefault(item => string.Equals(item.Tag as string, _aiUsageRing.ToString(), StringComparison.Ordinal))
            ?? combo.Items[0];

        combo.SelectionChanged += (_, _) =>
        {
            if (!_initializing && combo.SelectedItem is ComboBoxItem { Tag: string tag } && Enum.TryParse<AiUsageRing>(tag, out var ring))
            {
                _aiUsageRing = ring;
                ApplySettingsToLivePreviews(ReadSelection());
                TriggerAutoSave(immediate: true);
            }
        };
        Grid.SetColumn(combo, 1);
        row.Children.Add(combo);
        _aiRingBoxes.Add(combo);
        panel.Children.Add(row);
    }

    private void AttachDensityRow(string widgetId, FrameworkElement container)
    {
        var panel = container is Border { Child: Panel inner } ? inner : container as Panel;
        if (panel is null || _widgetDensityBoxes.ContainsKey(widgetId)) return;

        var row = new Grid { Margin = new Thickness(0, 0, 0, panel.Children.Count > 0 ? 10 : 0) };
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });

        var label = new TextBlock
        {
            Text = UiText.Get("WidgetDensity"),
            FontSize = 12,
            VerticalAlignment = VerticalAlignment.Center,
            Margin = new Thickness(0, 0, 12, 0)
        };
        label.SetResourceReference(TextBlock.ForegroundProperty, "TextSecondary");
        Grid.SetColumn(label, 0);
        row.Children.Add(label);

        var combo = new ComboBox { MinWidth = 128 };
        combo.Items.Add(new ComboBoxItem { Content = UiText.Get("WidgetDensityFollowGlobal"), Tag = "Default" });
        combo.Items.Add(new ComboBoxItem { Content = UiText.Get("WidgetDensityAuto"), Tag = nameof(WidgetDensityMode.Auto) });
        combo.Items.Add(new ComboBoxItem { Content = UiText.Get("WidgetDensityCompact"), Tag = nameof(WidgetDensityMode.Compact) });
        combo.Items.Add(new ComboBoxItem { Content = UiText.Get("WidgetDensityFull"), Tag = nameof(WidgetDensityMode.Full) });
        combo.SelectedIndex = 0;
        combo.SelectionChanged += (_, _) =>
        {
            if (!_initializing)
            {
                ApplySettingsToLivePreviews(ReadSelection());
                TriggerAutoSave(immediate: true);
            }
        };
        Grid.SetColumn(combo, 1);
        row.Children.Add(combo);

        _widgetDensityBoxes[widgetId] = combo;
        panel.Children.Insert(0, row);
    }

    private static void SelectDensityTag(ComboBox combo, string tag)
    {
        foreach (var item in combo.Items)
        {
            if (item is ComboBoxItem cbi && string.Equals(cbi.Tag as string, tag, StringComparison.Ordinal))
            {
                combo.SelectedItem = cbi;
                return;
            }
        }
        combo.SelectedIndex = 0;
    }

    private IReadOnlyDictionary<string, WidgetDensityMode>? ReadWidgetDensityOverrides()
    {
        Dictionary<string, WidgetDensityMode>? result = null;
        foreach (var (id, combo) in _widgetDensityBoxes)
        {
            if (combo.SelectedItem is ComboBoxItem { Tag: string tag } && Enum.TryParse<WidgetDensityMode>(tag, out var mode))
            {
                result ??= new Dictionary<string, WidgetDensityMode>(StringComparer.OrdinalIgnoreCase);
                result[id] = mode;
            }
        }
        return result;
    }

    private async Task InitializeWeatherLocationsAsync(string savedCity)
    {
        try
        {
            var countries = await CountryCityService.GetCountriesAsync();
            WeatherCountryBox.ItemsSource = countries.Select(c => c.Name).ToList();

            if (!string.IsNullOrWhiteSpace(savedCity))
            {
                var trCities = await CountryCityService.GetCitiesAsync("Turkey");
                string matchedCountry = trCities.Contains(savedCity, StringComparer.OrdinalIgnoreCase) ? "Turkey" : "Turkey";
                WeatherCountryBox.SelectedItem = matchedCountry;
                WeatherCityContainer.Visibility = Visibility.Visible;
                WeatherCityCombo.ItemsSource = trCities;
                WeatherCityCombo.SelectedItem = savedCity;
                WeatherCityBox.Text = savedCity;
            }
        }
        catch (Exception ex)
        {
            AppLog.Warning($"Weather location initialization error: {ex.Message}");
        }
    }

    private async void WeatherCountryBox_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (_initializing) return;
        var country = WeatherCountryBox.SelectedItem as string ?? WeatherCountryBox.Text.Trim();
        await OnCountrySelectedAsync(country);
    }

    private async void WeatherCountryBox_KeyUp(object sender, System.Windows.Input.KeyEventArgs e)
    {
        if (e.Key is System.Windows.Input.Key.Enter or System.Windows.Input.Key.Tab)
        {
            var country = WeatherCountryBox.Text.Trim();
            await OnCountrySelectedAsync(country);
        }
    }

    private async Task OnCountrySelectedAsync(string country)
    {
        if (string.IsNullOrWhiteSpace(country))
        {
            WeatherCityContainer.Visibility = Visibility.Collapsed;
            WeatherCityBox.Text = string.Empty;
            SaveSettings();
            return;
        }

        WeatherCityContainer.Visibility = Visibility.Visible;
        var cities = await CountryCityService.GetCitiesAsync(country);
        WeatherCityCombo.ItemsSource = cities;
        if (cities.Count > 0)
        {
            WeatherCityCombo.IsDropDownOpen = true;
        }
    }

    private void WeatherCityCombo_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (_initializing) return;
        var city = WeatherCityCombo.SelectedItem as string;
        if (!string.IsNullOrWhiteSpace(city))
        {
            WeatherCityBox.Text = city;
            PreferenceEdited(sender, e);
        }
    }

    private void WeatherCityCombo_KeyUp(object sender, System.Windows.Input.KeyEventArgs e)
    {
        if (e.Key is System.Windows.Input.Key.Enter or System.Windows.Input.Key.Tab)
        {
            var city = WeatherCityCombo.Text.Trim();
            if (!string.IsNullOrWhiteSpace(city))
            {
                WeatherCityBox.Text = city;
                PreferenceEdited(sender, e);
            }
        }
    }

    private void WeatherCityBox_TextChanged(object sender, System.Windows.Controls.TextChangedEventArgs e) => PreferenceEdited(sender, e);

    private void WeatherCityBox_LostFocus(object sender, RoutedEventArgs e)
    {
        if (_autoSaveTimer.IsEnabled)
        {
            _autoSaveTimer.Stop();
            SaveSettings();
        }
    }

    private void ExportSettings_Click(object sender, RoutedEventArgs e)
    {
        var dialog = new Microsoft.Win32.SaveFileDialog
        {
            FileName = "OpenTaskbar-settings.json",
            Filter = "OpenTaskbar (*.json)|*.json",
            DefaultExt = ".json"
        };
        if (dialog.ShowDialog(this) != true) return;
        var error = SettingsBackup.Export(dialog.FileName, _preferences.Current);
        if (error is not null)
            AppLog.Error($"Settings export: {error}");
        else
            MessageBox.Show(this, UiText.Get("ExportDone"), "OpenTaskbar", MessageBoxButton.OK, MessageBoxImage.Information);
    }

    private void ImportSettings_Click(object sender, RoutedEventArgs e)
    {
        var dialog = new Microsoft.Win32.OpenFileDialog { Filter = "OpenTaskbar (*.json)|*.json" };
        if (dialog.ShowDialog(this) != true) return;
        var (settings, pins, error) = SettingsBackup.Read(dialog.FileName);
        if (settings is null)
        {
            AppLog.ReportError(error ?? "ErrorSettingsFileInvalid");
            return;
        }

        var added = (Application.Current.MainWindow as MainWindow)?.ImportPins(pins) ?? 0;
        if (_preferences.Save(settings with { PinOrder = settings.PinOrder ?? _preferences.Current.PinOrder }) is { } saveError)
        {
            AppLog.ReportError(saveError);
            return;
        }

        _applied = _preferences.Current;
        _initializing = true;
        LoadAllSelections(_applied);
        _initializing = false;
        UpdateWidgetOptionState();
        MessageBox.Show(this, UiText.Format("ImportDone", added), "OpenTaskbar", MessageBoxButton.OK, MessageBoxImage.Information);
    }

    private void OpenLogFile_Click(object sender, RoutedEventArgs e) => AppLog.TryOpenLogFile();

    private void LoadAllSelections(DockSettings settings)
    {
        SelectChoice(ThemeBox, settings.Theme.ToString());
        SelectLanguage(settings.Language);
        SelectAlignment(settings);
        AnimationsToggle.IsChecked = settings.AnimationsEnabled;
        PreviewsToggle.IsChecked = settings.PreviewsEnabled;
        SelectChoice(AutoHideBox, settings.AutoHide.ToString());
        LoadSurfaceStyles(settings);
        MagnificationToggle.IsChecked = settings.MagnificationEnabled;
        SelectChoice(SizeBox, settings.Size.ToString());
        DockScaleSlider.Value = Math.Round(settings.DockScale * 100);
        SelectChoice(DockWidthModeBox, settings.DockWidthMode.ToString());
        UpdateDockWidthControls();
        UpdateDockScaleLabel();
        var reloadProfile = DockMetrics.For(settings.Size, settings.DockScale);
        CornerRadiusSlider.Value = settings.DockCornerRadius ?? (int)Math.Round(reloadProfile.Radius);
        UpdateCornerRadiusLabel();
        LoadThemePacks(settings.ThemePack);
        foreach (var option in _widgetOptions)
        {
            option.IsInstalled = settings.InstalledWidgets?.Contains(option.Id, StringComparer.Ordinal) == true;
            option.IsEnabled = settings.Widgets?.Contains(option.Id, StringComparer.Ordinal) == true;
        }
        FocusMinutesSlider.Value = settings.FocusMinutes;
        BreakMinutesSlider.Value = settings.FocusBreakMinutes;
        FocusSessionsSlider.Value = settings.FocusSessions;
        LoadFeatureSelection(settings);
    }

    private void ResetDockDefaults_Click(object sender, RoutedEventArgs e)
    {
        var defaults = new DockSettings();
        _initializing = true;
        try
        {
            SelectChoice(EdgeBox, defaults.Edge.ToString());
            SelectAlignment(defaults);
            SelectChoice(SizeBox, defaults.Size.ToString());
            DockScaleSlider.Value = Math.Round(defaults.DockScale * 100);
            SelectChoice(DockWidthModeBox, defaults.DockWidthMode.ToString());
            UpdateDockWidthControls();
            LoadDockGapControls(defaults);
            UpdateDockScaleLabel();
            SelectChoice(AutoHideBox, defaults.AutoHide.ToString());
            AnimationsToggle.IsChecked = defaults.AnimationsEnabled;
            MagnificationToggle.IsChecked = defaults.MagnificationEnabled;
            PreviewsToggle.IsChecked = defaults.PreviewsEnabled;
            RecycleBinToggle.IsChecked = defaults.ShowRecycleBin;
            CurrentDesktopToggle.IsChecked = defaults.CurrentDesktopOnly;
            SearchButtonToggle.IsChecked = defaults.ShowSearchButton;
            TaskViewButtonToggle.IsChecked = defaults.ShowTaskViewButton;
            TrayButtonToggle.IsChecked = defaults.ShowTrayButton;
            ShowDesktopButtonToggle.IsChecked = defaults.ShowDesktopButton;
            LoadDisplayChoices(defaults.TargetDisplay);
            AllMonitorsToggle.IsChecked = defaults.ShowOnAllMonitors;
            LoadSecondaryAppChoices(defaults.SecondaryApps);
            SecondaryWidgetsToggle.IsChecked = defaults.SecondaryWidgets;
            UpdateMultiMonitorControls();
        }
        finally
        {
            _initializing = false;
        }
        PreferenceEdited(sender, e);
    }

    private void UpdateWidgetOptionState()
    {
        bool IsExpanded(string id) => _widgetOptions.Any(o => string.Equals(o.Id, id, StringComparison.OrdinalIgnoreCase) && o.IsSettingsExpanded);
        ClockOptions.Visibility = IsExpanded("Clock") ? Visibility.Visible : Visibility.Collapsed;
        FocusOptions.Visibility = IsExpanded("Focus") ? Visibility.Visible : Visibility.Collapsed;
        StatsOptions.Visibility = IsExpanded("SystemStats") ? Visibility.Visible : Visibility.Collapsed;
        WeatherOptions.Visibility = IsExpanded("Weather") ? Visibility.Visible : Visibility.Collapsed;
        FocusMinutesValue.Text = UiText.Format("MinutesFormat", (int)Math.Round(FocusMinutesSlider.Value));
        BreakMinutesValue.Text = UiText.Format("MinutesFormat", (int)Math.Round(BreakMinutesSlider.Value));
        FocusSessionsValue.Text = ((int)Math.Round(FocusSessionsSlider.Value)).ToString(System.Globalization.CultureInfo.InvariantCulture);
    }

    private void FocusSlider_ValueChanged(object sender, RoutedPropertyChangedEventArgs<double> e)
    {
        if (_initializing) return;
        PreferenceEdited(sender, e);
    }

    private void SaveSettings()
    {
        if (_closed) return;
        var startupChanged = _startupAvailable && StartupToggle.IsChecked != _startupEnabled;
        if (startupChanged && _startup.SetEnabled(StartupToggle.IsChecked == true) is { } startupError)
        {
            Trace.TraceWarning("Startup error: {0}", startupError);
            return;
        }
        var settings = ReadSelection();
        if (_preferences.Save(settings) is { } error)
        {
            Trace.TraceWarning("Save settings error: {0}", error);
            return;
        }
        _applied = settings;
        _startupEnabled = StartupToggle.IsChecked == true;
    }

    private bool _isClosing;
    private bool _isClosingCompleted;

    private void Window_Closing(object? sender, CancelEventArgs e)
    {
        if (_closingApplication || Application.Current.Dispatcher.HasShutdownStarted || _isClosingCompleted) return;
        if (_preferences.Current.AnimationsEnabled && IsLoaded && !_isClosing)
        {
            e.Cancel = true;
            AnimateAndClose();
            return;
        }
        if (_autoSaveTimer.IsEnabled)
        {
            _autoSaveTimer.Stop();
            SaveSettings();
        }
    }

    private void AnimateAndClose()
    {
        if (_isClosing) return;
        _isClosing = true;
        if (_autoSaveTimer.IsEnabled)
        {
            _autoSaveTimer.Stop();
            SaveSettings();
        }

        var fade = new System.Windows.Media.Animation.DoubleAnimation(1.0, 0.0, TimeSpan.FromMilliseconds(160))
        {
            EasingFunction = new System.Windows.Media.Animation.QuadraticEase { EasingMode = System.Windows.Media.Animation.EasingMode.EaseOut }
        };
        var scaleAnim = new System.Windows.Media.Animation.DoubleAnimation(1.0, 0.95, TimeSpan.FromMilliseconds(160))
        {
            EasingFunction = new System.Windows.Media.Animation.QuadraticEase { EasingMode = System.Windows.Media.Animation.EasingMode.EaseOut }
        };

        var transform = new ScaleTransform(1.0, 1.0);
        SettingsFrame.RenderTransformOrigin = new Point(0.5, 0.5);
        SettingsFrame.RenderTransform = transform;

        fade.Completed += (_, _) =>
        {
            _isClosingCompleted = true;
            Close();
        };

        SettingsFrame.BeginAnimation(UIElement.OpacityProperty, fade);
        transform.BeginAnimation(ScaleTransform.ScaleXProperty, scaleAnim);
        transform.BeginAnimation(ScaleTransform.ScaleYProperty, scaleAnim);
    }

    private void PreferenceEdited(object sender, RoutedEventArgs e)
    {
        if (_initializing || _closed) return;
        if (e.OriginalSource is ToggleButton { TemplatedParent: ComboBox }) return;
        // Unnamed switches inside a widget card are the install toggles; their Click handler
        // saves explicitly. Named switches live in the per-widget settings panels and auto-save.
        if (e.OriginalSource is CheckBox { DataContext: DockWidgetOption, Name: "" }) return;
        var selectedSize = Enum.TryParse<DockSize>(Choice(SizeBox), out var size) ? size : DockSize.Compact;
        (Application.Current.MainWindow as MainWindow)?.SetSizePreview(selectedSize, Math.Round(DockScaleSlider.Value) / 100);
        UpdateWidgetOptionState();
        UpdateMultiMonitorControls();
        // Reflect widget-related edits in the catalog immediately, without waiting for the
        // debounced save. Every live preview is a real widget, so ApplySettings re-renders it.
        ApplySettingsToLivePreviews(ReadSelection());
        TriggerAutoSave(immediate: sender is not (Slider or TextBox));
    }

    private void TriggerAutoSave(bool immediate)
    {
        if (_initializing || _closed) return;
        if (immediate)
        {
            _autoSaveTimer.Stop();
            SaveSettings();
        }
        else
        {
            _autoSaveTimer.Stop();
            _autoSaveTimer.Start();
        }
    }

    private void RegisterSurface(SurfaceKind kind, Slider slider, TextBlock value, ComboBox box)
    {
        _surfaceSliders[kind] = slider;
        _surfaceValues[kind] = value;
        _surfaceBoxes[kind] = box;
    }

    private static SurfaceKind SurfaceOf(object sender) =>
        (sender as FrameworkElement)?.Tag is string tag && Enum.TryParse(tag, out SurfaceKind kind) ? kind : SurfaceKind.Dock;

    private SurfaceBackground SurfaceBackgroundOf(SurfaceKind kind) =>
        Enum.TryParse<SurfaceBackground>(Choice(_surfaceBoxes[kind]), out var background) ? background : SurfaceBackground.None;

    private void LoadSurfaceStyles(DockSettings settings)
    {
        var previous = _initializing;
        _initializing = true;
        try
        {
            LoadSurface(SurfaceKind.Dock, settings.DockBackground, settings.DockOpacity);
            LoadSurface(SurfaceKind.Menu, settings.MenuBackground, settings.MenuOpacity);
            LoadSurface(SurfaceKind.Widget, settings.WidgetBackground, settings.WidgetOpacity);
            LoadSurface(SurfaceKind.Settings, settings.SettingsBackground, settings.SettingsOpacity);
        }
        finally
        {
            _initializing = previous;
        }
    }

    private void LoadSurface(SurfaceKind kind, SurfaceBackground background, double opacity)
    {
        SelectChoice(_surfaceBoxes[kind], background.ToString());
        _surfaceSliders[kind].Value = Math.Clamp(opacity * 100, 0, 100);
        _surfaceSliders[kind].IsEnabled = background != SurfaceBackground.None;
        _surfaceValues[kind].Text = UiText.Format("OpacityPercent", (int)Math.Round(_surfaceSliders[kind].Value));
    }

    private void SegmentChoice_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (_initializing) return;
        if (ReferenceEquals(sender, EdgeBox)) UpdateDockWidthControls();
        PreferenceEdited(sender, e);
    }

    private void SurfaceStyle_Changed(object sender, SelectionChangedEventArgs e)
    {
        if (_initializing) return;
        var kind = SurfaceOf(sender);
        var background = SurfaceBackgroundOf(kind);
        _surfaceSliders[kind].IsEnabled = background != SurfaceBackground.None;
        ThemeManager.SetSurface(kind, background, Math.Round(_surfaceSliders[kind].Value) / 100);
        if (kind == SurfaceKind.Settings) UpdateBackdrop();
        PreferenceEdited(sender, e);
    }

    private void SurfaceOpacitySlider_ValueChanged(object sender, RoutedPropertyChangedEventArgs<double> e)
    {
        if (_initializing) return;
        var kind = SurfaceOf(sender);
        _surfaceValues[kind].Text = UiText.Format("OpacityPercent", (int)Math.Round(e.NewValue));
        ThemeManager.SetSurface(kind, SurfaceBackgroundOf(kind), Math.Round(e.NewValue) / 100);
        PreferenceEdited(sender, e);
    }

    private void DockWidthModeBox_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        UpdateDockWidthControls();
        if (!_initializing) PreferenceEdited(sender, e);
    }

    private void DockWidthSlider_ValueChanged(object sender, RoutedPropertyChangedEventArgs<double> e)
    {
        UpdateDockWidthLabel();
        if (!_initializing) PreferenceEdited(sender, e);
    }

    private static DockGapUnit GapUnit(ComboBox box) =>
        Enum.TryParse<DockGapUnit>(Choice(box), out var unit) ? unit : DockGapUnit.Pixels;

    private void LoadDockGapControls(DockSettings settings)
    {
        // Caller controls _initializing (constructor / reset both set it). Do not toggle it here: setting it to
        // false early would let slider/checkbox events fire and mark preferences as edited during load.
        SelectChoice(DockTopGapUnitBox, settings.DockTopGapUnit.ToString());
        ConfigureGapSlider(DockTopGapSlider, settings.DockTopGapUnit, settings.DockTopGapPixels, settings.DockTopGapPercent);
        SelectChoice(DockBottomGapUnitBox, settings.DockBottomGapUnit.ToString());
        ConfigureGapSlider(DockBottomGapSlider, settings.DockBottomGapUnit, settings.DockBottomGapPixels, settings.DockBottomGapPercent);
        UpdateDockGapLabels();
    }

    private static void ConfigureGapSlider(Slider slider, DockGapUnit unit, int pixels, int percent)
    {
        slider.Minimum = 0;
        slider.Maximum = unit == DockGapUnit.Percent ? 100 : 200;
        slider.TickFrequency = 1;
        slider.Value = unit == DockGapUnit.Percent ? percent : pixels;
    }

    private void UpdateDockGapLabels()
    {
        // XAML sets slider Values during InitializeComponent, before the unit ComboBoxes exist.
        if (DockTopGapValue is null || DockBottomGapValue is null) return;
        DockTopGapValue.Text = Math.Round(DockTopGapSlider.Value) + (DockTopGapUnitBox is not null && GapUnit(DockTopGapUnitBox) == DockGapUnit.Percent ? "%" : "px");
        DockBottomGapValue.Text = Math.Round(DockBottomGapSlider.Value) + (DockBottomGapUnitBox is not null && GapUnit(DockBottomGapUnitBox) == DockGapUnit.Percent ? "%" : "px");
    }

    private void DockTopGapUnitBox_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (_initializing) return;
        var unit = GapUnit(DockTopGapUnitBox);
        var value = Math.Round(DockTopGapSlider.Value);
        ConfigureGapSlider(DockTopGapSlider, unit, (int)value, (int)value);
        UpdateDockGapLabels();
        PreferenceEdited(sender, e);
    }

    private void DockBottomGapUnitBox_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (_initializing) return;
        var unit = GapUnit(DockBottomGapUnitBox);
        var value = Math.Round(DockBottomGapSlider.Value);
        ConfigureGapSlider(DockBottomGapSlider, unit, (int)value, (int)value);
        UpdateDockGapLabels();
        PreferenceEdited(sender, e);
    }

    private void DockTopGapSlider_ValueChanged(object sender, RoutedPropertyChangedEventArgs<double> e)
    {
        UpdateDockGapLabels();
        if (!_initializing) PreferenceEdited(sender, e);
    }

    private void DockBottomGapSlider_ValueChanged(object sender, RoutedPropertyChangedEventArgs<double> e)
    {
        UpdateDockGapLabels();
        if (!_initializing) PreferenceEdited(sender, e);
    }

    private void UpdateDockWidthControls()
    {
        var vertical = Choice(EdgeBox) is nameof(DockEdge.Left) or nameof(DockEdge.Right);
        if (DockWidthModeLabel is not null)
        {
            DockWidthModeLabel.Text = UiText.Get(vertical ? "DockHeightMode" : "DockWidthMode");
            DockWidthModeDescription.Text = UiText.Get(vertical ? "DockHeightModeDescription" : "DockWidthModeDescription");
            DockWidthValueLabel.Text = UiText.Get(vertical ? "DockHeightValue" : "DockWidthValue");
            DockWidthValueDescription.Text = UiText.Get(vertical ? "DockHeightValueDescription" : "DockWidthValueDescription");
        }

        var mode = Choice(DockWidthModeBox) ?? nameof(DockWidthMode.Auto);
        var fixedMode = mode is nameof(DockWidthMode.Pixels) or nameof(DockWidthMode.Percent);
        DockWidthValueRow.Visibility = fixedMode ? Visibility.Visible : Visibility.Collapsed;
        if (fixedMode)
        {
            var settings = _preferences.Current;
            var isPercent = mode == nameof(DockWidthMode.Percent);
            var value = isPercent ? settings.DockWidthPercent : settings.DockWidthPixels;
            DockWidthSlider.Minimum = isPercent ? 10 : 100;
            DockWidthSlider.Maximum = isPercent ? 100 : (vertical ? 3000 : 5000);
            // A 50-step tick over the 10–100 percent range only exposes 10, 60 and 100. Snap to whole percents
            // instead so the user can pick any percentage; pixels keep the coarse 50 px cadence.
            DockWidthSlider.TickFrequency = isPercent ? 1 : 50;
            DockWidthSlider.IsSnapToTickEnabled = true;
            DockWidthSlider.Value = Math.Clamp(value, DockWidthSlider.Minimum, DockWidthSlider.Maximum);
            UpdateDockWidthLabel();
        }
    }

    private void UpdateDockWidthLabel()
    {
        if (DockWidthValue is null || DockWidthSlider is null) return;
        var suffix = (Choice(DockWidthModeBox) ?? nameof(DockWidthMode.Auto)) == nameof(DockWidthMode.Percent) ? "%" : "px";
        DockWidthValue.Text = Math.Round(DockWidthSlider.Value) + suffix;
    }

    private void UpdateDockScaleLabel() => DockScaleValue.Text = UiText.Format("OpacityPercent", (int)Math.Round(DockScaleSlider.Value));

    private void UpdateCornerRadiusLabel() => CornerRadiusValue.Text = $"{(int)Math.Round(CornerRadiusSlider.Value)} px";

    private void CornerRadiusSlider_ValueChanged(object sender, RoutedPropertyChangedEventArgs<double> e)
    {
        if (_initializing) return;
        UpdateCornerRadiusLabel();
        (Application.Current.MainWindow as MainWindow)?.SetGeometryPreview((int)Math.Round(CornerRadiusSlider.Value));
        PreferenceEdited(sender, e);
    }

    private void DockScaleSlider_ValueChanged(object sender, RoutedPropertyChangedEventArgs<double> e)
    {
        if (_initializing) return;
        UpdateDockScaleLabel();
        PreferenceEdited(sender, e);
    }

    // Applying the scale commits the work-area reservation, which needs a fresh replacement session. Doing that on
    // every tick of a drag would be expensive, so commit once when the user lets go of the slider instead.
    private void DockScaleSlider_PreviewMouseUp(object sender, MouseButtonEventArgs e) => CommitDockScale();

    private void DockScaleSlider_KeyUp(object sender, KeyEventArgs e) => CommitDockScale();

    private void CommitDockScale()
    {
        if (_initializing || _closed) return;
        _autoSaveTimer.Stop();
        SaveSettings();
    }

    private void Minimize_Click(object sender, RoutedEventArgs e) => SystemCommands.MinimizeWindow(this);
    private void Maximize_Click(object sender, RoutedEventArgs e)
    {
        if (WindowState == WindowState.Maximized) SystemCommands.RestoreWindow(this);
        else SystemCommands.MaximizeWindow(this);
    }
    private void Close_Click(object sender, RoutedEventArgs e) => Close();

    internal void CloseForShutdown()
    {
        // Window_Closing returns immediately during application shutdown, so flush the debounced edits here.
        if (_autoSaveTimer.IsEnabled)
        {
            _autoSaveTimer.Stop();
            SaveSettings();
        }
        _closingApplication = true;
        Close();
    }

    private void Window_PreviewKeyDown(object sender, KeyEventArgs e)
    {
        if (e.Key != Key.Escape) return;
        e.Handled = true;
        Close();
    }

    [StructLayout(LayoutKind.Sequential)] private struct NativePoint(int x, int y) { public int X = x; public int Y = y; }
    [StructLayout(LayoutKind.Sequential)] private struct NativeRect { public int Left, Top, Right, Bottom; }
    [StructLayout(LayoutKind.Sequential)] private struct MonitorInfo { public int Size; public NativeRect Monitor, Work; public uint Flags; }
    [StructLayout(LayoutKind.Sequential)] private struct MinMaxInfo { public NativePoint Reserved, MaxSize, MaxPosition, MinTrackSize, MaxTrackSize; }
    [DllImport("user32.dll")] private static extern nint MonitorFromWindow(nint window, uint flags);
    [DllImport("user32.dll", EntryPoint = "GetMonitorInfoW", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)] private static extern bool GetMonitorInfo(nint monitor, ref MonitorInfo info);
    [DllImport("user32.dll")] private static extern uint GetDpiForWindow(nint window);
}
