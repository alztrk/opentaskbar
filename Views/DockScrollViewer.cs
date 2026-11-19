using System.Windows;
using System.Windows.Controls;
using System.Windows.Input;
using System.Windows.Media;
using System.Windows.Media.Animation;

namespace OpenTaskbar.Views;

public sealed class DockScrollViewer : ScrollViewer
{
    private const double EdgeFadeWidth = 24;
    private Point? _panStart;
    private double _panOffset;
    private double _targetHorizontalOffset;
    private double _targetVerticalOffset;
    private ScrollContentPresenter? _scrollContentPresenter;
    private Button? _previousButton;
    private Button? _nextButton;

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
        if (ScrollableHeight > 0)
        {
            if (_previousButton is not null)
            {
                _previousButton.Content = TryFindResource("IconChevronUp");
                _previousButton.HorizontalAlignment = HorizontalAlignment.Center;
                _previousButton.VerticalAlignment = VerticalAlignment.Top;
                _previousButton.Width = 34;
                _previousButton.Height = 20;
                _previousButton.Margin = new Thickness(0, 2, 0, 0);
            }
            if (_nextButton is not null)
            {
                _nextButton.Content = TryFindResource("IconChevronDown");
                _nextButton.HorizontalAlignment = HorizontalAlignment.Center;
                _nextButton.VerticalAlignment = VerticalAlignment.Bottom;
                _nextButton.Width = 34;
                _nextButton.Height = 20;
                _nextButton.Margin = new Thickness(0, 0, 0, 2);
            }
        }
        else
        {
            var topOffset = HoverHeadroom;
            if (_previousButton is not null)
            {
                _previousButton.Content = TryFindResource("IconChevronLeft");
                _previousButton.HorizontalAlignment = HorizontalAlignment.Left;
                _previousButton.VerticalAlignment = VerticalAlignment.Center;
                _previousButton.Width = 20;
                _previousButton.Height = 34;
                _previousButton.Margin = new Thickness(2, topOffset, 0, 0);
            }
            if (_nextButton is not null)
            {
                _nextButton.Content = TryFindResource("IconChevronRight");
                _nextButton.HorizontalAlignment = HorizontalAlignment.Right;
                _nextButton.VerticalAlignment = VerticalAlignment.Center;
                _nextButton.Width = 20;
                _nextButton.Height = 34;
                _nextButton.Margin = new Thickness(0, topOffset, 2, 0);
            }
        }
    }

    private void UpdateIndicators()
    {
        if (_previousButton is null && _nextButton is null) return;

        if (ScrollableHeight > 0)
        {
            var vHiddenBefore = VerticalOffset > 0.5;
            var vHiddenAfter = VerticalOffset < ScrollableHeight - 0.5;
            if (_previousButton is not null)
                _previousButton.Visibility = vHiddenBefore ? Visibility.Visible : Visibility.Collapsed;
            if (_nextButton is not null)
                _nextButton.Visibility = vHiddenAfter ? Visibility.Visible : Visibility.Collapsed;
            return;
        }

        if (ScrollableWidth > 0)
        {
            var hiddenBefore = HorizontalOffset > 0.5;
            var hiddenAfter = HorizontalOffset < ScrollableWidth - 0.5;
            if (_previousButton is not null)
                _previousButton.Visibility = hiddenBefore ? Visibility.Visible : Visibility.Collapsed;
            if (_nextButton is not null)
                _nextButton.Visibility = hiddenAfter ? Visibility.Visible : Visibility.Collapsed;
            return;
        }

        if (_previousButton is not null) _previousButton.Visibility = Visibility.Collapsed;
        if (_nextButton is not null) _nextButton.Visibility = Visibility.Collapsed;
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
