using System.Windows;
using System.Windows.Media;

namespace OpenTaskbar.Views;

/// <summary>
/// Attached properties that add a small outline glyph to Settings buttons and combo boxes.
/// Kept separate from Tag/CommandParameter so existing control state is untouched.
/// </summary>
public static class IconGlyph
{
    public static readonly DependencyProperty GlyphProperty = DependencyProperty.RegisterAttached(
        "Glyph", typeof(Geometry), typeof(IconGlyph), new PropertyMetadata(null));

    public static void SetGlyph(DependencyObject element, Geometry? value) => element.SetValue(GlyphProperty, value);
    public static Geometry? GetGlyph(DependencyObject element) => element.GetValue(GlyphProperty) as Geometry;

    public static readonly DependencyProperty SizeProperty = DependencyProperty.RegisterAttached(
        "Size", typeof(double), typeof(IconGlyph), new PropertyMetadata(14.0));

    public static void SetSize(DependencyObject element, double value) => element.SetValue(SizeProperty, value);
    public static double GetSize(DependencyObject element) => (double)element.GetValue(SizeProperty);

    public static readonly DependencyProperty MarginProperty = DependencyProperty.RegisterAttached(
        "Margin", typeof(Thickness), typeof(IconGlyph), new PropertyMetadata(new Thickness(0, 0, 8, 0)));

    public static void SetMargin(DependencyObject element, Thickness value) => element.SetValue(MarginProperty, value);
    public static Thickness GetMargin(DependencyObject element) => (Thickness)element.GetValue(MarginProperty);
}
