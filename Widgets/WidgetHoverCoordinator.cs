using System.Runtime.InteropServices;
using System.Windows;
using System.Windows.Input;
using System.Windows.Media;
using System.Windows.Threading;

namespace OpenTaskbar.Widgets;

/// <summary>
/// Shared hover-expansion coordinator for compact dock widgets. Uses a 350 ms hover intent delay before
/// opening, never steals window focus, and verifies physical cursor position at 200 ms intervals while open
/// (zero CPU when closed) so moving the pointer back to another window/video always closes the card cleanly.
/// </summary>
public sealed class WidgetHoverCoordinator : IDisposable
{
    private readonly DispatcherTimer _enterTimer;
    private readonly DispatcherTimer _leaveTimer;
    private readonly DockFlyout _flyout = new();
    private readonly Func<DockWidget, bool> _canExpandInline;
    private readonly Action<bool> _onLayoutChanged;
    private DockWidget? _pendingWidget;
    private DockWidget? _activeWidget;
    private UIElement? _activeFlyoutContent;
    private long _outsideSinceMs;
    private long _suppressedUntilMs;
    private bool _disposed;
    private readonly HashSet<DockWidget> _attached = [];

    public bool IsHoverActive => _activeWidget is not null || _flyout.IsOpen;
    public event Action<bool>? HoverActiveChanged;

    public WidgetHoverCoordinator(
        Dispatcher dispatcher,
        Func<DockWidget, bool> canExpandInline,
        Action<bool> onLayoutChanged)
    {
        _canExpandInline = canExpandInline;
        _onLayoutChanged = onLayoutChanged;
        _enterTimer = new DispatcherTimer(DispatcherPriority.Background, dispatcher)
        {
            Interval = TimeSpan.FromMilliseconds(350)
        };
        _enterTimer.Tick += EnterTimer_Tick;
        _leaveTimer = new DispatcherTimer(DispatcherPriority.Background, dispatcher)
        {
            Interval = TimeSpan.FromMilliseconds(180)
        };
        _leaveTimer.Tick += LeaveTimer_Tick;
        _flyout.Closed += Flyout_Closed;
    }

    public void Attach(DockWidget widget)
    {
        if (_disposed || !widget.SupportsCompactMode || !_attached.Add(widget)) return;
        widget.MouseEnter += Widget_MouseEnter;
        widget.MouseLeave += Widget_MouseLeave;
        widget.PreviewMouseDown += Widget_PreviewMouseDown;
    }

    public void Detach(DockWidget widget)
    {
        if (!_attached.Remove(widget)) return;
        widget.MouseEnter -= Widget_MouseEnter;
        widget.MouseLeave -= Widget_MouseLeave;
        widget.PreviewMouseDown -= Widget_PreviewMouseDown;
        if (_pendingWidget == widget)
        {
            _enterTimer.Stop();
            _pendingWidget = null;
        }
        if (_activeWidget == widget)
        {
            CollapseActive();
        }
    }

    /// <summary>
    /// Collapses hover UI and ignores hover intent for a while. Used when the interactive desktop returns so the
    /// first click after unlocking cannot land on an automatically reopened flyout's settings action.
    /// </summary>
    public void Suppress(TimeSpan duration)
    {
        CollapseActive();
        _suppressedUntilMs = Environment.TickCount64 + (long)duration.TotalMilliseconds;
    }

    public void CollapseActive()
    {
        _enterTimer.Stop();
        _leaveTimer.Stop();
        _pendingWidget = null;
        _outsideSinceMs = 0;
        if (_activeWidget is null && !_flyout.IsOpen) return;

        var previous = _activeWidget;
        _activeWidget = null;
        _activeFlyoutContent = null;
        var wasInline = previous?.IsHoverExpanded == true;
        if (previous is not null)
        {
            previous.IsHoverFlyoutOpen = false;
            previous.SetHoverExpanded(false);
        }
        if (_flyout.IsOpen) _flyout.Close();
        if (wasInline) _onLayoutChanged(false);
        HoverActiveChanged?.Invoke(false);
    }

    private void Flyout_Closed(object? sender, EventArgs e)
    {
        if (_activeWidget is null && _activeFlyoutContent is null) return;
        _leaveTimer.Stop();
        _outsideSinceMs = 0;
        var previous = _activeWidget;
        _activeWidget = null;
        _activeFlyoutContent = null;
        if (previous is not null)
        {
            previous.IsHoverFlyoutOpen = false;
            previous.SetHoverExpanded(false);
        }
        HoverActiveChanged?.Invoke(false);
    }

    private void Widget_MouseEnter(object sender, MouseEventArgs e)
    {
        if (_disposed || sender is not DockWidget widget || !widget.IsCompact) return;
        if (Environment.TickCount64 < _suppressedUntilMs) return;
        if (widget == _activeWidget)
        {
            _outsideSinceMs = 0;
            return;
        }

        _enterTimer.Stop();
        _pendingWidget = widget;
        _enterTimer.Start();
    }

    private void Widget_MouseLeave(object sender, MouseEventArgs e)
    {
        if (_disposed || sender is not DockWidget widget) return;
        if (_pendingWidget == widget)
        {
            _enterTimer.Stop();
            _pendingWidget = null;
        }

        if (_activeWidget == widget && !_leaveTimer.IsEnabled)
        {
            _leaveTimer.Start();
        }
    }

    private void Widget_PreviewMouseDown(object sender, MouseButtonEventArgs e)
    {
        _enterTimer.Stop();
        _pendingWidget = null;
    }

    private void EnterTimer_Tick(object? sender, EventArgs e)
    {
        _enterTimer.Stop();
        if (DockFlyout.IsAnyOpen || Environment.TickCount64 < _suppressedUntilMs)
        {
            _pendingWidget = null;
            return;
        }

        var widget = _pendingWidget;
        _pendingWidget = null;
        if (_disposed || widget is null || !widget.IsCompact || !IsWidgetCurrentlyHovered(widget)) return;

        if (_activeWidget is not null && _activeWidget != widget)
        {
            CollapseActive();
        }

        _activeWidget = widget;
        _outsideSinceMs = 0;
        if (_canExpandInline(widget))
        {
            widget.SetHoverExpanded(true);
            _onLayoutChanged(true);
            if (!_leaveTimer.IsEnabled) _leaveTimer.Start();
            HoverActiveChanged?.Invoke(true);
            return;
        }

        var content = widget.BuildHoverFlyoutContent();
        if (content is null)
        {
            _activeWidget = null;
            return;
        }

        _activeFlyoutContent = content;
        widget.IsHoverFlyoutOpen = true;
        _flyout.Show(widget, content, animate: true, focusable: false);
        if (!_leaveTimer.IsEnabled) _leaveTimer.Start();
        HoverActiveChanged?.Invoke(true);
    }

    private void LeaveTimer_Tick(object? sender, EventArgs e)
    {
        if (_disposed || _activeWidget is null)
        {
            _leaveTimer.Stop();
            return;
        }

        // A widget may be removed or its visual tree detached while the hover card
        // remains open. Never leave a non-focusable StaysOpen popup orphaned.
        if (!_activeWidget.IsLoaded || !_activeWidget.IsVisible)
        {
            CollapseActive();
            return;
        }

        // Keep open while dragging an internal control slider, while a click flyout is open, or while cursor rests over the widget or card.
        if (IsSliderDragCaptured(_activeWidget, _activeFlyoutContent)
            || (_activeFlyoutContent is null && DockFlyout.IsAnyOpen)
            || IsPointerInsideActiveSurface(_activeWidget, _activeFlyoutContent))
        {
            _outsideSinceMs = 0;
            return;
        }

        var now = Environment.TickCount64;
        if (_outsideSinceMs == 0)
        {
            _outsideSinceMs = now;
            return;
        }

        if (now - _outsideSinceMs >= 160)
        {
            CollapseActive();
        }
    }

    private static bool IsSliderDragCaptured(DockWidget widget, UIElement? flyoutContent)
    {
        if (Mouse.Captured is not DependencyObject captured) return false;
        return IsDescendantOf(widget, captured)
            || (flyoutContent is not null && IsDescendantOf(flyoutContent, captured));
    }

    private static bool IsWidgetCurrentlyHovered(DockWidget widget)
    {
        if (GetCursorPos(out var pt) && TryGetScreenBounds(widget, padding: 2, out var bounds))
        {
            return pt.X >= bounds.Left && pt.X <= bounds.Right && pt.Y >= bounds.Top && pt.Y <= bounds.Bottom;
        }
        return widget.IsMouseOver;
    }

    private static bool IsPointerInsideActiveSurface(DockWidget widget, UIElement? flyoutContent)
    {
        if (!GetCursorPos(out var pt))
        {
            return widget.IsMouseOver || flyoutContent is { IsMouseOver: true };
        }

        if (TryGetScreenBounds(widget, padding: 8, out var widgetBounds)
            && pt.X >= widgetBounds.Left && pt.X <= widgetBounds.Right
            && pt.Y >= widgetBounds.Top && pt.Y <= widgetBounds.Bottom)
        {
            return true;
        }

        if (flyoutContent is FrameworkElement fe
            && PresentationSource.FromVisual(fe) is not null
            && TryGetScreenBounds(fe, padding: 18, out var flyoutBounds)
            && pt.X >= flyoutBounds.Left && pt.X <= flyoutBounds.Right
            && pt.Y >= flyoutBounds.Top && pt.Y <= flyoutBounds.Bottom)
        {
            return true;
        }

        return false;
    }

    private static bool TryGetScreenBounds(FrameworkElement element, double padding, out (double Left, double Top, double Right, double Bottom) bounds)
    {
        bounds = default;
        var source = PresentationSource.FromVisual(element);
        if (source?.CompositionTarget is null || element.ActualWidth <= 0 || element.ActualHeight <= 0)
        {
            return false;
        }

        try
        {
            var dpiX = source.CompositionTarget.TransformToDevice.M11;
            var dpiY = source.CompositionTarget.TransformToDevice.M22;
            var padX = padding * dpiX;
            var padY = padding * dpiY;
            var tl = element.PointToScreen(new Point(0, 0));
            var br = element.PointToScreen(new Point(element.ActualWidth, element.ActualHeight));
            bounds = (tl.X - padX, tl.Y - padY, br.X + padX, br.Y + padY);
            return true;
        }
        catch (InvalidOperationException)
        {
            return false;
        }
    }

    private static bool IsDescendantOf(DependencyObject root, DependencyObject node)
    {
        var current = node;
        while (current is not null)
        {
            if (ReferenceEquals(current, root)) return true;
            current = (current is Visual or System.Windows.Media.Media3D.Visual3D ? VisualTreeHelper.GetParent(current) : null)
                ?? LogicalTreeHelper.GetParent(current);
        }
        return false;
    }

    public void Dispose()
    {
        if (_disposed) return;
        _disposed = true;
        foreach (var widget in _attached)
        {
            widget.MouseEnter -= Widget_MouseEnter;
            widget.MouseLeave -= Widget_MouseLeave;
            widget.PreviewMouseDown -= Widget_PreviewMouseDown;
        }
        _attached.Clear();
        _enterTimer.Tick -= EnterTimer_Tick;
        _leaveTimer.Tick -= LeaveTimer_Tick;
        _flyout.Closed -= Flyout_Closed;
        CollapseActive();
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct NativePoint
    {
        public int X;
        public int Y;
    }

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool GetCursorPos(out NativePoint pt);
}
