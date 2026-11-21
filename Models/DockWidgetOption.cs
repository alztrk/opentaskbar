using System.ComponentModel;
using System.Globalization;
using System.Windows;
using OpenTaskbar.Services;

namespace OpenTaskbar.Models;

public sealed class DockWidgetOption(
    string id,
    string label,
    string description,
    bool isInstalled,
    bool isEnabled,
    DockWidgetItem preview,
    DockWidgetCategory category = DockWidgetCategory.TimeAndWeather) : INotifyPropertyChanged
{
    private bool _isInstalled = isInstalled;
    private bool _isEnabled = isEnabled;
    private string _label = label;
    private string _description = description;
    private bool _isSettingsExpanded;

    public string Id { get; } = id;
    public string Category => CategoryEnum switch
    {
        DockWidgetCategory.TimeAndWeather => "Time & Weather",
        DockWidgetCategory.SystemAndHardware => "System & Hardware",
        DockWidgetCategory.AiQuotas => "AI Quotas",
        DockWidgetCategory.Productivity => "Productivity",
        _ => "Media & Controls"
    };
    public int CategoryOrder => Category switch
    {
        "Time & Weather" => 0,
        "System & Hardware" => 1,
        "AI Quotas" => 2,
        "Productivity" => 3,
        _ => 4
    };
    public string Label => _label;
    public string Description => _description;
    public DockWidgetItem Preview { get; } = preview;

    private FrameworkElement? _livePreview;

    /// <summary>The real dock widget instance rendered in the catalog. Falls back to the static template when absent.</summary>
    public FrameworkElement? LivePreview
    {
        get => _livePreview;
        set
        {
            if (ReferenceEquals(_livePreview, value)) return;
            _livePreview = value;
            PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(LivePreview)));
            PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(PreviewContent)));
        }
    }

    public object PreviewContent => (object?)_livePreview ?? Preview;

    private UIElement? _previewFlyoutContent;
    private bool _showFlyoutPreview;

    /// <summary>Flyout panel rendered inline in the catalog when the user asks to preview it.</summary>
    public UIElement? PreviewFlyoutContent
    {
        get => _previewFlyoutContent;
        set
        {
            if (ReferenceEquals(_previewFlyoutContent, value)) return;
            _previewFlyoutContent = value;
            PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(PreviewFlyoutContent)));
            PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(PreviewFlyoutVisibility)));
        }
    }

    public bool ShowFlyoutPreview
    {
        get => _showFlyoutPreview;
        set
        {
            if (_showFlyoutPreview == value) return;
            _showFlyoutPreview = value;
            PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(ShowFlyoutPreview)));
            PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(PreviewFlyoutVisibility)));
            PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(FlyoutPreviewGlyph)));
        }
    }

    public Visibility PreviewFlyoutVisibility =>
        _showFlyoutPreview && _previewFlyoutContent is not null ? Visibility.Visible : Visibility.Collapsed;

    public string FlyoutPreviewGlyph => _showFlyoutPreview ? "\uE70E" : "\uE70D";

    private bool _isHardwareAvailable = true;
    /// <summary>False when the widget's required hardware is absent (e.g. no battery on a desktop).</summary>
    public bool IsHardwareAvailable
    {
        get => _isHardwareAvailable;
        set
        {
            if (_isHardwareAvailable == value) return;
            _isHardwareAvailable = value;
            PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(IsHardwareAvailable)));
            PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(UnavailableVisibility)));
        }
    }

    public Visibility UnavailableVisibility => _isHardwareAvailable ? Visibility.Collapsed : Visibility.Visible;

    private object? _nativePreview;
    public object? NativePreview
    {
        get => _nativePreview ?? Preview;
        set
        {
            if (ReferenceEquals(_nativePreview, value)) return;
            _nativePreview = value;
            PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(NativePreview)));
        }
    }
    public FrameworkElement? SettingsView { get; set; }
    public DockWidgetCategory CategoryEnum { get; } = category;

    public string CategoryLabel => CategoryEnum switch
    {
        DockWidgetCategory.TimeAndWeather => UiText.Get("WidgetCategoryTime"),
        DockWidgetCategory.SystemAndHardware => UiText.Get("WidgetCategorySystem"),
        DockWidgetCategory.AiQuotas => UiText.Get("WidgetCategoryAi"),
        DockWidgetCategory.MediaAndControls => UiText.Get("WidgetCategoryMedia"),
        DockWidgetCategory.Productivity => UiText.Get("WidgetCategoryProductivity"),
        _ => string.Empty
    };

    public bool HasSettings => string.Equals(Id, "Clock", StringComparison.OrdinalIgnoreCase)
        || string.Equals(Id, "Weather", StringComparison.OrdinalIgnoreCase)
        || string.Equals(Id, "Focus", StringComparison.OrdinalIgnoreCase)
        || string.Equals(Id, "SystemStats", StringComparison.OrdinalIgnoreCase)
        || string.Equals(Id, "Media", StringComparison.OrdinalIgnoreCase)
        || string.Equals(Id, "Audio", StringComparison.OrdinalIgnoreCase)
        || string.Equals(Id, "QuickSettings", StringComparison.OrdinalIgnoreCase);

    public string InstallActionLabel => UiText.Get(IsInstalled ? "WidgetRemove" : "WidgetInstall");
    public string InstallStatus => UiText.Get(IsInstalled ? "WidgetInstalled" : "WidgetAvailable");

    public Visibility HasSettingsVisibility => HasSettings ? Visibility.Visible : Visibility.Collapsed;

    public bool IsSettingsExpanded
    {
        get => _isSettingsExpanded;
        set
        {
            if (_isSettingsExpanded == value) return;
            _isSettingsExpanded = value;
            PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(IsSettingsExpanded)));
            PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(SettingsGlyph)));
        }
    }

    public string SettingsGlyph => _isSettingsExpanded ? "\uE70E" : "\uE70D";

    public string IconKey => Id switch
    {
        "Clock" or "Date" => "IconClock",
        "Weather" => "IconCloudSun",
        "DayProgress" => "IconCalendar",
        "Stopwatch" => "IconTimer",
        "Countdown" => "IconHourglass",
        "SystemStats" => "IconCpu",
        "NetSpeed" => "IconWifi",
        "Battery" => "IconBattery",
        "BluetoothBattery" => "IconBluetooth",
        "Storage" => "IconHardDrive",
        "AiUsageClaude" => "LogoClaude",
        "AiUsageCodex" => "LogoCodex",
        "AiUsageAntigravity" => "LogoAntigravity",
        "AiUsageOpenCode" => "LogoOpenCode",
        "AiUsageCopilot" => "LogoCopilot",
        "AiUsageDeepSeek" => "LogoDeepSeek",
        "AiUsageOpenRouter" => "LogoOpenRouter",
        "Media" => "IconMusic",
        "Audio" => "IconVolume",
        "QuickSettings" => "IconSliders",
        "Focus" => "IconTarget",
        _ => "IconLayers"
    };

    public bool IsFilledIcon => Id.StartsWith("AiUsage", StringComparison.OrdinalIgnoreCase);

    public object? IconGeometry => Application.Current?.TryFindResource(IconKey);

    public bool MatchesSearch(string query, CultureInfo culture) =>
        culture.CompareInfo.IndexOf(Label, query, CompareOptions.IgnoreCase) >= 0
        || culture.CompareInfo.IndexOf(Description, query, CompareOptions.IgnoreCase) >= 0;

    public void UpdateText(string label, string description)
    {
        var changed = false;
        if (_label != label)
        {
            _label = label;
            Preview.Label = label;
            PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(Label)));
            changed = true;
        }
        if (_description != description)
        {
            _description = description;
            PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(Description)));
            changed = true;
        }
        if (changed)
        {
            PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(CategoryLabel)));
        }
    }

    public void RefreshInstallLabels()
    {
        PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(InstallActionLabel)));
        PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(InstallStatus)));
    }

    public bool IsInstalled
    {
        get => _isInstalled;
        set
        {
            if (_isInstalled == value) return;
            Set(ref _isInstalled, value, nameof(IsInstalled));
            PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(InstallActionLabel)));
            PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(InstallStatus)));
        }
    }

    public bool IsEnabled
    {
        get => _isEnabled;
        set => Set(ref _isEnabled, value, nameof(IsEnabled));
    }

    public event PropertyChangedEventHandler? PropertyChanged;

    private void Set(ref bool field, bool value, string propertyName)
    {
        if (field == value) return;
        field = value;
        PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(propertyName));
    }
}
