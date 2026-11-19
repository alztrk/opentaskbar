using System.Windows;
using System.Windows.Controls;
using System.Windows.Input;
using System.Windows.Media;
using System.Windows.Media.Animation;

namespace OpenTaskbar.Views;

public sealed class DockScrollViewer : ScrollViewer
{
    private const double EdgeFadeWidth = 34;
    private Point? _panStart;
    private double _panOffset;
    private double _targetHorizontalOffset;
    private double _targetVerticalOffset;
    private ScrollContentPresenter? _scrollContentPresenter;
    private Button? _previousButton;
    private Button? _nextButton;
    private Button? _externalPreviousButton;
    private Button? _externalNextButton;

    public static readonly DependencyProperty CurrentHorizontalOffsetProperty = DependencyProperty.Register(
        nameof(CurrentHorizontalOffset), typeof(double), typeof(DockScrollViewer),
        new PropertyMetadata(0d, (d, e) => ((DockScrollViewer)d).ScrollToHorizontalOffset((double)e.NewValue)));

    public static readonly DependencyProperty CurrentVerticalOffsetProperty = DependencyProperty.Register(
        nameof(CurrentVerticalOffset), typeof(double), typeof(DockScrollViewer),
        new PropertyMetadata(0d, (d, e) => ((DockScrollViewer)d).ScrollToVerticalOffset((double)e.NewValue)));

    public double CurrentHorizontalOffset
    {
        get => (double)GetValue(CurrentHorizontalOffsetProperty);
        set => SetValue(CurrentHorizontalOffsetProperty, value);
    }

    public double CurrentVerticalOffset
    {
        get => (double)GetValue(CurrentVerticalOffsetProperty);
        set => SetValue(CurrentVerticalOffsetProperty, value);
    }

    public static readonly DependencyProperty HoverHeadroomProperty = DependencyProperty.Register(
        nameof(HoverHeadroom), typeof(double), typeof(DockScrollViewer),
        new PropertyMetadata(0d, LayoutChanged), value => value is double height && double.IsFinite(height) && height >= 0);
    public static readonly DependencyProperty ContentInsetProperty = DependencyProperty.Register(
        nameof(ContentInset), typeof(Thickness), typeof(DockScrollViewer), new PropertyMetadata(new Thickness(), LayoutChanged));

    public double HoverHeadroom { get => (double)GetValue(HoverHeadroomProperty); set => SetValue(HoverHeadroomProperty, value); }
    public Thickness ContentInset { get => (Thickness)GetValue(ContentInsetProperty); set => SetValue(ContentInsetProperty, value); }
    public bool IsPanning => _panStart is not null;

    /// <summary>Returns the width a part of the content needs to be shown without scrolling.</summary>
    public double MeasureContentWidth(UIElement part)
    {
        UpdateLayout();
        part.Measure(new Size(double.PositiveInfinity, double.PositiveInfinity));
        return part.DesiredSize.Width;
    }

    public bool ScrollByWheel(int delta, ModifierKeys modifiers, bool animate = false)
    {
        if (delta == 0) return false;
        // When only the horizontal axis overflows (e.g. too many dock items), a plain wheel scrolls it.
        var horizontalOnly = ScrollableHeight <= 0 && ScrollableWidth > 0;
        if (!horizontalOnly && (modifiers & (ModifierKeys.Shift | ModifierKeys.Control)) == 0) return false;
        var cellSize = TryFindResource("DockCellSize") as double? ?? 52d;
        var multiplier = animate ? 2.0 : 1.0;
        var distance = delta / (double)Mouse.MouseWheelDeltaForOneLine * cellSize * multiplier;
        if (!horizontalOnly && ScrollableHeight > 0)
        {
            if (animate && SystemParameters.ClientAreaAnimation)
            {
                var baseOffset = (Math.Sign(distance) == Math.Sign(VerticalOffset - _targetVerticalOffset))
                    ? _targetVerticalOffset : VerticalOffset;
                AnimateScrollVertical(baseOffset - distance);
            }
            else
            {
                ScrollToVerticalOffset(VerticalOffset - distance);
                _targetVerticalOffset = VerticalOffset;
            }
            return true;
        }
        if (ScrollableWidth > 0)
        {
            if (animate && SystemParameters.ClientAreaAnimation)
            {
                var baseOffset = (Math.Sign(distance) == Math.Sign(HorizontalOffset - _targetHorizontalOffset))
                    ? _targetHorizontalOffset : HorizontalOffset;
                AnimateScrollHorizontal(baseOffset - distance);
            }
            else
            {
                ScrollToHorizontalOffset(HorizontalOffset - distance);
                _targetHorizontalOffset = HorizontalOffset;
            }
            return true;
        }
        return false;
    }

    private void AnimateScrollHorizontal(double target)
    {
        target = Math.Clamp(target, 0, ScrollableWidth);
        _targetHorizontalOffset = target;
        if (!SystemParameters.ClientAreaAnimation)
        {
            ScrollToHorizontalOffset(target);
            return;
        }

        var animation = new DoubleAnimation
        {
            From = HorizontalOffset,
            To = target,
            Duration = TimeSpan.FromMilliseconds(100),
            EasingFunction = new QuarticEase { EasingMode = EasingMode.EaseOut }
        };
        BeginAnimation(CurrentHorizontalOffsetProperty, animation, HandoffBehavior.SnapshotAndReplace);
    }

    private void AnimateScrollVertical(double target)
    {
        target = Math.Clamp(target, 0, ScrollableHeight);
        _targetVerticalOffset = target;
        if (!SystemParameters.ClientAreaAnimation)
        {
            ScrollToVerticalOffset(target);
            return;
        }

        var animation = new DoubleAnimation
        {
            From = VerticalOffset,
            To = target,
            Duration = TimeSpan.FromMilliseconds(100),
            EasingFunction = new QuarticEase { EasingMode = EasingMode.EaseOut }
        };
        BeginAnimation(CurrentVerticalOffsetProperty, animation, HandoffBehavior.SnapshotAndReplace);
    }

    protected override void OnPreviewMouseWheel(MouseWheelEventArgs e)
    {
        if (ScrollByWheel(e.Delta, Keyboard.Modifiers, animate: true))
        {
            e.Handled = true;
            return;
        }

        // Plain wheel (without Shift/Ctrl) also scrolls overflowing dock content directly
        if (e.Delta != 0 && ScrollableWidth > 0)
        {
            var cellSize = TryFindResource("DockCellSize") as double? ?? 52d;
            var distance = e.Delta / (double)Mouse.MouseWheelDeltaForOneLine * cellSize * 2.0;
            if (SystemParameters.ClientAreaAnimation)
            {
                var baseOffset = (Math.Sign(distance) == Math.Sign(HorizontalOffset - _targetHorizontalOffset))
                    ? _targetHorizontalOffset : HorizontalOffset;
                AnimateScrollHorizontal(baseOffset - distance);
            }
            else
            {
                ScrollToHorizontalOffset(HorizontalOffset - distance);
                _targetHorizontalOffset = HorizontalOffset;
            }
            e.Handled = true;
            return;
        }
        if (e.Delta != 0 && ScrollableHeight > 0)
        {
            var cellSize = TryFindResource("DockCellSize") as double? ?? 52d;
            var distance = e.Delta / (double)Mouse.MouseWheelDeltaForOneLine * cellSize * 2.0;
            if (SystemParameters.ClientAreaAnimation)
            {
                var baseOffset = (Math.Sign(distance) == Math.Sign(VerticalOffset - _targetVerticalOffset))
                    ? _targetVerticalOffset : VerticalOffset;
                AnimateScrollVertical(baseOffset - distance);
            }
            else
            {
                ScrollToVerticalOffset(VerticalOffset - distance);
                _targetVerticalOffset = VerticalOffset;
            }
            e.Handled = true;
            return;
        }

        base.OnPreviewMouseWheel(e);
    }

    protected override void OnPreviewMouseDown(MouseButtonEventArgs e)
    {
        if (e.ChangedButton == MouseButton.Middle && ScrollableWidth > 0 && CaptureMouse())
        {
            BeginAnimation(CurrentHorizontalOffsetProperty, null);
            BeginAnimation(CurrentVerticalOffsetProperty, null);
            _panStart = e.GetPosition(this);
            _panOffset = HorizontalOffset;
            e.Handled = true;
        }
        base.OnPreviewMouseDown(e);
    }

    protected override void OnMouseMove(MouseEventArgs e)
    {
        if (_panStart is { } origin)
        {
            var target = _panOffset + origin.X - e.GetPosition(this).X;
            ScrollToHorizontalOffset(target);
            _targetHorizontalOffset = HorizontalOffset;
            e.Handled = true;
        }
        base.OnMouseMove(e);
    }

    protected override void OnPreviewMouseUp(MouseButtonEventArgs e)
    {
        if (e.ChangedButton == MouseButton.Middle && IsPanning)
        {
            ReleaseMouseCapture();
            e.Handled = true;
        }
        base.OnPreviewMouseUp(e);
    }

    public override void OnApplyTemplate()
    {
        base.OnApplyTemplate();

        if (_previousButton is not null)
            _previousButton.Click -= OnPreviousButtonClick;
        if (_nextButton is not null)
            _nextButton.Click -= OnNextButtonClick;

        _scrollContentPresenter = GetTemplateChild("PART_ScrollContentPresenter") as ScrollContentPresenter;
        _previousButton = GetTemplateChild("PART_PreviousButton") as Button;
        _nextButton = GetTemplateChild("PART_NextButton") as Button;

        if (_previousButton is not null)
            _previousButton.Click += OnPreviousButtonClick;
        if (_nextButton is not null)
            _nextButton.Click += OnNextButtonClick;

        UpdateIndicatorLayout();
        UpdateIndicators();
    }

    private void OnPreviousButtonClick(object sender, RoutedEventArgs e) => ScrollStep(forward: false);
    private void OnNextButtonClick(object sender, RoutedEventArgs e) => ScrollStep(forward: true);

    /// <summary>
    /// Registers external overlay buttons (e.g. placed in DockSurface) that mirror the scroll
    /// indicator state. The external buttons must already have their Click events wired to
    /// <see cref="ScrollStep"/> via <see cref="ScrollStepBack"/> and <see cref="ScrollStepForward"/>.
    /// </summary>
    public void SetExternalIndicators(Button? previous, Button? next)
    {
        _externalPreviousButton = previous;
        _externalNextButton = next;
        UpdateIndicatorLayout();
        UpdateIndicators();
    }

    public void ScrollStepBack() => ScrollStep(forward: false);
    public void ScrollStepForward() => ScrollStep(forward: true);

    private void ScrollStep(bool forward)
    {
        var cellSize = TryFindResource("DockCellSize") as double? ?? 52d;
        var step = cellSize * 2.5;
        if (ScrollableHeight > 0)
        {
            var current = (Math.Abs(VerticalOffset - _targetVerticalOffset) > 1) ? _targetVerticalOffset : VerticalOffset;
            var next = forward ? current + step : current - step;
            AnimateScrollVertical(next);
        }
        else if (ScrollableWidth > 0)
        {
            var current = (Math.Abs(HorizontalOffset - _targetHorizontalOffset) > 1) ? _targetHorizontalOffset : HorizontalOffset;
            var next = forward ? current + step : current - step;
            AnimateScrollHorizontal(next);
        }
    }

    private void UpdateIndicatorLayout()
    {
        var topOffset = HoverHeadroom;
        if (ScrollableHeight > 0)
        {
            if (_previousButton is not null)
            {
                _previousButton.Content = TryFindResource("IconChevronUp");
                _previousButton.Background = TryFindResource("DockScrollScrimTop") as Brush;
                _previousButton.HorizontalAlignment = HorizontalAlignment.Stretch;
                _previousButton.VerticalAlignment = VerticalAlignment.Top;
                _previousButton.Width = double.NaN;
                _previousButton.Height = 28;
                _previousButton.HorizontalContentAlignment = HorizontalAlignment.Center;
                _previousButton.VerticalContentAlignment = VerticalAlignment.Top;
                _previousButton.Padding = new Thickness(0, 4, 0, 0);
                _previousButton.Margin = new Thickness(0, topOffset, 0, 0);
                _previousButton.Tag = new CornerRadius(16, 16, 0, 0);
            }
            if (_nextButton is not null)
            {
                _nextButton.Content = TryFindResource("IconChevronDown");
                _nextButton.Background = TryFindResource("DockScrollScrimBottom") as Brush;
                _nextButton.HorizontalAlignment = HorizontalAlignment.Stretch;
                _nextButton.VerticalAlignment = VerticalAlignment.Bottom;
                _nextButton.Width = double.NaN;
                _nextButton.Height = 28;
                _nextButton.HorizontalContentAlignment = HorizontalAlignment.Center;
                _nextButton.VerticalContentAlignment = VerticalAlignment.Bottom;
                _nextButton.Padding = new Thickness(0, 0, 0, 4);
                _nextButton.Margin = new Thickness(0);
                _nextButton.Tag = new CornerRadius(0, 0, 16, 16);
            }
        }
        else
        {
            if (_previousButton is not null)
            {
                _previousButton.Content = TryFindResource("IconChevronLeft");
                _previousButton.Background = TryFindResource("DockScrollScrimStart") as Brush;
                _previousButton.HorizontalAlignment = HorizontalAlignment.Left;
                _previousButton.VerticalAlignment = VerticalAlignment.Stretch;
                _previousButton.Width = 28;
                _previousButton.Height = double.NaN;
                _previousButton.HorizontalContentAlignment = HorizontalAlignment.Left;
                _previousButton.VerticalContentAlignment = VerticalAlignment.Center;
                _previousButton.Padding = new Thickness(5, 0, 0, 0);
                _previousButton.Margin = new Thickness(0, topOffset, 0, 0);
                _previousButton.Tag = new CornerRadius(16, 0, 0, 16);
            }
            if (_nextButton is not null)
            {
                _nextButton.Content = TryFindResource("IconChevronRight");
                _nextButton.Background = TryFindResource("DockScrollScrimEnd") as Brush;
                _nextButton.HorizontalAlignment = HorizontalAlignment.Right;
                _nextButton.VerticalAlignment = VerticalAlignment.Stretch;
                _nextButton.Width = 28;
                _nextButton.Height = double.NaN;
                _nextButton.HorizontalContentAlignment = HorizontalAlignment.Right;
                _nextButton.VerticalContentAlignment = VerticalAlignment.Center;
                _nextButton.Padding = new Thickness(0, 0, 5, 0);
                _nextButton.Margin = new Thickness(0, topOffset, 0, 0);
                _nextButton.Tag = new CornerRadius(0, 16, 16, 0);
            }
        }
    }

    private void UpdateIndicators()
    {
        if (_previousButton is null && _nextButton is null
            && _externalPreviousButton is null && _externalNextButton is null) return;

        if (ScrollableHeight > 0)
        {
            var vHiddenBefore = VerticalOffset > 0.5;
            var vHiddenAfter = VerticalOffset < ScrollableHeight - 0.5;
            var prevVis = vHiddenBefore ? Visibility.Visible : Visibility.Collapsed;
            var nextVis = vHiddenAfter ? Visibility.Visible : Visibility.Collapsed;
            // Template buttons: keep in sync for tests; hide visually when external overlay is active.
            if (_previousButton is not null)
                _previousButton.Visibility = _externalPreviousButton is null ? prevVis : Visibility.Collapsed;
            if (_nextButton is not null)
                _nextButton.Visibility = _externalNextButton is null ? nextVis : Visibility.Collapsed;
            if (_externalPreviousButton is not null)
                _externalPreviousButton.Visibility = prevVis;
            if (_externalNextButton is not null)
                _externalNextButton.Visibility = nextVis;
            return;
        }

        if (ScrollableWidth > 0)
        {
            var hiddenBefore = HorizontalOffset > 0.5;
            var hiddenAfter = HorizontalOffset < ScrollableWidth - 0.5;
            var prevVis = hiddenBefore ? Visibility.Visible : Visibility.Collapsed;
            var nextVis = hiddenAfter ? Visibility.Visible : Visibility.Collapsed;
            if (_previousButton is not null)
                _previousButton.Visibility = _externalPreviousButton is null ? prevVis : Visibility.Collapsed;
            if (_nextButton is not null)
                _nextButton.Visibility = _externalNextButton is null ? nextVis : Visibility.Collapsed;
            if (_externalPreviousButton is not null)
                _externalPreviousButton.Visibility = prevVis;
            if (_externalNextButton is not null)
                _externalNextButton.Visibility = nextVis;
            return;
        }

        if (_previousButton is not null) _previousButton.Visibility = Visibility.Collapsed;
        if (_nextButton is not null) _nextButton.Visibility = Visibility.Collapsed;
        if (_externalPreviousButton is not null) _externalPreviousButton.Visibility = Visibility.Collapsed;
        if (_externalNextButton is not null) _externalNextButton.Visibility = Visibility.Collapsed;
    }

    private void ApplyOpacityMask(Brush? mask)
    {
        if (_scrollContentPresenter is not null)
        {
            _scrollContentPresenter.OpacityMask = mask;
            ClearValue(OpacityMaskProperty);
        }
        else
        {
            OpacityMask = mask;
        }
    }

    protected override void OnLostMouseCapture(MouseEventArgs e)
    {
        _panStart = null;
        base.OnLostMouseCapture(e);
    }

    protected override void OnScrollChanged(ScrollChangedEventArgs e)
    {
        base.OnScrollChanged(e);
        UpdateEdgeFade();
    }

    protected override void OnRenderSizeChanged(SizeChangedInfo sizeInfo)
    {
        base.OnRenderSizeChanged(sizeInfo);
        UpdateEdgeFade();
    }

    /// <summary>Fades the side that has more content, since the scrollbar is hidden and overflow would otherwise go unnoticed.</summary>
    private void UpdateEdgeFade()
    {
        UpdateIndicatorLayout();
        UpdateIndicators();

        if (ScrollableHeight > 0)
        {
            var vHiddenBefore = VerticalOffset > 0.5;
            var vHiddenAfter = VerticalOffset < ScrollableHeight - 0.5;
            if (ActualHeight <= EdgeFadeWidth * 2 || (!vHiddenBefore && !vHiddenAfter))
            {
                ApplyOpacityMask(null);
                return;
            }
            var vFade = EdgeFadeWidth / ActualHeight;
            var vMask = new LinearGradientBrush { StartPoint = new Point(0, 0), EndPoint = new Point(0, 1) };
            vMask.GradientStops.Add(new GradientStop(Color.FromArgb(vHiddenBefore ? (byte)0 : (byte)255, 0, 0, 0), 0));
            vMask.GradientStops.Add(new GradientStop(Colors.Black, vFade));
            vMask.GradientStops.Add(new GradientStop(Colors.Black, 1 - vFade));
            vMask.GradientStops.Add(new GradientStop(Color.FromArgb(vHiddenAfter ? (byte)0 : (byte)255, 0, 0, 0), 1));
            vMask.Freeze();
            ApplyOpacityMask(vMask);
            return;
        }

        var hiddenBefore = HorizontalOffset > 0.5;
        var hiddenAfter = HorizontalOffset < ScrollableWidth - 0.5;
        if (ActualWidth <= EdgeFadeWidth * 2 || (!hiddenBefore && !hiddenAfter))
        {
            ApplyOpacityMask(null);
            return;
        }

        var fade = EdgeFadeWidth / ActualWidth;
        var mask = new LinearGradientBrush { StartPoint = new Point(0, 0), EndPoint = new Point(1, 0) };
        mask.GradientStops.Add(new GradientStop(Color.FromArgb(hiddenBefore ? (byte)0 : (byte)255, 0, 0, 0), 0));
        mask.GradientStops.Add(new GradientStop(Colors.Black, fade));
        mask.GradientStops.Add(new GradientStop(Colors.Black, 1 - fade));
        mask.GradientStops.Add(new GradientStop(Color.FromArgb(hiddenAfter ? (byte)0 : (byte)255, 0, 0, 0), 1));
        mask.Freeze();
        ApplyOpacityMask(mask);
    }

    private static void LayoutChanged(DependencyObject owner, DependencyPropertyChangedEventArgs e)
    {
        if (owner is not DockScrollViewer viewer) return;
        var inset = viewer.ContentInset;
        // Expand the clipping viewport upward, then keep the shelf content at its original position.
        viewer.SetCurrentValue(MarginProperty, new Thickness(inset.Left, inset.Top - viewer.HoverHeadroom, inset.Right, inset.Bottom));
        viewer.SetCurrentValue(PaddingProperty, new Thickness(0, viewer.HoverHeadroom, 0, 0));
        viewer.UpdateIndicatorLayout();
    }
}
