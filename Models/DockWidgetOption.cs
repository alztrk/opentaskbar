using System.ComponentModel;
using System.Globalization;
using System.Windows;

namespace OpenTaskbar.Models;

public sealed class DockWidgetOption(string id, string label, string description, bool isInstalled, bool isEnabled, DockWidgetItem preview) : INotifyPropertyChanged
{
    private bool _isInstalled = isInstalled;
    private bool _isEnabled = isEnabled;
    private string _label = label;
    private string _description = description;
    private bool _isSettingsExpanded;

    public string Id { get; } = id;
    public string Category => Id switch
    {
        "Clock" or "DayProgress" or "Weather" or "Stopwatch" or "Countdown" => "Time & Weather",
        "SystemStats" or "NetSpeed" or "BluetoothBattery" or "Battery" or "Storage" => "System & Hardware",
        "AiUsage" => "AI Quotas",
        "Focus" or "Search" or "Desktops" => "Productivity",
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
    public FrameworkElement? SettingsView { get; set; }

    public bool HasSettings => string.Equals(Id, "Clock", StringComparison.OrdinalIgnoreCase)
        || string.Equals(Id, "Weather", StringComparison.OrdinalIgnoreCase)
        || string.Equals(Id, "Focus", StringComparison.OrdinalIgnoreCase)
        || string.Equals(Id, "SystemStats", StringComparison.OrdinalIgnoreCase)
        || string.Equals(Id, "Media", StringComparison.OrdinalIgnoreCase)
        || string.Equals(Id, "Audio", StringComparison.OrdinalIgnoreCase)
        || string.Equals(Id, "QuickSettings", StringComparison.OrdinalIgnoreCase)
        || string.Equals(Id, "Desktops", StringComparison.OrdinalIgnoreCase)
        || string.Equals(Id, "Search", StringComparison.OrdinalIgnoreCase)
        || string.Equals(Id, "AiUsage", StringComparison.OrdinalIgnoreCase);

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

    public bool MatchesSearch(string query, CultureInfo culture) =>
        culture.CompareInfo.IndexOf(Label, query, CompareOptions.IgnoreCase) >= 0
        || culture.CompareInfo.IndexOf(Description, query, CompareOptions.IgnoreCase) >= 0;

    public void UpdateText(string label, string description)
    {
        if (_label != label)
        {
            _label = label;
            Preview.Label = label;
            PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(Label)));
        }
        if (_description != description)
        {
            _description = description;
            PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(Description)));
        }
    }

    public bool IsInstalled
    {
        get => _isInstalled;
        set => Set(ref _isInstalled, value, nameof(IsInstalled));
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
