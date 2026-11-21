using System;
using System.Collections.Generic;
using System.Windows;
using System.Windows.Controls;
using OpenTaskbar.Models;

namespace OpenTaskbar.Views;

/// <summary>
/// Two-column widget catalogue: an expanded widget occupies an entire row and
/// moves subsequent items down. This panel is data-driven; no widget names are hard-coded.
/// </summary>
public sealed class ExpandableWidgetGrid : Panel
{
    public static readonly DependencyProperty MinItemWidthProperty =
        DependencyProperty.Register(nameof(MinItemWidth), typeof(double), typeof(ExpandableWidgetGrid),
            new FrameworkPropertyMetadata(275.0, FrameworkPropertyMetadataOptions.AffectsMeasure |
                FrameworkPropertyMetadataOptions.AffectsArrange));

    public double MinItemWidth
    {
        get => (double)GetValue(MinItemWidthProperty);
        set => SetValue(MinItemWidthProperty, value);
    }

    private readonly List<(int Index, Rect Bounds)> _positions = [];

    private bool Expanded(UIElement child) =>
        child is FrameworkElement { DataContext: DockWidgetOption option } && option.IsSettingsExpanded;

    protected override Size MeasureOverride(Size availableSize)
    {
        _positions.Clear();
        if (InternalChildren.Count == 0) return new Size(0, 0);

        var width = availableSize.Width;
        if (double.IsInfinity(width) || width <= 0)
            width = ActualWidth > 0 ? ActualWidth : 680;

        // Single-column layout: each category is a full-width row and widgets stack inside it.
        const int columns = 1;
        const double gap = 10;
        var half = width;
        var y = 0.0;
        var pending = -1;
        var pendingHeight = 0.0;

        void Flush()
        {
            if (pending < 0) return;
            _positions.Add((pending, new Rect(0, y, half, pendingHeight)));
            pending = -1;
            y += pendingHeight + gap;
            pendingHeight = 0;
        }

        for (var i = 0; i < InternalChildren.Count; i++)
        {
            var child = InternalChildren[i];
            if (Expanded(child))
            {
                Flush();
                child.Measure(new Size(width, double.PositiveInfinity));
                var h = child.DesiredSize.Height;
                _positions.Add((i, new Rect(0, y, width, h)));
                y += h + gap;
                continue;
            }

            child.Measure(new Size(half, double.PositiveInfinity));
            if (pending < 0 || columns == 1)
            {
                Flush();
                pending = i;
                pendingHeight = child.DesiredSize.Height;
            }
            else
            {
                var h = Math.Max(pendingHeight, child.DesiredSize.Height);
                var left = _positions.Count;
                // The previous item was measured already; place the complete row together.
                _positions.Add((pending, new Rect(0, y, half, h)));
                _positions.Add((i, new Rect(half + gap, y, half, h)));
                pending = -1;
                pendingHeight = 0;
                y += h + gap;
            }
        }
        Flush();
        return new Size(width, Math.Max(0, y - gap));
    }

    protected override Size ArrangeOverride(Size finalSize)
    {
        // When responsive width changes, WPF measures again before arranging.
        foreach (var (index, bounds) in _positions)
            InternalChildren[index].Arrange(bounds);
        return finalSize;
    }
}
