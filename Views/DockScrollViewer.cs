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
    private double _externalIndicatorCornerRadius = 19;
    private bool? _lastExternalVertical;
    private double _lastExternalRadius = double.NaN;

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
        // When only one axis overflows (horizontal bottom dock or vertical side dock), a plain wheel scrolls it.
        var horizontalOnly = ScrollableHeight <= 0 && ScrollableWidth > 0;
        var verticalOnly = ScrollableWidth <= 0 && ScrollableHeight > 0;
        if (!horizontalOnly && !verticalOnly && (modifiers & (ModifierKeys.Shift | ModifierKeys.Control)) == 0) return false;
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
    /// Registers external overlay buttons (e.g. placed at the outer edges of DockSurface) that
    /// render the scroll scrim and chevron in place of the internal template buttons.
    /// </summary>
    public void SetExternalIndicators(Button? previous, Button? next)
    {
        _externalPreviousButton = previous;
        _externalNextButton = next;
        _lastExternalVertical = null;
        _lastExternalRadius = double.NaN;
        UpdateIndicatorLayout();
        UpdateIndicators();
    }

    /// <summary>Inner corner radius matching DockFrame so external edge scrims align cleanly with the dock corners.</summary>
    public double ExternalIndicatorCornerRadius
    {
        get => _externalIndicatorCornerRadius;
        set
        {
            var clamped = double.IsFinite(value) ? Math.Max(0, value) : 0;
            if (Math.Abs(_externalIndicatorCornerRadius - clamped) < 0.01) return;
            _externalIndicatorCornerRadius = clamped;
            UpdateIndicatorLayout();
        }
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
        var vertical = ScrollableHeight > 0;
        if (vertical)
        {
            if (_previousButton is not null)
            {
                _previousButton.Content = TryFindResource("IconChevronUp");
                _previousButton.SetResourceReference(BackgroundProperty, "DockScrollScrimTop");
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
                _nextButton.SetResourceReference(BackgroundProperty, "DockScrollScrimBottom");
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
                _previousButton.SetResourceReference(BackgroundProperty, "DockScrollScrimStart");
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
                _nextButton.SetResourceReference(BackgroundProperty, "DockScrollScrimEnd");
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

        UpdateExternalIndicatorLayout(vertical);
    }

    private void UpdateExternalIndicatorLayout(bool vertical)
    {
        if (_externalPreviousButton is null && _externalNextButton is null) return;
        var radius = _externalIndicatorCornerRadius;
        if (_lastExternalVertical == vertical && Math.Abs(_lastExternalRadius - radius) < 0.01) return;
        _lastExternalVertical = vertical;
        _lastExternalRadius = radius;

        // Inset by 1 DIP (ControlStroke) so edge scrims stay cleanly inside DockFrame's border.
        if (vertical)
        {
            if (_externalPreviousButton is not null)
            {
                _externalPreviousButton.Content = TryFindResource("IconChevronUp");
                _externalPreviousButton.SetResourceReference(BackgroundProperty, "DockScrollScrimTop");
                _externalPreviousButton.HorizontalAlignment = HorizontalAlignment.Stretch;
                _externalPreviousButton.VerticalAlignment = VerticalAlignment.Top;
                _externalPreviousButton.Width = double.NaN;
                _externalPreviousButton.Height = 28;
                _externalPreviousButton.HorizontalContentAlignment = HorizontalAlignment.Center;
                _externalPreviousButton.VerticalContentAlignment = VerticalAlignment.Top;
                _externalPreviousButton.Padding = new Thickness(0, 6, 0, 0);
                _externalPreviousButton.Margin = new Thickness(1, 1, 1, 0);
                _externalPreviousButton.Tag = new CornerRadius(radius, radius, 0, 0);
            }
            if (_externalNextButton is not null)
            {
                _externalNextButton.Content = TryFindResource("IconChevronDown");
                _externalNextButton.SetResourceReference(BackgroundProperty, "DockScrollScrimBottom");
                _externalNextButton.HorizontalAlignment = HorizontalAlignment.Stretch;
                _externalNextButton.VerticalAlignment = VerticalAlignment.Bottom;
                _externalNextButton.Width = double.NaN;
                _externalNextButton.Height = 28;
                _externalNextButton.HorizontalContentAlignment = HorizontalAlignment.Center;
                _externalNextButton.VerticalContentAlignment = VerticalAlignment.Bottom;
                _externalNextButton.Padding = new Thickness(0, 0, 0, 6);
                _externalNextButton.Margin = new Thickness(1, 0, 1, 1);
                _externalNextButton.Tag = new CornerRadius(0, 0, radius, radius);
            }
        }
        else
        {
            if (_externalPreviousButton is not null)
            {
                _externalPreviousButton.Content = TryFindResource("IconChevronLeft");
                _externalPreviousButton.SetResourceReference(BackgroundProperty, "DockScrollScrimStart");
                _externalPreviousButton.HorizontalAlignment = HorizontalAlignment.Left;
                _externalPreviousButton.VerticalAlignment = VerticalAlignment.Stretch;
                _externalPreviousButton.Width = 28;
                _externalPreviousButton.Height = double.NaN;
                _externalPreviousButton.HorizontalContentAlignment = HorizontalAlignment.Left;
                _externalPreviousButton.VerticalContentAlignment = VerticalAlignment.Center;
                // 1 DIP margin + 7 DIP padding places the chevron right beside the 8 DIP resize grip.
                _externalPreviousButton.Padding = new Thickness(7, 0, 0, 0);
                _externalPreviousButton.Margin = new Thickness(1, 1, 0, 1);
                _externalPreviousButton.Tag = new CornerRadius(radius, 0, 0, radius);
            }
            if (_externalNextButton is not null)
            {
                _externalNextButton.Content = TryFindResource("IconChevronRight");
                _externalNextButton.SetResourceReference(BackgroundProperty, "DockScrollScrimEnd");
                _externalNextButton.HorizontalAlignment = HorizontalAlignment.Right;
                _externalNextButton.VerticalAlignment = VerticalAlignment.Stretch;
                _externalNextButton.Width = 28;
                _externalNextButton.Height = double.NaN;
                _externalNextButton.HorizontalContentAlignment = HorizontalAlignment.Right;
                _externalNextButton.VerticalContentAlignment = VerticalAlignment.Center;
                _externalNextButton.Padding = new Thickness(0, 0, 7, 0);
                _externalNextButton.Margin = new Thickness(0, 1, 1, 1);
                _externalNextButton.Tag = new CornerRadius(0, radius, radius, 0);
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
