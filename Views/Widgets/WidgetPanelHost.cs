using System.Runtime.InteropServices;
using System.Windows;
using System.Windows.Controls.Primitives;
using System.Windows.Input;
using OpenTaskbar.Services;

namespace OpenTaskbar.Views.Widgets;

public sealed class WidgetPanelHost : IDisposable
{
    private readonly Window _owner;
    private readonly WidgetPanelFrame _frame = new();
    private readonly Popup _popup;
    private FrameworkElement? _anchor;
    private FrameworkElement? _closedAnchor;
    private bool _isOpenState;
    private bool _suppressNextClick;
    private long _suppressNextClickUntilMs;
    private bool _explicitClose;
    private bool _restoreFocus;
    private bool _disposed;

    public WidgetPanelHost(Window owner)
    {
        _owner = owner;
        _popup = new Popup
        {
            AllowsTransparency = true,
            StaysOpen = false,
            Placement = PlacementMode.Top,
            VerticalOffset = -DockMetrics.FlyoutGap,
            PopupAnimation = PopupAnimation.None,
            Child = _frame
        };
        _popup.Opened += Popup_Opened;
        _popup.Closed += Popup_Closed;
        _frame.CloseRequested += Frame_CloseRequested;
        _frame.PreviewKeyDown += Frame_PreviewKeyDown;
        _owner.LocationChanged += Owner_LayoutChanged;
        _owner.SizeChanged += Owner_SizeChanged;
        _owner.Deactivated += Owner_Deactivated;
    }

    public bool IsOpen => _isOpenState || _popup.IsOpen;
    public event EventHandler? IsOpenChanged;
    public event Action<string>? OperationFailed;

    public void Open(FrameworkElement anchor, string heading, FrameworkElement body)
    {
        ObjectDisposedException.ThrowIf(_disposed, this);
        if (IsOpen && ReferenceEquals(_anchor, anchor))
        {
            _isOpenState = false;
            Close();
            return;
        }

        if (_suppressNextClick && ReferenceEquals(_closedAnchor, anchor) && Environment.TickCount64 <= _suppressNextClickUntilMs)
        {
            _isOpenState = false;
            _suppressNextClick = false;
            _closedAnchor = null;
            return;
        }

        _suppressNextClick = false;
        _closedAnchor = null;
        Close(restoreFocus: false);
        if (!_owner.IsActive)
            _owner.Activate();
        _isOpenState = true;
        _anchor = anchor;
        _restoreFocus = true;
        _frame.Heading = heading;
        _frame.Body = body;
        var gap = ComputeVerticalGap(anchor);
        _popup.VerticalOffset = -gap;
        _frame.MaxWidth = Math.Max(1, SystemParameters.WorkArea.Width - 16);
        _frame.MaxHeight = Math.Max(1, SystemParameters.WorkArea.Height - 16);
        _popup.HorizontalOffset = (anchor.ActualWidth - Math.Min(_frame.Width, _frame.MaxWidth)) / 2;
        _popup.PlacementTarget = anchor;
        _popup.IsOpen = true;
    }

    private static double ComputeVerticalGap(FrameworkElement anchor)
    {
        const double desiredGapDip = 8.0;
        DependencyObject? current = anchor;
        FrameworkElement? dockFrame = null;
        while (current is not null)
        {
            if (current is FrameworkElement fe && string.Equals(fe.Name, "DockFrame", StringComparison.Ordinal))
            {
                dockFrame = fe;
                break;
            }
            current = System.Windows.Media.VisualTreeHelper.GetParent(current) ?? LogicalTreeHelper.GetParent(current);
        }

        if (dockFrame is null || !anchor.IsLoaded || dockFrame.ActualHeight <= 0)
            return desiredGapDip;

        try
        {
            var topInset = Math.Max(0, anchor.TranslatePoint(new Point(0, 0), dockFrame).Y);
            return topInset + desiredGapDip;
        }
        catch (InvalidOperationException)
        {
            return desiredGapDip;
        }
    }

    public void Close(bool restoreFocus = true)
    {
        _isOpenState = false;
        _explicitClose = true;
        _restoreFocus = restoreFocus;
        _popup.IsOpen = false;
    }

    private void Popup_Opened(object? sender, EventArgs e)
    {
        IsOpenChanged?.Invoke(this, EventArgs.Empty);
        _frame.UpdateLayout();
        if (_popup.IsOpen && _frame.Body is { } body && !body.MoveFocus(new TraversalRequest(FocusNavigationDirection.First)))
            OperationFailed?.Invoke("WidgetPanelFocusError");
    }

    private void Popup_Closed(object? sender, EventArgs e)
    {
        _isOpenState = false;
        var anchor = _anchor;
        if (!_explicitClose && anchor is not null && Mouse.LeftButton == MouseButtonState.Pressed && IsCursorOver(anchor))
        {
            _suppressNextClick = true;
            _suppressNextClickUntilMs = Environment.TickCount64 + 300;
            _closedAnchor = anchor;
        }
        else
        {
            _suppressNextClick = false;
            _closedAnchor = null;
        }
        _explicitClose = false;
        _anchor = null;
        _frame.Body = null;
        _popup.PlacementTarget = null;
        if (_restoreFocus && _owner.IsActive && anchor is { IsLoaded: true, IsVisible: true })
            anchor.MoveFocus(new TraversalRequest(FocusNavigationDirection.First));
        IsOpenChanged?.Invoke(this, EventArgs.Empty);
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

    private static bool IsCursorOver(UIElement? element)
    {
        if (element is null || !element.IsVisible) return false;
        if (element.IsMouseOver) return true;
        if (element is FrameworkElement fe && TryGetScreenBounds(fe, padding: 6, out var bounds))
        {
            if (GetCursorPos(out var pt))
            {
                return pt.X >= bounds.Left && pt.X <= bounds.Right && pt.Y >= bounds.Top && pt.Y <= bounds.Bottom;
            }
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
            bounds = (Math.Min(tl.X, br.X) - padX, Math.Min(tl.Y, br.Y) - padY, Math.Max(tl.X, br.X) + padX, Math.Max(tl.Y, br.Y) + padY);
            return true;
        }
        catch (InvalidOperationException)
        {
            return false;
        }
    }

    private void Frame_CloseRequested(object? sender, EventArgs e) => Close();
    private void Frame_PreviewKeyDown(object sender, KeyEventArgs e)
    {
        if (e.Key != Key.Escape) return;
        e.Handled = true;
        Close();
    }
    private void Owner_LayoutChanged(object? sender, EventArgs e) => Close(restoreFocus: false);
    private void Owner_SizeChanged(object sender, SizeChangedEventArgs e) => Close(restoreFocus: false);
    private void Owner_Deactivated(object? sender, EventArgs e) => Close(restoreFocus: false);

    public void Dispose()
    {
        if (_disposed) return;
        Close(restoreFocus: false);
        _popup.Opened -= Popup_Opened;
        _popup.Closed -= Popup_Closed;
        _frame.CloseRequested -= Frame_CloseRequested;
        _frame.PreviewKeyDown -= Frame_PreviewKeyDown;
        _owner.LocationChanged -= Owner_LayoutChanged;
        _owner.SizeChanged -= Owner_SizeChanged;
        _owner.Deactivated -= Owner_Deactivated;
        _frame.Body = null;
        _popup.Child = null;
        _disposed = true;
    }
}
