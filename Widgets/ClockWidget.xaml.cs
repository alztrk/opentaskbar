using System.Globalization;
using System.Windows;
using System.Windows.Automation;
using System.Windows.Controls;
using System.Windows.Controls.Primitives;
using System.Windows.Input;
using System.Windows.Media;
using OpenTaskbar.Models;
using OpenTaskbar.Services;

namespace OpenTaskbar.Widgets;

public partial class ClockWidget : DockWidget
{
    public enum TimeOfDayPeriod { Morning, Noon, Evening }

    public sealed record TimeOfDayTheme(Brush Background, Brush Border, Brush Accent, string IconKey);

    private static readonly TimeOfDayTheme MorningTheme = CreateTimeTheme(
        Color.FromArgb(0x38, 0xF5, 0x9E, 0x0B),
        Color.FromArgb(0x10, 0xFB, 0x92, 0x3C),
        Color.FromArgb(0x48, 0xF5, 0x9E, 0x0B),
        Color.FromRgb(0xF5, 0x9E, 0x0B),
        "IconSun");

    private static readonly TimeOfDayTheme NoonTheme = CreateTimeTheme(
        Color.FromArgb(0x34, 0x38, 0xBD, 0xF8),
        Color.FromArgb(0x0E, 0x02, 0x84, 0xC7),
        Color.FromArgb(0x48, 0x38, 0xBD, 0xF8),
        Color.FromRgb(0x0E, 0xA5, 0xE9),
        "IconSun");

    private static readonly TimeOfDayTheme EveningTheme = CreateTimeTheme(
        Color.FromArgb(0x3C, 0x63, 0x66, 0xF1),
        Color.FromArgb(0x12, 0x43, 0x38, 0xCA),
        Color.FromArgb(0x4E, 0x81, 0x8C, 0xF8),
        Color.FromRgb(0xA7, 0x8B, 0xFA),
        "IconMoon");

    private static TimeOfDayTheme CreateTimeTheme(
        Color bgStart, Color bgEnd, Color borderColor, Color accentColor, string iconKey)
    {
        var bg = new LinearGradientBrush(bgStart, bgEnd, new Point(0, 0), new Point(1, 1));
        bg.Freeze();
        var border = new SolidColorBrush(borderColor);
        border.Freeze();
        var accent = new SolidColorBrush(accentColor);
        accent.Freeze();
        return new TimeOfDayTheme(bg, border, accent, iconKey);
    }

    public static TimeOfDayPeriod GetPeriod(int hour) => hour switch
    {
        >= 6 and < 11 => TimeOfDayPeriod.Morning,
        >= 11 and < 18 => TimeOfDayPeriod.Noon,
        _ => TimeOfDayPeriod.Evening
    };

    public static TimeOfDayTheme GetTheme(TimeOfDayPeriod period) => period switch
    {
        TimeOfDayPeriod.Morning => MorningTheme,
        TimeOfDayPeriod.Noon => NoonTheme,
        _ => EveningTheme
    };

    private readonly DockFlyout _calendar = new();
    private string _lastRenderKey = string.Empty;
    private DateOnly _shownMonth;
    private DateOnly? _selectedDate;
    private TextBlock? _flyoutLiveTime;

    public ClockWidget()
    {
        InitializeComponent();
        _calendar.Closed += (_, _) => _flyoutLiveTime = null;
    }

    public override bool SupportsCompactMode => true;
    public override int ShrinkPriority => 80;
    public override double FullWidthEstimate => Settings.ClockDateLayout == ClockDateLayout.Below ? 104 : 148;
    public override double CompactWidthEstimate => 84;

    // The clock renders from the local system time only, so a single frame is safe in Settings.
    public override bool SupportsStaticPreview => true;
    protected override void OnRenderPreview() => OnTick(DateTime.Now);

    private CultureInfo Culture => Settings.Language.Culture;

    protected override void OnActivated()
    {
        _lastRenderKey = string.Empty;
        OnTick(DateTime.Now);
    }

    protected override void OnSettingsChanged()
    {
        _lastRenderKey = string.Empty;
        if (IsActive) OnTick(DateTime.Now);
    }

    protected override void OnCompactChanged(bool compact) => ApplyDensityLayout();
    protected override void OnHoverExpandedChanged(bool expanded) => ApplyDensityLayout();

    private void ApplyDensityLayout()
    {
        _lastRenderKey = string.Empty;
        OnTick(DateTime.Now);
    }

    protected override void OnDeactivated() => _calendar.Close();

    protected override void OnTick(DateTime now)
    {
        var culture = Culture;
        var timeFormat = Settings.Clock24Hour
            ? (Settings.ClockShowSeconds ? "HH:mm:ss" : "HH:mm")
            : (Settings.ClockShowSeconds ? "hh:mm:ss tt" : "hh:mm tt");
        var timeString = now.ToString(timeFormat, culture);
        if (_flyoutLiveTime is not null) _flyoutLiveTime.Text = timeString;

        var compact = IsCompact && !IsHoverExpanded;
        var stacked = !compact && Settings.ClockDateLayout == ClockDateLayout.Below && Settings.DateStyle != ClockDateStyle.Hidden;
        var renderKey = $"{now:yyyyMMdd}|{timeString}|{Settings.DateStyle}|{Settings.DateFormat}|{Settings.ClockDateLayout}|{Settings.Language}|{compact}";
        if (renderKey == _lastRenderKey) return;

        _lastRenderKey = renderKey;
        TimeText.Text = timeString;

        if (stacked)
        {
            TimeAndDatePanel.Orientation = Orientation.Vertical;
            TimeText.FontSize = 13;
            TimeText.LineHeight = 14.5;
            TimeText.HorizontalAlignment = HorizontalAlignment.Center;
            DateColumn.Visibility = Visibility.Visible;
            DateColumn.Margin = new Thickness(0, 1, 0, 0);
            DateColumn.HorizontalAlignment = HorizontalAlignment.Center;
            DateText.FontSize = 10;
            DateText.LineHeight = 11;
            DateText.HorizontalAlignment = HorizontalAlignment.Center;
            DateText.TextAlignment = TextAlignment.Center;
            DayOfWeekText.Visibility = Visibility.Collapsed;
            var formattedDate = FormatDate(now, culture);
            DateText.Text = Settings.DateStyle == ClockDateStyle.DayAndShortDate
                ? $"{now.ToString("ddd", culture)} · {formattedDate}"
                : formattedDate;
        }
        else
        {
            TimeAndDatePanel.Orientation = Orientation.Horizontal;
            TimeText.FontSize = 15;
            TimeText.LineHeight = double.NaN;
            TimeText.HorizontalAlignment = HorizontalAlignment.Left;
            DateColumn.Margin = new Thickness(9, 0, 0, 0);
            DateColumn.HorizontalAlignment = HorizontalAlignment.Left;
            DateText.HorizontalAlignment = HorizontalAlignment.Left;
            DateText.TextAlignment = TextAlignment.Left;

            if (compact || Settings.DateStyle == ClockDateStyle.Hidden)
            {
                DateColumn.Visibility = Visibility.Collapsed;
            }
            else
            {
                DateColumn.Visibility = Visibility.Visible;
                switch (Settings.DateStyle)
                {
                    case ClockDateStyle.DayAndShortDate:
                        DayOfWeekText.Visibility = Visibility.Visible;
                        DateText.FontSize = 11.5;
                        DateText.LineHeight = 13;
                        DateText.Text = FormatDate(now, culture);
                        DayOfWeekText.Text = now.ToString("ddd", culture);
                        break;
                    case ClockDateStyle.ShortDateOnly:
                    case ClockDateStyle.NumericDate:
                        DayOfWeekText.Visibility = Visibility.Collapsed;
                        DateText.FontSize = 13;
                        DateText.LineHeight = double.NaN;
                        DateText.Text = FormatDate(now, culture);
                        break;
                }
            }
        }
        ClockButton.ToolTip = now.ToString("D", culture);
        RaiseContentWidthChanged();
    }

    private string FormatDate(DateTime now, CultureInfo culture) => Settings.DateFormat switch
    {
        ClockDateFormat.DdMmYyyy => now.ToString("dd/MM/yyyy", CultureInfo.InvariantCulture),
        ClockDateFormat.DdMmYy => now.ToString("dd/MM/yy", CultureInfo.InvariantCulture),
        ClockDateFormat.MmDdYyyy => now.ToString("MM/dd/yyyy", CultureInfo.InvariantCulture),
        ClockDateFormat.YyyyMmDd => now.ToString("yyyy-MM-dd", CultureInfo.InvariantCulture),
        ClockDateFormat.DdMmYyyyDots => now.ToString("dd.MM.yyyy", CultureInfo.InvariantCulture),
        _ => Settings.DateStyle switch
        {
            ClockDateStyle.ShortDateOnly => now.ToString("MMM d", culture),
            ClockDateStyle.NumericDate => now.ToString("d", culture),
            _ => now.ToString(culture.DateTimeFormat.MonthDayPattern.Replace("MMMM", "MMM"), culture)
        }
    };

    public override UIElement? BuildPreviewFlyoutContent()
    {
        var host = new ContentControl();
        host.Content = BuildCalendar(host);
        return host;
    }

    private void ClockButton_Click(object sender, RoutedEventArgs e)
    {
        _calendar.Toggle(ClockButton, () =>
        {
            var today = DateOnly.FromDateTime(DateTime.Today);
            _shownMonth = new DateOnly(today.Year, today.Month, 1);
            _selectedDate = null;
            var root = new ContentControl();
            root.Content = BuildCalendar(root);
            return root;
        });
    }

    internal static string FormatLocalTimeZone()
    {
        var tz = TimeZoneInfo.Local;
        var offset = tz.GetUtcOffset(DateTime.Now);
        var sign = offset < TimeSpan.Zero ? "-" : "+";
        var abs = offset.Duration();
        var utc = abs.Minutes == 0
            ? $"UTC{sign}{abs.Hours}"
            : $"UTC{sign}{abs.Hours}:{abs.Minutes:00}";
        var name = tz.IsDaylightSavingTime(DateTime.Now) ? tz.DaylightName : tz.StandardName;
        return string.IsNullOrWhiteSpace(name) ? utc : $"{utc} · {name}";
    }

    /// <summary>V19 calendar flyout with live clock, system timezone, month/year navigation, selection, and Today jump.</summary>
    internal UIElement BuildCalendar(ContentControl host)
    {
        var culture = Culture;
        var now = DateTime.Now;
        var today = DateOnly.FromDateTime(now.Date);
        if (_shownMonth == default) _shownMonth = new DateOnly(today.Year, today.Month, 1);

        var root = new StackPanel { Width = 320 };

        // Top header: crisp live time + full date; timezone shown cleanly via tooltip without awkward right-edge clipping.
        var topRow = new StackPanel { Margin = new Thickness(0, 0, 0, 10) };

        var timeFormat = Settings.Clock24Hour
            ? (Settings.ClockShowSeconds ? "HH:mm:ss" : "HH:mm")
            : (Settings.ClockShowSeconds ? "hh:mm:ss tt" : "hh:mm tt");
        var liveTime = new TextBlock
        {
            Text = now.ToString(timeFormat, culture),
            FontSize = 26,
            FontWeight = FontWeights.SemiBold,
            Typography = { NumeralAlignment = FontNumeralAlignment.Tabular }
        };
        liveTime.SetResourceReference(TextBlock.ForegroundProperty, "TextPrimary");
        _flyoutLiveTime = liveTime;

        var fullDate = new TextBlock
        {
            Text = now.ToString("dddd, d MMMM", culture),
            FontSize = 12,
            Margin = new Thickness(0, 1, 0, 0),
            ToolTip = FormatLocalTimeZone()
        };
        fullDate.SetResourceReference(TextBlock.ForegroundProperty, "TextSecondary");
        topRow.Children.Add(liveTime);
        topRow.Children.Add(fullDate);
        root.Children.Add(topRow);
        root.Children.Add(DockFlyout.Divider(6));

        // Month & year navigation bar.
        var nav = new Grid { Margin = new Thickness(0, 0, 0, 10) };
        nav.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        nav.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        nav.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        nav.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        nav.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });

        var prevYear = NavButton("IconChevronsLeft", -12, "PreviousYear", host);
        var prevMonth = NavButton("IconChevronLeft", -1, "PreviousMonth", host);
        var nextMonth = NavButton("IconChevronRight", 1, "NextMonth", host);
        var nextYear = NavButton("IconChevronsRight", 12, "NextYear", host);

        var monthTitleText = culture.TextInfo.ToTitleCase(_shownMonth.ToString("MMMM yyyy", culture));
        var monthButton = new Button
        {
            Content = new TextBlock
            {
                Text = monthTitleText,
                FontWeight = FontWeights.SemiBold,
                FontSize = 13,
                HorizontalAlignment = HorizontalAlignment.Center,
                VerticalAlignment = VerticalAlignment.Center
            },
            HorizontalAlignment = HorizontalAlignment.Center,
            Padding = new Thickness(8, 4, 8, 4),
            ToolTip = UiText.Get("GoToToday")
        };
        monthButton.SetResourceReference(StyleProperty, "FlyoutButton");
        monthButton.Click += (_, _) =>
        {
            _shownMonth = new DateOnly(today.Year, today.Month, 1);
            _selectedDate = today;
            host.Content = BuildCalendar(host);
        };

        Grid.SetColumn(prevYear, 0);
        Grid.SetColumn(prevMonth, 1);
        Grid.SetColumn(monthButton, 2);
        Grid.SetColumn(nextMonth, 3);
        Grid.SetColumn(nextYear, 4);
        nav.Children.Add(prevYear);
        nav.Children.Add(prevMonth);
        nav.Children.Add(monthButton);
        nav.Children.Add(nextMonth);
        nav.Children.Add(nextYear);
        root.Children.Add(nav);

        // 7-column calendar grid with localized FirstDayOfWeek.
        var grid = new UniformGrid { Columns = 7, Margin = new Thickness(0, 0, 0, 4) };
        var firstDay = culture.DateTimeFormat.FirstDayOfWeek;
        for (var index = 0; index < 7; index++)
        {
            var day = (DayOfWeek)(((int)firstDay + index) % 7);
            var dow = new TextBlock
            {
                Text = culture.DateTimeFormat.GetShortestDayName(day),
                FontSize = 11,
                FontWeight = FontWeights.Medium,
                HorizontalAlignment = HorizontalAlignment.Center,
                VerticalAlignment = VerticalAlignment.Center,
                Margin = new Thickness(0, 0, 0, 6)
            };
            dow.SetResourceReference(TextBlock.ForegroundProperty, "TextSecondary");
            grid.Children.Add(dow);
        }

        var offset = ((int)_shownMonth.DayOfWeek - (int)firstDay + 7) % 7;
        var start = _shownMonth.AddDays(-offset);
        for (var cell = 0; cell < 42; cell++)
        {
            var date = start.AddDays(cell);
            var isToday = date == today;
            var isSelected = _selectedDate == date && !isToday;
            var isCurrentMonth = date.Month == _shownMonth.Month;

            var label = new TextBlock
            {
                Text = date.Day.ToString(culture),
                HorizontalAlignment = HorizontalAlignment.Center,
                VerticalAlignment = VerticalAlignment.Center,
                FontSize = 12,
                FontWeight = isToday || isSelected ? FontWeights.SemiBold : FontWeights.Normal,
                Typography = { NumeralAlignment = FontNumeralAlignment.Tabular }
            };

            var cellBorder = new Border
            {
                Height = 31,
                CornerRadius = new CornerRadius(7),
                Margin = new Thickness(1.5),
                Cursor = Cursors.Hand,
                Child = label
            };

            if (isToday)
            {
                cellBorder.SetResourceReference(Border.BackgroundProperty, "WidgetSelectedFill");
                cellBorder.SetResourceReference(Border.BorderBrushProperty, "WidgetSelectedStroke");
                cellBorder.BorderThickness = new Thickness(1);
                label.SetResourceReference(TextBlock.ForegroundProperty, "WidgetSelectedForeground");
            }
            else if (isSelected)
            {
                cellBorder.SetResourceReference(Border.BackgroundProperty, "SurfaceSelectedStrong");
                cellBorder.SetResourceReference(Border.BorderBrushProperty, "StrokeSubtle");
                cellBorder.BorderThickness = new Thickness(1);
                label.SetResourceReference(TextBlock.ForegroundProperty, "TextPrimary");
            }
            else
            {
                cellBorder.Background = Brushes.Transparent;
                if (isCurrentMonth)
                {
                    label.SetResourceReference(TextBlock.ForegroundProperty, "TextPrimary");
                }
                else
                {
                    label.SetResourceReference(TextBlock.ForegroundProperty, "TextSecondary");
                    label.Opacity = 0.42;
                }
            }

            var capturedDate = date;
            cellBorder.MouseLeftButtonUp += (_, _) =>
            {
                _selectedDate = capturedDate;
                if (capturedDate.Month != _shownMonth.Month || capturedDate.Year != _shownMonth.Year)
                {
                    _shownMonth = new DateOnly(capturedDate.Year, capturedDate.Month, 1);
                }
                host.Content = BuildCalendar(host);
            };

            grid.Children.Add(cellBorder);
        }

        root.Children.Add(grid);

        // Footer divider + Today summary & quick jump button.
        var divider = new Border { Height = 1, Margin = new Thickness(0, 8, 0, 8) };
        divider.SetResourceReference(Border.BackgroundProperty, "StrokeSubtle");
        root.Children.Add(divider);

        var footer = new DockPanel();
        var todayBtn = new Button
        {
            Content = new TextBlock { Text = UiText.Get("GoToToday"), FontSize = 11, FontWeight = FontWeights.SemiBold },
            Padding = new Thickness(8, 4, 8, 4),
            VerticalAlignment = VerticalAlignment.Center
        };
        todayBtn.SetResourceReference(StyleProperty, "FlyoutButton");
        todayBtn.Click += (_, _) =>
        {
            _shownMonth = new DateOnly(today.Year, today.Month, 1);
            _selectedDate = today;
            host.Content = BuildCalendar(host);
        };
        DockPanel.SetDock(todayBtn, Dock.Right);
        footer.Children.Add(todayBtn);

        var todaySummary = new TextBlock
        {
            Text = $"{UiText.Get("Today")} · {now.ToString("d MMMM, dddd", culture)}",
            FontSize = 10.5,
            VerticalAlignment = VerticalAlignment.Center,
            TextTrimming = TextTrimming.CharacterEllipsis
        };
        todaySummary.SetResourceReference(TextBlock.ForegroundProperty, "TextSecondary");
        footer.Children.Add(todaySummary);
        root.Children.Add(footer);

        return root;
    }

    private Button NavButton(string icon, int months, string labelKey, ContentControl host)
    {
        var glyph = new ContentControl { Content = FindResource(icon), Width = 14, Height = 14 };
        glyph.SetResourceReference(StyleProperty, "WidgetIcon");
        glyph.SetResourceReference(ForegroundProperty, "TextPrimary");
        var button = new Button { Content = glyph, Width = 26, Height = 26 };
        button.SetResourceReference(StyleProperty, "WidgetIconButton");
        var label = UiText.Get(labelKey);
        button.ToolTip = label;
        AutomationProperties.SetName(button, label);
        button.Click += (_, _) =>
        {
            var totalMonths = (_shownMonth.Year - 1) * 12 + (_shownMonth.Month - 1) + months;
            var clamped = Math.Clamp(totalMonths, 1, (9998 - 1) * 12 + 11);
            _shownMonth = new DateOnly(clamped / 12 + 1, clamped % 12 + 1, 1);
            host.Content = BuildCalendar(host);
        };
        return button;
    }
}
