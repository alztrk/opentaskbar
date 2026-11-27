using System.Globalization;
using System.Windows;
using System.Windows.Automation;
using System.Windows.Controls;
using System.Windows.Media;
using OpenTaskbar.Services;
using OpenTaskbar.Views.Widgets;

namespace OpenTaskbar.Widgets;

/// <summary>
/// V19 compact day progress indicator: circular ring + tabular elapsed percentage + "day" label. No flyout.
/// </summary>
public sealed class DayProgressWidget : DockWidget
{
    public static string Id => "DayProgress";

    private readonly Border _card;
    private readonly RingMeter _ring;
    private readonly TextBlock _percentText;
    private readonly TextBlock _unitText;
    private int _renderedPercent = -1;
    private string _renderedUnit = string.Empty;

    public override bool SupportsCompactMode => true;
    public override int ShrinkPriority => 60;
    public override bool SupportsStaticPreview => true;
    public override double FullWidthEstimate => 96;
    public override double CompactWidthEstimate => 68;

    public DayProgressWidget()
    {
        _ring = new RingMeter
        {
            Width = 22,
            Height = 22,
            StrokeThickness = 2.8,
            Text = string.Empty,
            Caption = string.Empty,
            VerticalAlignment = VerticalAlignment.Center,
            Margin = new Thickness(0, 0, 7, 0),
            IsHitTestVisible = false
        };
        _ring.SetResourceReference(RingMeter.TrackBrushProperty, "WidgetTrack");
        _ring.SetResourceReference(RingMeter.ValueBrushProperty, "WidgetBlue");

        _percentText = new TextBlock
        {
            Text = "0%",
            FontSize = 13,
            FontWeight = FontWeights.SemiBold,
            VerticalAlignment = VerticalAlignment.Center,
            Typography = { NumeralAlignment = FontNumeralAlignment.Tabular }
        };
        _percentText.SetResourceReference(TextBlock.ForegroundProperty, "TextPrimary");

        _unitText = new TextBlock
        {
            FontSize = 11.5,
            VerticalAlignment = VerticalAlignment.Center,
            Margin = new Thickness(5, 0, 0, 0)
        };
        _unitText.SetResourceReference(TextBlock.ForegroundProperty, "TextSecondary");

        var row = new StackPanel
        {
            Orientation = Orientation.Horizontal,
            VerticalAlignment = VerticalAlignment.Center
        };
        row.Children.Add(_ring);
        row.Children.Add(_percentText);
        row.Children.Add(_unitText);

        _card = new Border
        {
            Padding = new Thickness(10, 0, 10, 0),
            Child = row
        };
        _card.SetResourceReference(StyleProperty, "WidgetCardBorder");

        Content = _card;
        HasData = true;
    }

    protected override void OnActivated() => Render(DateTime.Now, force: true);
    protected override void OnRenderPreview() => Render(DateTime.Now, force: true);
    protected override void OnLanguageChanged() => Render(DateTime.Now, force: true);
    protected override void OnTick(DateTime now) => Render(now, force: false);

    protected override void OnCompactChanged(bool compact) => ApplyDensityLayout();
    protected override void OnHoverExpandedChanged(bool expanded) => ApplyDensityLayout();

    public override UIElement BuildDynamicIslandExpandedContent()
    {
        var now = DateTime.Now;
        var fraction = Math.Clamp(now.TimeOfDay.TotalSeconds / TimeSpan.FromDays(1).TotalSeconds, 0, 1);
        var value = fraction.ToString("P0", Settings.Language.Culture);
        var detail = now.ToString("t", Settings.Language.Culture);
        return DynamicIslandStatusPanel.Create(
            UiText.Get("WidgetDayProgress"), value, detail, fraction, Color.FromRgb(0x91, 0xDE, 0xDC));
    }

    private void ApplyDensityLayout()
    {
        var compact = IsCompact && !IsHoverExpanded;
        _unitText.Visibility = compact ? Visibility.Collapsed : Visibility.Visible;
        _card.Padding = compact ? new Thickness(8, 0, 8, 0) : new Thickness(10, 0, 10, 0);
        RaiseContentWidthChanged();
    }

    internal void Render(DateTime now, bool force)
    {
        var fraction = Math.Clamp(now.TimeOfDay.TotalSeconds / 86400.0, 0.0, 1.0);
        var percent = (int)Math.Round(fraction * 100);
        var unit = UiText.Get("DayProgressUnit");

        if (!force && percent == _renderedPercent && unit == _renderedUnit) return;
        _renderedPercent = percent;
        _renderedUnit = unit;

        _ring.Value = fraction;
        _percentText.Text = $"{percent.ToString(CultureInfo.InvariantCulture)}%";
        _unitText.Text = unit;
        var summary = $"{UiText.Get("WidgetDayProgress")}: {_percentText.Text}";
        _card.ToolTip = summary;
        AutomationProperties.SetName(_card, summary);
    }
}
