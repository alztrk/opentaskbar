using System.Windows;
using System.Windows.Controls;
using System.Windows.Media;

namespace OpenTaskbar.Views.Widgets;

internal sealed record DynamicIslandStatusRow(string Label, string Value, string Detail, double Progress);

internal static class DynamicIslandStatusPanel
{
    private static readonly Brush ForegroundBrush = CreateFrozenBrush(0xFA, 0xFA, 0xFA);
    private static readonly Brush SecondaryBrush = CreateFrozenBrush(0xB7, 0xB8, 0xC1);
    private static readonly Brush TrackBrush = CreateFrozenBrush(0x33, 0x38, 0x43);

    private static SolidColorBrush CreateFrozenBrush(byte r, byte g, byte b)
    {
        var brush = new SolidColorBrush(Color.FromRgb(r, g, b));
        brush.Freeze();
        return brush;
    }

    public static UIElement Create(string title, string value, string detail, double? progress, Color accent)
    {
        var root = new StackPanel
        {
            VerticalAlignment = VerticalAlignment.Center,
            Margin = new Thickness(4, 6, 4, 6)
        };
        root.Children.Add(CreateText(title, 11, SecondaryBrush, FontWeights.Medium));
        root.Children.Add(CreateText(value, 25, ForegroundBrush, FontWeights.SemiBold, new Thickness(0, 2, 0, 0)));
        if (progress is { } fraction)
            root.Children.Add(CreateMeter(fraction, accent));
        if (!string.IsNullOrWhiteSpace(detail))
            root.Children.Add(CreateText(detail, 11.5, SecondaryBrush, FontWeights.Normal, new Thickness(0, 7, 0, 0)));
        return root;
    }

    public static UIElement CreateRows(string title, IReadOnlyList<DynamicIslandStatusRow> rows, Color accent)
    {
        var root = new StackPanel
        {
            VerticalAlignment = VerticalAlignment.Center,
            Margin = new Thickness(4, 2, 4, 2)
        };
        root.Children.Add(CreateText(title, 11, SecondaryBrush, FontWeights.Medium, new Thickness(0, 0, 0, 8)));

        foreach (var row in rows)
        {
            var block = new StackPanel { Margin = new Thickness(0, 0, 0, 9) };
            var header = new Grid();
            header.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
            header.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
            var label = CreateText(row.Label, 11, SecondaryBrush, FontWeights.Medium);
            var value = CreateText(row.Value, 12.5, ForegroundBrush, FontWeights.SemiBold);
            value.HorizontalAlignment = HorizontalAlignment.Right;
            Grid.SetColumn(value, 1);
            header.Children.Add(label);
            header.Children.Add(value);
            block.Children.Add(header);
            block.Children.Add(CreateMeter(row.Progress, accent));
            if (!string.IsNullOrWhiteSpace(row.Detail))
                block.Children.Add(CreateText(row.Detail, 10.5, SecondaryBrush, FontWeights.Normal, new Thickness(0, 3, 0, 0)));
            root.Children.Add(block);
        }

        return root;
    }

    private static TextBlock CreateText(
        string text,
        double size,
        Brush foreground,
        FontWeight weight,
        Thickness? margin = null) => new()
    {
        Text = text,
        FontSize = size,
        FontWeight = weight,
        Foreground = foreground,
        Margin = margin ?? new Thickness(0),
        TextTrimming = TextTrimming.CharacterEllipsis,
        Typography = { NumeralAlignment = FontNumeralAlignment.Tabular }
    };

    private static FrameworkElement CreateMeter(double value, Color accent)
    {
        var progress = Math.Clamp(double.IsFinite(value) ? value : 0, 0, 1);
        var visible = Math.Max(progress, 0.001);
        var remainder = Math.Max(1 - progress, 0.001);
        var meter = new Grid
        {
            Height = 6,
            Margin = new Thickness(0, 5, 0, 0),
            ClipToBounds = true
        };
        meter.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(visible, GridUnitType.Star) });
        meter.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(remainder, GridUnitType.Star) });
        var fill = new Border
        {
            Background = new SolidColorBrush(accent),
            CornerRadius = new CornerRadius(4, 0, 0, 4)
        };
        var track = new Border
        {
            Background = TrackBrush,
            CornerRadius = new CornerRadius(0, 4, 4, 0)
        };
        Grid.SetColumn(track, 1);
        meter.Children.Add(fill);
        meter.Children.Add(track);
        return meter;
    }
}
