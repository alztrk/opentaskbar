using System.Windows;
using System.Windows.Automation;
using System.Windows.Controls;
using System.Windows.Media;
using OpenTaskbar.Models;
using OpenTaskbar.Services;

namespace OpenTaskbar.Widgets;

/// <summary>
/// V19 inline weather widget: shows current conditions, today's high/low, feels-like, humidity, wind,
/// and 3-day forecast directly on the taskbar without any flyout. Compact mode keeps current weather only.
/// </summary>
public sealed class WeatherWidget : DockWidget
{
    private static readonly TimeSpan RefreshInterval = TimeSpan.FromMinutes(30);
    private static readonly TimeSpan RetryInterval = TimeSpan.FromMinutes(2);
    private static readonly SolidColorBrush WarmSunBrush = Freeze(Color.FromRgb(0xE5, 0xAE, 0x5E));
    private static readonly SolidColorBrush RainBrush = Freeze(Color.FromRgb(0x6E, 0xAA, 0xF3));

    private static SolidColorBrush Freeze(Color color)
    {
        var b = new SolidColorBrush(color);
        b.Freeze();
        return b;
    }

    private readonly Border _card;
    private readonly StackPanel _root;
    private WeatherReport? _report;
    private string? _error;
    private DateTime _loadedAt = DateTime.MinValue;
    private string? _loadedFor;
    private string? _loadingFor;
    private CancellationTokenSource? _loading;

    public override bool SupportsCompactMode => true;
    public override int ShrinkPriority => 15;
    public override double FullWidthEstimate => 520;
    public override double CompactWidthEstimate => 118;

    public WeatherWidget()
    {
        _root = new StackPanel
        {
            Orientation = Orientation.Horizontal,
            VerticalAlignment = VerticalAlignment.Center
        };

        _card = new Border
        {
            Padding = new Thickness(10, 0, 10, 0),
            Child = _root
        };
        _card.SetResourceReference(StyleProperty, "WidgetCardBorder");

        Content = _card;
        HasData = false;
    }

    protected override void OnLanguageChanged() => Render();
    protected override void OnCompactChanged(bool compact) => Render();

    private string LocationKey => $"{Settings.WeatherCity.Trim()}|{Settings.Language}";

    protected override void OnActivated()
    {
        UiText.Current.PropertyChanged += UiText_PropertyChanged;
        _ = LoadAsync();
    }

    protected override void OnDeactivated()
    {
        UiText.Current.PropertyChanged -= UiText_PropertyChanged;
        _loading?.Cancel();
    }

    private void UiText_PropertyChanged(object? sender, System.ComponentModel.PropertyChangedEventArgs e) => Render();

    protected override void OnSettingsChanged()
    {
        if (IsActive && _loadedFor != LocationKey && _loadingFor != LocationKey) _ = LoadAsync();
    }

    protected override void OnTick(DateTime now)
    {
        if (DateTime.UtcNow - _loadedAt > (_report is null ? RetryInterval : RefreshInterval) && _loading is null) _ = LoadAsync();
    }

    internal void ApplyReport(WeatherReport? report, string? error = null)
    {
        _report = report;
        _error = error;
        Render();
    }

    private async Task LoadAsync()
    {
        var key = LocationKey;
        if (_loading is not null && _loadingFor == key) return;
        _loading?.Cancel();
        var loading = new CancellationTokenSource();
        _loading = loading;
        _loadingFor = key;
        _loadedAt = DateTime.UtcNow;
        if (_loadedFor != key)
        {
            _report = null;
            _error = null;
            Render();
        }
        try
        {
            var (report, error) = await WeatherService.LoadAsync(Settings.WeatherCity, Settings.Language, loading.Token);
            if (!ReferenceEquals(_loading, loading) || !IsActive || LocationKey != key) return;
            if (report is not null || _loadedFor != key) _report = report;
            if (error is not null && _loadedFor != key && !string.IsNullOrWhiteSpace(Settings.WeatherCity)) Report(error);
            _error = _report is null ? error : null;
            _loadedFor = key;
            Render();
        }
        finally
        {
            if (ReferenceEquals(_loading, loading))
            {
                _loading = null;
                _loadingFor = null;
            }
            loading.Dispose();
        }
    }

    private void Render()
    {
        _root.Children.Clear();
        HasData = _report is not null || _error is not null;

        if (_report is not { } report)
        {
            var unavailable = new TextBlock
            {
                Text = UiText.Get("WeatherUnavailable"),
                FontSize = 12,
                VerticalAlignment = VerticalAlignment.Center
            };
            unavailable.SetResourceReference(StyleProperty, "WidgetCaption");
            _root.Children.Add(unavailable);
            _card.ToolTip = _error ?? UiText.Get("WeatherUnavailable");
            AutomationProperties.SetName(_card, $"{UiText.Get("Weather")}: {_error ?? UiText.Get("WeatherUnavailable")}");
            RaiseContentWidthChanged();
            return;
        }

        var culture = Settings.Language.Culture;
        var (iconKey, conditionKey) = WeatherService.Describe(report.Code);
        var conditionText = UiText.Get(conditionKey);

        // Current weather block: icon + temperature + place & condition.
        var mainIcon = new ContentControl
        {
            Content = TryFindResource(iconKey),
            Width = IsCompact ? 16 : 20,
            Height = IsCompact ? 16 : 20,
            Margin = new Thickness(0, 0, 7, 0),
            VerticalAlignment = VerticalAlignment.Center,
            Foreground = IconBrushForCode(report.Code)
        };
        mainIcon.SetResourceReference(StyleProperty, "WidgetIcon");
        _root.Children.Add(mainIcon);

        var tempText = new TextBlock
        {
            Text = FormatDegrees(report.Temperature),
            FontSize = IsCompact ? 15 : 18,
            FontWeight = FontWeights.Bold,
            VerticalAlignment = VerticalAlignment.Center,
            Typography = { NumeralAlignment = FontNumeralAlignment.Tabular }
        };
        tempText.SetResourceReference(TextBlock.ForegroundProperty, "TextPrimary");
        _root.Children.Add(tempText);

        if (!IsCompact)
        {
            var placeStack = new StackPanel { VerticalAlignment = VerticalAlignment.Center, Margin = new Thickness(7, 0, 0, 0) };
            if (!string.IsNullOrWhiteSpace(report.Place))
            {
                var placeBlock = new TextBlock
                {
                    Text = report.Place,
                    FontSize = 11,
                    FontWeight = FontWeights.SemiBold,
                    LineHeight = 12.5,
                    MaxWidth = 96,
                    TextTrimming = TextTrimming.CharacterEllipsis
                };
                placeBlock.SetResourceReference(TextBlock.ForegroundProperty, "TextPrimary");
                placeStack.Children.Add(placeBlock);
            }
            var condBlock = new TextBlock
            {
                Text = conditionText,
                FontSize = 10,
                LineHeight = 11.5,
                MaxWidth = 108,
                TextTrimming = TextTrimming.CharacterEllipsis
            };
            condBlock.SetResourceReference(TextBlock.ForegroundProperty, "TextSecondary");
            placeStack.Children.Add(condBlock);
            _root.Children.Add(placeStack);

            // Inline metrics: Today max/min, feels-like, humidity, wind.
            _root.Children.Add(CreateSeparator());

            if (report.Days.Count > 0)
            {
                var today = report.Days[0];
                _root.Children.Add(CreateMetricPair(
                    UiText.Get("WeatherToday"),
                    $"{FormatDegrees(today.Max)} / {FormatDegrees(today.Min)}"));
            }

            var feelsLabel = UiText.Format("WeatherFeelsLike", string.Empty).Trim();
            if (feelsLabel.Length > 0) feelsLabel = char.ToUpper(feelsLabel[0], culture) + feelsLabel[1..];
            _root.Children.Add(CreateMetricPair(
                feelsLabel,
                FormatDegrees(report.ApparentTemperature)));

            _root.Children.Add(CreateMetricPair(
                UiText.Get("WeatherHumidity"),
                (report.Humidity / 100.0).ToString("P0", culture)));

            _root.Children.Add(CreateMetricPair(
                UiText.Get("WeatherWind"),
                UiText.Format("WeatherWindSpeed", Math.Round(report.WindSpeed).ToString(culture))));

            // Next 3 days forecast inline.
            var forecastDays = report.Days.Skip(1).Take(3).ToList();
            if (forecastDays.Count > 0)
            {
                _root.Children.Add(CreateSeparator());
                for (var i = 0; i < forecastDays.Count; i++)
                {
                    var day = forecastDays[i];
                    var (dayIconKey, _) = WeatherService.Describe(day.Code);
                    var dayPanel = new StackPanel
                    {
                        Orientation = Orientation.Horizontal,
                        VerticalAlignment = VerticalAlignment.Center,
                        Margin = new Thickness(i == 0 ? 0 : 8, 0, 0, 0)
                    };
                    var dayIcon = new ContentControl
                    {
                        Content = TryFindResource(dayIconKey),
                        Width = 14,
                        Height = 14,
                        Margin = new Thickness(0, 0, 4, 0),
                        VerticalAlignment = VerticalAlignment.Center
                    };
                    dayIcon.SetResourceReference(StyleProperty, "WidgetIcon");
                    dayIcon.SetResourceReference(ForegroundProperty, "TextSecondary");

                    var dayLabel = i == 0
                        ? UiText.Get("WeatherTomorrow")
                        : culture.TextInfo.ToTitleCase(day.Date.ToString("ddd", culture));
                    var dayPair = CreateMetricPair(dayLabel, $"{FormatDegrees(day.Max)} / {FormatDegrees(day.Min)}", compactValue: true);
                    dayPair.Margin = new Thickness(0);

                    dayPanel.Children.Add(dayIcon);
                    dayPanel.Children.Add(dayPair);
                    _root.Children.Add(dayPanel);
                }
            }
        }

        var summary = string.IsNullOrEmpty(report.Place)
            ? $"{FormatDegrees(report.Temperature)} · {conditionText}"
            : $"{report.Place} · {FormatDegrees(report.Temperature)} · {conditionText}";
        _card.ToolTip = summary;
        AutomationProperties.SetName(_card, $"{UiText.Get("Weather")}: {summary}");
        RaiseContentWidthChanged();
    }

    private static Brush IconBrushForCode(int code) => code switch
    {
        >= 51 and <= 82 or >= 95 => RainBrush,
        _ => WarmSunBrush
    };

    private static Border CreateSeparator()
    {
        var sep = new Border
        {
            Width = 1,
            Height = 20,
            Margin = new Thickness(9, 0, 9, 0),
            VerticalAlignment = VerticalAlignment.Center
        };
        sep.SetResourceReference(Border.BackgroundProperty, "StrokeSubtle");
        return sep;
    }

    private static StackPanel CreateMetricPair(string label, string value, bool compactValue = false)
    {
        var stack = new StackPanel
        {
            VerticalAlignment = VerticalAlignment.Center,
            Margin = new Thickness(0, 0, 9, 0)
        };
        var lbl = new TextBlock
        {
            Text = label,
            FontSize = 9.5,
            LineHeight = 11
        };
        lbl.SetResourceReference(TextBlock.ForegroundProperty, "TextSecondary");

        var val = new TextBlock
        {
            Text = value,
            FontSize = compactValue ? 10.5 : 11.5,
            FontWeight = FontWeights.SemiBold,
            LineHeight = 13,
            Typography = { NumeralAlignment = FontNumeralAlignment.Tabular }
        };
        val.SetResourceReference(TextBlock.ForegroundProperty, "TextPrimary");

        stack.Children.Add(lbl);
        stack.Children.Add(val);
        return stack;
    }

    private string FormatDegrees(double value) => $"{Math.Round(value).ToString(Settings.Language.Culture)}°";
}
