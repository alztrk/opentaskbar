using System.Windows;
using System.Windows.Media;

namespace OpenTaskbar.Widgets;

/// <summary>
/// Split top/bottom arc gauge (top = 5h session, bottom = 7d weekly) with a center diamond.
/// </summary>
public sealed class SplitArcGauge : FrameworkElement
{
    private const double ArcSpanDegrees = 136.0;

    public static readonly DependencyProperty TopValueProperty = DependencyProperty.Register(
        nameof(TopValue), typeof(double), typeof(SplitArcGauge),
        new FrameworkPropertyMetadata(0.0, FrameworkPropertyMetadataOptions.AffectsRender, null, CoerceValue));

    public static readonly DependencyProperty BottomValueProperty = DependencyProperty.Register(
        nameof(BottomValue), typeof(double), typeof(SplitArcGauge),
        new FrameworkPropertyMetadata(0.0, FrameworkPropertyMetadataOptions.AffectsRender, null, CoerceValue));

    public static readonly DependencyProperty StrokeThicknessProperty = DependencyProperty.Register(
        nameof(StrokeThickness), typeof(double), typeof(SplitArcGauge),
        new FrameworkPropertyMetadata(2.6, FrameworkPropertyMetadataOptions.AffectsRender));

    public static readonly DependencyProperty TrackBrushProperty = DependencyProperty.Register(
        nameof(TrackBrush), typeof(Brush), typeof(SplitArcGauge),
        new FrameworkPropertyMetadata(Brushes.Gray, FrameworkPropertyMetadataOptions.AffectsRender));

    public static readonly DependencyProperty TopBrushProperty = DependencyProperty.Register(
        nameof(TopBrush), typeof(Brush), typeof(SplitArcGauge),
        new FrameworkPropertyMetadata(Brushes.DeepSkyBlue, FrameworkPropertyMetadataOptions.AffectsRender));

    public static readonly DependencyProperty BottomBrushProperty = DependencyProperty.Register(
        nameof(BottomBrush), typeof(Brush), typeof(SplitArcGauge),
        new FrameworkPropertyMetadata(Brushes.MediumPurple, FrameworkPropertyMetadataOptions.AffectsRender));

    public static readonly DependencyProperty CenterBrushProperty = DependencyProperty.Register(
        nameof(CenterBrush), typeof(Brush), typeof(SplitArcGauge),
        new FrameworkPropertyMetadata(Brushes.Gray, FrameworkPropertyMetadataOptions.AffectsRender));

    public static readonly DependencyProperty CenterGlyphProperty = DependencyProperty.Register(
        nameof(CenterGlyph), typeof(Geometry), typeof(SplitArcGauge),
        new FrameworkPropertyMetadata(null, FrameworkPropertyMetadataOptions.AffectsRender));

    public double TopValue { get => (double)GetValue(TopValueProperty); set => SetValue(TopValueProperty, value); }
    public double BottomValue { get => (double)GetValue(BottomValueProperty); set => SetValue(BottomValueProperty, value); }
    public double StrokeThickness { get => (double)GetValue(StrokeThicknessProperty); set => SetValue(StrokeThicknessProperty, value); }
    public Brush TrackBrush { get => (Brush)GetValue(TrackBrushProperty); set => SetValue(TrackBrushProperty, value); }
    public Brush TopBrush { get => (Brush)GetValue(TopBrushProperty); set => SetValue(TopBrushProperty, value); }
    public Brush BottomBrush { get => (Brush)GetValue(BottomBrushProperty); set => SetValue(BottomBrushProperty, value); }
    public Brush CenterBrush { get => (Brush)GetValue(CenterBrushProperty); set => SetValue(CenterBrushProperty, value); }
    public Geometry? CenterGlyph { get => (Geometry?)GetValue(CenterGlyphProperty); set => SetValue(CenterGlyphProperty, value); }

    private static object CoerceValue(DependencyObject d, object value) =>
        value is double v && double.IsFinite(v) ? Math.Clamp(v, 0, 1) : 0.0;

    protected override void OnRender(DrawingContext context)
    {
        var size = Math.Min(ActualWidth, ActualHeight);
        if (size <= StrokeThickness * 2) return;

        var center = new Point(ActualWidth / 2, ActualHeight / 2);
        var radius = (size - StrokeThickness) / 2;
        var trackPen = CreatePen(TrackBrush, StrokeThickness);

        // Top arc: left-to-right clockwise across the top (-158 deg to -22 deg).
        const double topStartDeg = -90.0 - ArcSpanDegrees / 2.0;
        DrawArc(context, center, radius, topStartDeg, ArcSpanDegrees, SweepDirection.Clockwise, trackPen);
        if (TopValue > 0.005)
        {
            var topPen = CreatePen(TopBrush, StrokeThickness);
            DrawArc(context, center, radius, topStartDeg, ArcSpanDegrees * TopValue, SweepDirection.Clockwise, topPen);
        }

        // Bottom arc: left-to-right counter-clockwise across the bottom (+158 deg to +22 deg).
        const double bottomStartDeg = 90.0 + ArcSpanDegrees / 2.0;
        DrawArc(context, center, radius, bottomStartDeg, -ArcSpanDegrees, SweepDirection.Counterclockwise, trackPen);
        if (BottomValue > 0.005)
        {
            var bottomPen = CreatePen(BottomBrush, StrokeThickness);
            DrawArc(context, center, radius, bottomStartDeg, -ArcSpanDegrees * BottomValue, SweepDirection.Counterclockwise, bottomPen);
        }

        if (CenterGlyph is { } glyph && !glyph.Bounds.IsEmpty)
        {
            var bounds = glyph.Bounds;
            var targetSize = Math.Max(10.0, size * 0.54);
            var scale = targetSize / Math.Max(bounds.Width, bounds.Height);
            var offsetX = center.X - (bounds.X + bounds.Width / 2.0) * scale;
            var offsetY = center.Y - (bounds.Y + bounds.Height / 2.0) * scale;

            context.PushTransform(new TranslateTransform(offsetX, offsetY));
            context.PushTransform(new ScaleTransform(scale, scale));
            context.DrawGeometry(CenterBrush, null, glyph);
            context.Pop();
            context.Pop();
        }
        else
        {
            var diamondRadius = Math.Max(2.5, size * 0.16);
            var diamond = new StreamGeometry();
            using (var g = diamond.Open())
            {
                g.BeginFigure(new Point(center.X, center.Y - diamondRadius), true, true);
                g.LineTo(new Point(center.X + diamondRadius, center.Y), true, false);
                g.LineTo(new Point(center.X, center.Y + diamondRadius), true, false);
                g.LineTo(new Point(center.X - diamondRadius, center.Y), true, false);
            }
            diamond.Freeze();
            context.DrawGeometry(CenterBrush, null, diamond);
        }
    }

    private static void DrawArc(
        DrawingContext context,
        Point center,
        double radius,
        double startDegrees,
        double sweepDegrees,
        SweepDirection direction,
        Pen pen)
    {
        var startRad = startDegrees * Math.PI / 180.0;
        var endRad = (startDegrees + sweepDegrees) * Math.PI / 180.0;

        var start = new Point(center.X + radius * Math.Cos(startRad), center.Y + radius * Math.Sin(startRad));
        var end = new Point(center.X + radius * Math.Cos(endRad), center.Y + radius * Math.Sin(endRad));

        var geometry = new StreamGeometry();
        using (var g = geometry.Open())
        {
            g.BeginFigure(start, false, false);
            g.ArcTo(end, new Size(radius, radius), 0, Math.Abs(sweepDegrees) > 180.0, direction, true, false);
        }
        geometry.Freeze();
        context.DrawGeometry(null, pen, geometry);
    }

    private static Pen CreatePen(Brush brush, double thickness)
    {
        var pen = new Pen(brush, thickness)
        {
            StartLineCap = PenLineCap.Round,
            EndLineCap = PenLineCap.Round
        };
        pen.Freeze();
        return pen;
    }
}
