using System.Windows;
using System.Windows.Automation;
using System.Windows.Controls;
using System.Windows.Input;
using System.Windows.Media;
using OpenTaskbar.Models;
using OpenTaskbar.Services;

namespace OpenTaskbar.Widgets;

/// <summary>
/// Weather widget: shows a compact inline card on the dock (icon, temperature, place/condition, and today's
/// high/low matching the Settings preview, #84) and opens a click flyout with feels-like, humidity, wind,
/// and 3-day forecast details. Compact mode keeps icon and current temperature only.
/// </summary>
public sealed class WeatherWidget : DockWidget
{
    private static readonly TimeSpan RefreshInterval = TimeSpan.FromMinutes(30);
    private static readonly TimeSpan RetryInterval = TimeSpan.FromMinutes(2);
    private static readonly SharedWidgetRequest<string, (WeatherReport? Report, string? Error)> SharedWeather =
        new(TimeSpan.FromMinutes(1));
    private static readonly SolidColorBrush WarmSunBrush = Freeze(Color.FromRgb(0xE5, 0xAE, 0x5E));
    private static readonly SolidColorBrush RainBrush = Freeze(Color.FromRgb(0x6E, 0xAA, 0xF3));

    private static SolidColorBrush Freeze(Color color)
    {
        var b = new SolidColorBrush(color);
        b.Freeze();
        return b;
    }

    private readonly DockFlyout _flyout = new();
    private Border? _flyoutHost;
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
    public override double FullWidthEstimate => 240;
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
            Cursor = Cursors.Hand,
            Child = _root
        };
        _card.SetResourceReference(StyleProperty, "WidgetCardBorder");
        _card.MouseLeftButtonUp += Card_MouseLeftButtonUp;

        Content = _card;
        HasData = false;
    }

    private void Card_MouseLeftButtonUp(object sender, MouseButtonEventArgs e)
    {
        if (_report is null) return;
        e.Handled = true;
        _flyout.Toggle(_card, () =>
        {
            var host = new Border { Width = 296 };
            _flyoutHost = host;
            host.Child = BuildFlyoutContent();
            return host;
        }, animate: true, focusable: false);
    }

    private void RefreshOpenFlyout()
    {
        if (_flyout.IsOpen && _flyoutHost is not null)
        {
            _flyoutHost.Child = BuildFlyoutContent();
        }
    }

    protected override void OnLanguageChanged() => Render();
    protected override void OnCompactChanged(bool compact) => Render();
    protected override void OnHoverExpandedChanged(bool expanded) => Render();

    private string LocationKey => $"{Settings.WeatherCity.Trim()}|{Settings.Language}";

    protected override void OnActivated()
    {
        _ = LoadAsync();
    }

    protected override void OnDeactivated()
    {
        _flyout.Close();
        _loading?.Cancel();
        _loading = null;
        _loadingFor = null;
    }

    protected override void OnSettingsChanged()
    {
        if (IsActive && _loadedFor != LocationKey && _loadingFor != LocationKey) _ = LoadAsync();
        // Unit changes are converted locally, so repaint immediately instead of waiting for the next fetch.
        Render();
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
        if (_loading is { IsCancellationRequested: false } && _loadingFor == key) return;
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
            var city = Settings.WeatherCity;
            var language = Settings.Language;
            var (report, error) = await SharedWeather.ReadAsync(key,
                () => WeatherService.LoadAsync(city, language, CancellationToken.None), loading.Token);
            if (!ReferenceEquals(_loading, loading) || !IsActive || LocationKey != key) return;
            if (report is not null || _loadedFor != key) _report = report;
            if (error is not null && _loadedFor != key && !string.IsNullOrWhiteSpace(Settings.WeatherCity)) Report(error);
            _error = _report is null ? error : null;
            if (report is not null) _loadedFor = key;
            Render();
        }
        catch (OperationCanceledException) when (loading.IsCancellationRequested) { }
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

        var compact = IsCompact && !IsHoverExpanded;
        _card.Padding = compact ? new Thickness(8, 0, 8, 0) : new Thickness(10, 0, 10, 0);
        var culture = Settings.Language.Culture;
        var (iconKey, conditionKey) = WeatherService.Describe(report.Code);
        var conditionText = UiText.Get(conditionKey);

        // Current weather block: icon + temperature + place & condition.
        var mainIcon = new ContentControl
        {
            Content = TryFindResource(iconKey),
            Width = compact ? 16 : 20,
            Height = compact ? 16 : 20,
            Margin = new Thickness(0, 0, 7, 0),
            VerticalAlignment = VerticalAlignment.Center,
            Foreground = IconBrushForCode(report.Code)
        };
        mainIcon.SetResourceReference(StyleProperty, "WidgetIcon");
        _root.Children.Add(mainIcon);

        var tempText = new TextBlock
        {
            Text = FormatDegrees(report.Temperature),
            FontSize = compact ? 15 : 18,
            FontWeight = FontWeights.Bold,
            VerticalAlignment = VerticalAlignment.Center,
            Typography = { NumeralAlignment = FontNumeralAlignment.Tabular }
        };
        tempText.SetResourceReference(TextBlock.ForegroundProperty, "TextPrimary");
        _root.Children.Add(tempText);

        if (!compact)
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

            // Inline full-size card matches the Settings catalog preview: icon, temp, place/condition, and today's range.
            // Feels-like, humidity, wind, and 3-day forecast live in the click flyout so the dock card stays compact (#84).
            if (report.Days.Count > 0)
            {
                _root.Children.Add(CreateSeparator());
                var today = report.Days[0];
                var todayPair = CreateMetricPair(
                    UiText.Get("WeatherToday"),
                    $"{FormatDegrees(today.Max)} / {FormatDegrees(today.Min)}");
                todayPair.Margin = new Thickness(0);
                _root.Children.Add(todayPair);
            }
        }

        var summary = string.IsNullOrEmpty(report.Place)
            ? $"{FormatDegrees(report.Temperature)} · {conditionText}"
            : $"{report.Place} · {FormatDegrees(report.Temperature)} · {conditionText}";
        _card.ToolTip = summary;
        AutomationProperties.SetName(_card, $"{UiText.Get("Weather")}: {summary}");
        RefreshOpenFlyout();
        RaiseContentWidthChanged();
    }

    /// <summary>Builds the detailed weather click flyout with feels-like, humidity, wind, and 3-day forecast.</summary>
    public override UIElement? BuildDynamicIslandExpandedContent() => BuildFlyoutContent();

    internal UIElement BuildFlyoutContent()
    {
        var root = new StackPanel { Width = 296 };
        if (_report is not { } report)
        {
            root.Children.Add(DockFlyout.Header(UiText.Get("Weather")));
            var empty = DockFlyout.Caption(_error ?? UiText.Get("WeatherUnavailable"));
            root.Children.Add(empty);
            return root;
        }

        var culture = Settings.Language.Culture;
        var (iconKey, conditionKey) = WeatherService.Describe(report.Code);
        var conditionText = UiText.Get(conditionKey);

        // Header: place + condition on left, temperature + condition icon on right.
        var header = new DockPanel { LastChildFill = true, Margin = new Thickness(0, 0, 0, 10) };
        var tempRow = new StackPanel { Orientation = Orientation.Horizontal, VerticalAlignment = VerticalAlignment.Center };
        var heroIcon = new ContentControl
        {
            Content = TryFindResource(iconKey),
            Width = 22,
            Height = 22,
            Margin = new Thickness(0, 0, 6, 0),
            VerticalAlignment = VerticalAlignment.Center,
            Foreground = IconBrushForCode(report.Code)
        };
        heroIcon.SetResourceReference(StyleProperty, "WidgetIcon");
        var heroTemp = new TextBlock
        {
            Text = FormatDegrees(report.Temperature),
            FontSize = 26,
            FontWeight = FontWeights.SemiBold,
            VerticalAlignment = VerticalAlignment.Center,
            Typography = { NumeralAlignment = FontNumeralAlignment.Tabular }
        };
        heroTemp.SetResourceReference(TextBlock.ForegroundProperty, "TextPrimary");
        tempRow.Children.Add(heroIcon);
        tempRow.Children.Add(heroTemp);
        DockPanel.SetDock(tempRow, Dock.Right);
        header.Children.Add(tempRow);

        var titleStack = new StackPanel { VerticalAlignment = VerticalAlignment.Center, Margin = new Thickness(0, 0, 8, 0) };
        var placeBlock = new TextBlock
        {
            Text = string.IsNullOrWhiteSpace(report.Place) ? UiText.Get("Weather") : report.Place,
            FontSize = 13,
            FontWeight = FontWeights.SemiBold,
            TextTrimming = TextTrimming.CharacterEllipsis
        };
        placeBlock.SetResourceReference(TextBlock.ForegroundProperty, "TextPrimary");
        var condBlock = new TextBlock { Text = conditionText, FontSize = 11, Margin = new Thickness(0, 2, 0, 0) };
        condBlock.SetResourceReference(TextBlock.ForegroundProperty, "TextSecondary");
        titleStack.Children.Add(placeBlock);
        titleStack.Children.Add(condBlock);
        header.Children.Add(titleStack);
        root.Children.Add(header);

        root.Children.Add(DockFlyout.Divider(6));

        // Clean flat 2x2 key-value metrics without nested bento boxes.
        var metricsGrid = new System.Windows.Controls.Primitives.UniformGrid { Columns = 2, Margin = new Thickness(0, 2, 0, 2) };
        var feelsLabel = UiText.Format("WeatherFeelsLike", string.Empty).Trim();
        if (feelsLabel.Length > 0) feelsLabel = char.ToUpper(feelsLabel[0], culture) + feelsLabel[1..];

        void AddDetailMetric(string label, string value)
        {
            var row = new DockPanel { Margin = new Thickness(0, 3, 10, 3) };
            var valText = new TextBlock
            {
                Text = value,
                FontSize = 12,
                FontWeight = FontWeights.SemiBold,
                VerticalAlignment = VerticalAlignment.Center,
                Typography = { NumeralAlignment = FontNumeralAlignment.Tabular }
            };
            valText.SetResourceReference(TextBlock.ForegroundProperty, "TextPrimary");
            DockPanel.SetDock(valText, Dock.Right);
            row.Children.Add(valText);

            var lblText = new TextBlock
            {
                Text = label,
                FontSize = 11,
                VerticalAlignment = VerticalAlignment.Center,
                Margin = new Thickness(0, 0, 6, 0),
                TextTrimming = TextTrimming.CharacterEllipsis
            };
            lblText.SetResourceReference(TextBlock.ForegroundProperty, "TextSecondary");
            row.Children.Add(lblText);
            metricsGrid.Children.Add(row);
        }

        if (report.Days.Count > 0)
        {
            var today = report.Days[0];
            AddDetailMetric(UiText.Get("WeatherToday"), $"{FormatDegrees(today.Max)} / {FormatDegrees(today.Min)}");
        }
        AddDetailMetric(feelsLabel, FormatDegrees(report.ApparentTemperature));
        AddDetailMetric(UiText.Get("WeatherHumidity"), (report.Humidity / 100.0).ToString("P0", culture));
        var windSpeed = UsesFahrenheit ? report.WindSpeed * 0.621371 : report.WindSpeed;
        var windText = UsesFahrenheit ? "WeatherWindSpeedMph" : "WeatherWindSpeed";
        AddDetailMetric(UiText.Get("WeatherWind"), UiText.Format(windText, Math.Round(windSpeed).ToString(culture)));
        root.Children.Add(metricsGrid);

        // Next 3 days forecast rows.
        var forecastDays = report.Days.Skip(1).Take(3).ToList();
        if (forecastDays.Count > 0)
        {
            root.Children.Add(DockFlyout.Divider(8));

            for (var i = 0; i < forecastDays.Count; i++)
            {
                var day = forecastDays[i];
                var (dayIconKey, dayConditionKey) = WeatherService.Describe(day.Code);
                var row = new DockPanel { LastChildFill = true, Margin = new Thickness(0, i == 0 ? 0 : 6, 0, 0) };

                var rangeText = new TextBlock
                {
                    Text = $"{FormatDegrees(day.Max)} / {FormatDegrees(day.Min)}",
                    FontSize = 12,
                    FontWeight = FontWeights.SemiBold,
                    VerticalAlignment = VerticalAlignment.Center,
                    Typography = { NumeralAlignment = FontNumeralAlignment.Tabular }
                };
                rangeText.SetResourceReference(TextBlock.ForegroundProperty, "TextPrimary");
                DockPanel.SetDock(rangeText, Dock.Right);
                row.Children.Add(rangeText);

                var leftRow = new StackPanel { Orientation = Orientation.Horizontal, VerticalAlignment = VerticalAlignment.Center };
                var dayIcon = new ContentControl
                {
                    Content = TryFindResource(dayIconKey),
                    Width = 15,
                    Height = 15,
                    Margin = new Thickness(0, 0, 8, 0),
                    VerticalAlignment = VerticalAlignment.Center,
                    Foreground = IconBrushForCode(day.Code)
                };
                dayIcon.SetResourceReference(StyleProperty, "WidgetIcon");
                leftRow.Children.Add(dayIcon);

                var dayLabel = i == 0
                    ? UiText.Get("WeatherTomorrow")
                    : culture.TextInfo.ToTitleCase(day.Date.ToString("ddd", culture));
                var dayText = new TextBlock
                {
                    Text = $"{dayLabel} · {UiText.Get(dayConditionKey)}",
                    FontSize = 12,
                    VerticalAlignment = VerticalAlignment.Center,
                    TextTrimming = TextTrimming.CharacterEllipsis
                };
                dayText.SetResourceReference(TextBlock.ForegroundProperty, "TextSecondary");
                leftRow.Children.Add(dayText);

                row.Children.Add(leftRow);
                root.Children.Add(row);
            }
        }

        return root;
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

    private bool UsesFahrenheit => Settings.WeatherUnit == TemperatureUnit.Fahrenheit;

    private string FormatDegrees(double celsius)
    {
        var value = UsesFahrenheit ? celsius * 9.0 / 5.0 + 32 : celsius;
        var suffix = UsesFahrenheit ? "°F" : "°";
        return $"{Math.Round(value).ToString(Settings.Language.Culture)}{suffix}";
    }
}
