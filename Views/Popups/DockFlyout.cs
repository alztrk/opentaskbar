using System.Runtime.InteropServices;
using System.Text;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Controls.Primitives;
using System.Windows.Documents;
using System.Windows.Input;
using System.Windows.Media;
using System.Windows.Media.Animation;
using OpenTaskbar.Models;
using OpenTaskbar.Services;
using OpenTaskbar.Views.Windows;

namespace OpenTaskbar;

/// <summary>
/// A glass popup anchored to a dock element (stacks, calendar, tray, desktops). Opens away from the docked screen edge
/// and closes when the pointer clicks elsewhere.
/// </summary>
public sealed class DockFlyout
{
    private static readonly SolidColorBrush ShadowBrush1 = FreezeBrush(Color.FromArgb(0x26, 0x05, 0x07, 0x0B));
    private static readonly SolidColorBrush ShadowBrush2 = FreezeBrush(Color.FromArgb(0x18, 0x05, 0x07, 0x0B));
    private static readonly SolidColorBrush ShadowBrush3 = FreezeBrush(Color.FromArgb(0x0E, 0x05, 0x07, 0x0B));
    private static readonly SolidColorBrush ShadowBrush4 = FreezeBrush(Color.FromArgb(0x06, 0x05, 0x07, 0x0B));

    private static SolidColorBrush FreezeBrush(Color color)
    {
        var brush = new SolidColorBrush(color);
        brush.Freeze();
        return brush;
    }

    internal static void AttachSoftShadow(Panel host, double cornerRadius)
    {
        // Four concentric outer stroke rings with transparent interiors preserve glass translucency without ShaderEffect/D3DCompiler.
        AddRing(host, 2, 1, cornerRadius + 2, ShadowBrush1);
        AddRing(host, 5, 2, cornerRadius + 5, ShadowBrush2);
        AddRing(host, 9, 3, cornerRadius + 9, ShadowBrush3);
        AddRing(host, 14, 4, cornerRadius + 14, ShadowBrush4);
    }

    private static void AddRing(Panel host, double spread, double yOffset, double radius, Brush brush)
    {
        var prev = spread switch
        {
            2 => 0d,
            5 => 2d,
            9 => 5d,
            _ => 9d
        };
        var thickness = spread - prev;
        host.Children.Add(new Border
        {
            Margin = new Thickness(-spread, -spread + yOffset, -spread, -spread - yOffset),
            CornerRadius = new CornerRadius(radius),
            BorderThickness = new Thickness(thickness),
            BorderBrush = brush,
            Background = Brushes.Transparent,
            IsHitTestVisible = false
        });
    }

    private static readonly List<WeakReference<DockFlyout>> FlyoutRegistry = [];
    public static event Action? AnyClosed;
    private UIElement? _currentTarget;
    private bool _isOpenState;
    private bool _suppressNextClick;
    private bool _explicitClose;
    private WeakReference<UIElement>? _closedTarget;
    private long _closedAt;
    private nint _previousForeground;
    private bool _restoreFocus;

    private readonly Popup _popup;
    private readonly Border _root;
    private readonly Border _frame;
    private readonly Border _innerBevel;

    private static void CleanRegistry() => FlyoutRegistry.RemoveAll(r => !r.TryGetTarget(out _));

    public static bool IsAnyOpen
    {
        get
        {
            CleanRegistry();
            for (var i = 0; i < FlyoutRegistry.Count; i++)
            {
                if (FlyoutRegistry[i].TryGetTarget(out var flyout) && flyout._popup.Dispatcher.CheckAccess() && flyout._popup.IsOpen)
                    return true;
            }
            return false;
        }
    }

    public static bool IsMouseOverAny
    {
        get
        {
            CleanRegistry();
            for (var i = 0; i < FlyoutRegistry.Count; i++)
            {
                if (FlyoutRegistry[i].TryGetTarget(out var flyout) && flyout._popup.Dispatcher.CheckAccess() && flyout._popup.IsOpen)
                {
                    if (flyout._root.IsMouseOver || flyout._frame.IsMouseOver)
                        return true;
                }
            }
            return false;
        }
    }

    public static void CloseAll(bool restoreFocus = true, bool force = false)
    {
        CleanRegistry();
        for (var i = 0; i < FlyoutRegistry.Count; i++)
        {
            // FlyoutRegistry is static and holds weak references to flyouts created on other
            // dispatchers (tests create several STA threads). Touching IsOpen across threads
            // throws; skip those entries just like IsAnyOpen does.
            if (FlyoutRegistry[i].TryGetTarget(out var flyout) && flyout._popup.Dispatcher.CheckAccess()
                && flyout._popup.IsOpen && (force || !flyout._popup.StaysOpen))
                flyout.Close(restoreFocus);
        }
    }

    public DockFlyout()
    {
        FlyoutRegistry.Add(new WeakReference<DockFlyout>(this));
        _innerBevel = new Border
        {
            BorderThickness = new Thickness(0),
            CornerRadius = new CornerRadius(11),
            Padding = new Thickness(12),
            Background = Brushes.Transparent
        };

        _frame = new Border
        {
            CornerRadius = new CornerRadius(12),
            Padding = new Thickness(0),
            ClipToBounds = true,
            Child = _innerBevel
        };
        _frame.SetResourceReference(Border.BorderThicknessProperty, "MenuStrokeThickness");
        _frame.SetResourceReference(Border.BackgroundProperty, "MenuMaterial");
        _frame.SetResourceReference(Border.BorderBrushProperty, "MenuStrokeMaterial");
        _frame.SetResourceReference(TextElement.ForegroundProperty, "TextPrimary");
        _frame.SetResourceReference(TextElement.FontFamilyProperty, "InterfaceFont");

        var contentHost = new Grid();
        AttachSoftShadow(contentHost, 12);
        contentHost.Children.Add(_frame);

        var scale = new ScaleTransform(1, 1);
        var translate = new TranslateTransform();
        var transformGroup = new TransformGroup { Children = { scale, translate } };
        // Use Border.Padding instead of Margin on the Popup root: WPF excludes Popup.Child.Margin from
        // CustomPopupPlacementCallback's popupSize, which previously shifted every flyout downward into the dock.
        _root = new Border
        {
            Padding = new Thickness(10, 8, 10, DockMetrics.FlyoutContentInset),
            Background = Brushes.Transparent,
            RenderTransform = transformGroup,
            RenderTransformOrigin = new Point(0.5, 0.5),
            Child = contentHost
        };
        _popup = new Popup
        {
            AllowsTransparency = true,
            StaysOpen = false,
            Child = _root,
            // The XAML entrance below already fades the content. A native Fade popup adds a ~200 ms layered
            // window animation during which the first mouse message can be delayed (#128 finding 9).
            PopupAnimation = PopupAnimation.None,
            Placement = PlacementMode.Custom,
            CustomPopupPlacementCallback = Place
        };
        _popup.Opened += (_, _) =>
        {
            if (!SystemParameters.ClientAreaAnimation)
            {
                translate.X = 0;
                translate.Y = 0;
                scale.ScaleX = 1;
                scale.ScaleY = 1;
                _root.Opacity = 1;
                return;
            }

            // Subtle V19 flyout entrance: 190 ms ease-out, 0 -> 1 opacity, 7 DIP slide, 0.985 -> 1 scale.
            var duration = TimeSpan.FromMilliseconds(190);
            var ease = new CubicEase { EasingMode = EasingMode.EaseOut };
            var isBottom = Edge == DockEdge.Bottom;
            var isLeft = Edge == DockEdge.Left;
            var isRight = Edge == DockEdge.Right;

            var prop = (isLeft || isRight) ? TranslateTransform.XProperty : TranslateTransform.YProperty;
            var fromVal = isBottom ? 7.0 : (isLeft ? -7.0 : 7.0);

            translate.BeginAnimation(prop, new DoubleAnimation(fromVal, 0.0, duration) { EasingFunction = ease });
            scale.BeginAnimation(ScaleTransform.ScaleXProperty, new DoubleAnimation(0.985, 1.0, duration) { EasingFunction = ease });
            scale.BeginAnimation(ScaleTransform.ScaleYProperty, new DoubleAnimation(0.985, 1.0, duration) { EasingFunction = ease });
            _root.BeginAnimation(UIElement.OpacityProperty, new DoubleAnimation(0.0, 1.0, duration) { EasingFunction = ease });
        };
        _root.AddHandler(FrameworkElement.ContextMenuOpeningEvent, new ContextMenuEventHandler((_, e) =>
        {
            if (e.Handled) return;
            ContextMenu? cm = null;
            if (e.Source is FrameworkElement fe && fe.ContextMenu is not null)
                cm = fe.ContextMenu;
            else if (e.OriginalSource is DependencyObject dep)
            {
                var cur = dep;
                while (cur is not null && cm is null)
                {
                    if (cur is FrameworkElement el && el.ContextMenu is not null)
                        cm = el.ContextMenu;
                    cur = (cur is Visual or System.Windows.Media.Media3D.Visual3D ? VisualTreeHelper.GetParent(cur) : null)
                        ?? LogicalTreeHelper.GetParent(cur);
                }
            }
            if (cm is not null)
            {
                _popup.StaysOpen = true;
                RoutedEventHandler? onClosed = null;
                onClosed = (_, _) =>
                {
                    cm.Closed -= onClosed;
                    _popup.Dispatcher.BeginInvoke(System.Windows.Threading.DispatcherPriority.Input, () =>
                    {
                        if (!_popup.IsOpen) return;
                        _popup.StaysOpen = false;
                        if (!IsCursorOver(_frame) && !IsCursorOver(_currentTarget))
                        {
                            Close();
                        }
                    });
                };
                cm.Closed += onClosed;
            }
        }), handledEventsToo: true);
        _popup.Closed += (_, _) =>
        {
            UnhookMouse();
            _popup.StaysOpen = false;
            _isOpenState = false;
            var target = _currentTarget ?? _popup.PlacementTarget;
            var isClosingTargetClick = !_explicitClose && target is not null && (_suppressNextClick || IsCursorOver(target));
            if (target is not null)
            {
                _closedTarget = new WeakReference<UIElement>(target);
            }
            if (_currentTarget is not null)
            {
                _currentTarget.PreviewMouseDown -= Target_PreviewMouseDown;
                _currentTarget = null;
            }
            _closedAt = Environment.TickCount64;
            _suppressNextClick = isClosingTargetClick;
            _explicitClose = false;
            // Light dismiss and Esc hand the keyboard back to the app that had it before the flyout opened.
            if (_restoreFocus && _previousForeground != 0)
            {
                NativeWindowTracker.Activate(_previousForeground);
            }
            _restoreFocus = false;
            _previousForeground = 0;

            _innerBevel.Child = null;
            _popup.PlacementTarget = null;
            Keyboard.ClearFocus();
            Closed?.Invoke(this, EventArgs.Empty);
            AnyClosed?.Invoke();
        };
        _popup.KeyDown += (_, args) =>
        {
            if (args.Key != System.Windows.Input.Key.Escape) return;
            args.Handled = true;
            _popup.IsOpen = false;
        };
    }

    /// <summary>The dock edge the flyouts open away from.</summary>
    public static DockEdge Edge { get; set; } = DockEdge.Bottom;

    public event EventHandler? Closed;

    public bool IsOpen => _isOpenState || _popup.IsOpen;

    public bool StaysOpen
    {
        get => _popup.StaysOpen;
        set => _popup.StaysOpen = value;
    }

    public System.Windows.Threading.Dispatcher Dispatcher => _popup.Dispatcher;

    private void Target_PreviewMouseDown(object sender, MouseButtonEventArgs e)
    {
        AppLog.Info($"[DockFlyout.PreviewMouseDown] sender={sender.GetType().Name}, IsOpen={_popup.IsOpen}");
        if (_popup.IsOpen)
        {
            _suppressNextClick = true;
            _closedTarget = new WeakReference<UIElement>((UIElement)sender);
        }
    }

    private nint _mouseHook;
    private HookProc? _mouseHookDelegate;
    private uint _contextMenuProcessId;

    public void RetainForExternalContextMenu(nint appHwnd = 0)
    {
        if (!_popup.IsOpen) return;
        _popup.StaysOpen = true;
        if (appHwnd != 0)
        {
            GetWindowThreadProcessId(appHwnd, out _contextMenuProcessId);
        }
        if (_mouseHook == 0)
        {
            _mouseHookDelegate = MouseHookCallback;
            try
            {
                _mouseHook = SetWindowsHookEx(WH_MOUSE_LL, _mouseHookDelegate, GetModuleHandle(null), 0);
            }
            catch (Exception ex)
            {
                AppLog.Warning($"Failed to set mouse hook: {ex.Message}");
            }
            if (_mouseHook == 0)
            {
                _popup.StaysOpen = false;
                _mouseHookDelegate = null;
            }
        }
    }

    private void UnhookMouse()
    {
        if (_mouseHook != 0)
        {
            try
            {
                UnhookWindowsHookEx(_mouseHook);
            }
            catch (Exception ex)
            {
                AppLog.Warning($"Failed to unhook mouse: {ex.Message}");
            }
            _mouseHook = 0;
            _mouseHookDelegate = null;
        }
        _contextMenuProcessId = 0;
    }

    private nint MouseHookCallback(int nCode, nint wParam, nint lParam)
    {
        if (nCode >= 0 && (wParam == WM_LBUTTONDOWN || wParam == WM_RBUTTONDOWN || wParam == WM_MBUTTONDOWN ||
                           wParam == WM_NCLBUTTONDOWN || wParam == WM_NCRBUTTONDOWN || wParam == WM_NCMBUTTONDOWN))
        {
            try
            {
                var hookStruct = Marshal.PtrToStructure<MSLLHOOKSTRUCT>(lParam);
                var pt = hookStruct.pt;

                // Check if click is inside the flyout frame or target button
                if (IsPointInside(_frame, pt) || IsPointInside(_currentTarget, pt))
                {
                    return CallNextHookEx(_mouseHook, nCode, wParam, lParam);
                }

                // Check if click is on the external context menu
                var clickedHwnd = WindowFromPoint(pt);
                if (clickedHwnd != 0)
                {
                    var rootHwnd = GetAncestor(clickedHwnd, GA_ROOT);
                    var effectiveHwnd = rootHwnd != 0 ? rootHwnd : clickedHwnd;

                    var className = GetClassNameString(effectiveHwnd);
                    if (string.Equals(className, "#32768", StringComparison.Ordinal))
                    {
                        return CallNextHookEx(_mouseHook, nCode, wParam, lParam);
                    }

                    if (_contextMenuProcessId != 0)
                    {
                        GetWindowThreadProcessId(effectiveHwnd, out var clickPid);
                        if (clickPid == _contextMenuProcessId && IsPopupMenuWindow(effectiveHwnd, className))
                        {
                            return CallNextHookEx(_mouseHook, nCode, wParam, lParam);
                        }
                    }
                }

                // Click was outside the flyout AND outside the context menu!
                _popup.Dispatcher.BeginInvoke(System.Windows.Threading.DispatcherPriority.Normal, () =>
                {
                    Close();
                });
            }
            catch (Exception ex)
            {
                AppLog.Warning($"Mouse hook callback error: {ex.Message}");
            }
        }
        return CallNextHookEx(_mouseHook, nCode, wParam, lParam);
    }

    private static bool IsPointInside(UIElement? element, NativePoint pt)
    {
        if (element is null || !element.IsVisible) return false;
        if (element is FrameworkElement fe && TryGetScreenBounds(fe, padding: 6, out var bounds))
        {
            return pt.X >= bounds.Left && pt.X <= bounds.Right && pt.Y >= bounds.Top && pt.Y <= bounds.Bottom;
        }
        return false;
    }

    private static string GetClassNameString(nint hwnd)
    {
        var sb = new StringBuilder(256);
        return GetClassName(hwnd, sb, sb.Capacity) > 0 ? sb.ToString() : string.Empty;
    }

    private const int WH_MOUSE_LL = 14;
    private const nint WM_LBUTTONDOWN = 0x0201;
    private const nint WM_RBUTTONDOWN = 0x0204;
    private const nint WM_MBUTTONDOWN = 0x0207;
    private const nint WM_NCLBUTTONDOWN = 0x00A1;
    private const nint WM_NCRBUTTONDOWN = 0x00A4;
    private const nint WM_NCMBUTTONDOWN = 0x00A7;
    private const uint GA_ROOT = 2;

    [StructLayout(LayoutKind.Sequential)]
    private struct MSLLHOOKSTRUCT
    {
        public NativePoint pt;
        public uint mouseData;
        public uint flags;
        public uint time;
        public nint dwExtraInfo;
    }

    private delegate nint HookProc(int nCode, nint wParam, nint lParam);

    [DllImport("user32.dll", CharSet = CharSet.Auto, SetLastError = true)]
    private static extern nint SetWindowsHookEx(int idHook, HookProc lpfn, nint hMod, uint dwThreadId);

    [DllImport("user32.dll", CharSet = CharSet.Auto, SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool UnhookWindowsHookEx(nint hhk);

    [DllImport("user32.dll", CharSet = CharSet.Auto, SetLastError = true)]
    private static extern nint CallNextHookEx(nint hhk, int nCode, nint wParam, nint lParam);

    [DllImport("kernel32.dll", CharSet = CharSet.Auto, SetLastError = true)]
    private static extern nint GetModuleHandle(string? lpModuleName);

    [DllImport("user32.dll")]
    private static extern nint WindowFromPoint(NativePoint point);

    [DllImport("user32.dll")]
    private static extern uint GetWindowThreadProcessId(nint hWnd, out uint lpdwProcessId);

    [DllImport("user32.dll")]
    private static extern nint GetAncestor(nint hwnd, uint gaFlags);

    [DllImport("user32.dll", CharSet = CharSet.Auto, SetLastError = true)]
    private static extern int GetClassName(nint hWnd, StringBuilder lpClassName, int nMaxCount);

    [DllImport("user32.dll", EntryPoint = "GetWindowLongW")]
    private static extern int GetWindowLong32(nint hWnd, int nIndex);

    private static bool IsPopupMenuWindow(nint hwnd, string className)
    {
        if (string.Equals(className, "#32768", StringComparison.Ordinal)
            || string.Equals(className, "Popup", StringComparison.OrdinalIgnoreCase)
            || string.Equals(className, "Xaml_WindowedPopupClass", StringComparison.Ordinal)
            || className.Contains("DropDown", StringComparison.OrdinalIgnoreCase))
        {
            return true;
        }
        const int gwlStyle = -16;
        const uint wsPopup = 0x80000000;
        var style = unchecked((uint)GetWindowLong32(hwnd, gwlStyle));
        return (style & wsPopup) != 0;
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
        catch (Exception ex)
        {
            AppLog.Warning($"[TryGetScreenBounds] PointToScreen error: {ex.Message}");
            return false;
        }
    }

    public void Show(UIElement target, UIElement content, bool animate = true, bool focusable = false)
    {
        if (Window.GetWindow(target) is DynamicIslandWindow)
        {
            if (_popup.IsOpen) Close(restoreFocus: false);
            return;
        }

        UnhookMouse();
        _isOpenState = true;
        _explicitClose = false;
        _suppressNextClick = false;
        _closedTarget = null;
        var inset = DockMetrics.FlyoutContentInset;
        _root.Padding = Edge switch
        {
            DockEdge.Left => new Thickness(10, 8, 12, inset),
            DockEdge.Right => new Thickness(12, 8, 10, inset),
            _ => new Thickness(10, 8, 10, inset)
        };
        _innerBevel.Child = content;
        _popup.StaysOpen = false;
        _popup.PopupAnimation = PopupAnimation.None;
        CleanRegistry();
        for (var i = 0; i < FlyoutRegistry.Count; i++)
        {
            if (!FlyoutRegistry[i].TryGetTarget(out var flyout) || ReferenceEquals(flyout, this)) continue;
            var dispatcher = flyout._popup.Dispatcher;
            if (dispatcher.CheckAccess())
            {
                if (flyout._popup.IsOpen) flyout.Close();
            }
            else if (!dispatcher.HasShutdownStarted && !dispatcher.HasShutdownFinished)
                dispatcher.BeginInvoke(new Action(() => flyout.Close()));
        }
        if (_currentTarget is not null && !ReferenceEquals(_currentTarget, target))
        {
            _currentTarget.PreviewMouseDown -= Target_PreviewMouseDown;
        }
        _currentTarget = target;
        target.PreviewMouseDown -= Target_PreviewMouseDown;
        target.PreviewMouseDown += Target_PreviewMouseDown;
        _popup.PlacementTarget = target;
        if (focusable)
        {
            var owner = Window.GetWindow(target);
            _previousForeground = NativeWindowTracker.GetForegroundApplicationWindow();
            _restoreFocus = true;
            owner?.Activate();
            _popup.IsOpen = true;
            _frame.Focusable = true;
            _frame.Focus();
        }
        else
        {
            _previousForeground = 0;
            _restoreFocus = false;
            _popup.IsOpen = true;
            _frame.Focusable = false;
        }
    }

    public void Toggle(UIElement target, Func<UIElement> content, bool animate = true, bool focusable = false)
    {
        var isAnchorMatch = ReferenceEquals(_currentTarget, target) || ReferenceEquals(_popup.PlacementTarget, target);
        if (isAnchorMatch && (_isOpenState || _popup.IsOpen))
        {
            _isOpenState = false;
            _suppressNextClick = false;
            _closedTarget = null;
            Close();
            return;
        }

        if (_suppressNextClick && (_closedTarget is null || (_closedTarget.TryGetTarget(out var prev) && ReferenceEquals(prev, target))))
        {
            _suppressNextClick = false;
            _closedTarget = null;
            return;
        }

        Show(target, content(), animate, focusable);
    }

    public bool RecentlyClosed(UIElement target)
    {
        if (_suppressNextClick && (_closedTarget is null || (_closedTarget.TryGetTarget(out var clickTarget) && ReferenceEquals(clickTarget, target))))
        {
            _suppressNextClick = false;
            _closedTarget = null;
            return true;
        }

        if (_closedTarget is not null && _closedTarget.TryGetTarget(out var prev) && ReferenceEquals(prev, target)
            && Environment.TickCount64 - _closedAt < 350)
        {
            return true;
        }

        return false;
    }

    /// <summary>Closes flyout; clears focus restoration when explicitly requested.</summary>
    public void Close(bool restoreFocus = true)
    {
        UnhookMouse();
        _popup.StaysOpen = false;
        _explicitClose = true;
        _isOpenState = false;
        if (!restoreFocus) _restoreFocus = false;
        _popup.IsOpen = false;
    }

    private CustomPopupPlacement[] Place(Size popupSize, Size targetSize, Point offset)
    {
        // Measure the anchor's actual distance to the outer DockFrame edge so every flyout sits
        // exactly DesiredFlyoutGap (7 DIP) outside the visible taskbar regardless of button height or dock size.
        var gap = ComputePlacementGap(targetSize);
        return Edge switch
        {
            DockEdge.Left =>
            [
                new CustomPopupPlacement(new Point(targetSize.Width + gap, (targetSize.Height - popupSize.Height) / 2), PopupPrimaryAxis.Vertical),
                new CustomPopupPlacement(new Point(-popupSize.Width - gap, (targetSize.Height - popupSize.Height) / 2), PopupPrimaryAxis.Vertical)
            ],
            DockEdge.Right =>
            [
                new CustomPopupPlacement(new Point(-popupSize.Width - gap, (targetSize.Height - popupSize.Height) / 2), PopupPrimaryAxis.Vertical),
                new CustomPopupPlacement(new Point(targetSize.Width + gap, (targetSize.Height - popupSize.Height) / 2), PopupPrimaryAxis.Vertical)
            ],
            _ =>
            [
                new CustomPopupPlacement(new Point((targetSize.Width - popupSize.Width) / 2, -popupSize.Height - gap), PopupPrimaryAxis.Horizontal),
                new CustomPopupPlacement(new Point((targetSize.Width - popupSize.Width) / 2, targetSize.Height + gap), PopupPrimaryAxis.Horizontal)
            ]
        };
    }

    private double ComputePlacementGap(Size targetSize)
    {
        const double desiredGapDip = 8.0;
        var target = (_currentTarget ?? _popup.PlacementTarget) as FrameworkElement;
        if (target is null || !target.IsLoaded) return DockMetrics.FlyoutGap;

        var source = PresentationSource.FromVisual(target);
        var dpiX = source?.CompositionTarget?.TransformToDevice.M11 ?? 1.0;
        var dpiY = source?.CompositionTarget?.TransformToDevice.M22 ?? 1.0;

        var dockFrame = FindAncestorByName(target, "DockFrame");
        if (dockFrame is null || dockFrame.ActualWidth <= 0 || dockFrame.ActualHeight <= 0)
        {
            var scale = Edge == DockEdge.Bottom ? dpiY : dpiX;
            return Math.Max(0, desiredGapDip * scale);
        }

        try
        {
            // CustomPopupPlacementCallback operates in physical device pixels, so measure both the target and
            // DockFrame in screen pixels and scale the desired DIP gap + popup margin by the monitor DPI.
            var targetTopLeftPx = target.PointToScreen(new Point(0, 0));
            var targetBottomRightPx = target.PointToScreen(new Point(target.ActualWidth, target.ActualHeight));
            var dockTopLeftPx = dockFrame.PointToScreen(new Point(0, 0));
            var dockBottomRightPx = dockFrame.PointToScreen(new Point(dockFrame.ActualWidth, dockFrame.ActualHeight));

            var (anchorInsetPx, desiredGapPx, contentMarginPx) = Edge switch
            {
                DockEdge.Left => (
                    Math.Max(0, dockBottomRightPx.X - targetBottomRightPx.X),
                    desiredGapDip * dpiX,
                    _root.Padding.Left * dpiX),
                DockEdge.Right => (
                    Math.Max(0, targetTopLeftPx.X - dockTopLeftPx.X),
                    desiredGapDip * dpiX,
                    _root.Padding.Right * dpiX),
                _ => (
                    Math.Max(0, targetTopLeftPx.Y - dockTopLeftPx.Y),
                    desiredGapDip * dpiY,
                    _root.Padding.Bottom * dpiY)
            };
            return anchorInsetPx + desiredGapPx - contentMarginPx;
        }
        catch (InvalidOperationException)
        {
            var scale = Edge == DockEdge.Bottom ? dpiY : dpiX;
            return DockMetrics.FlyoutGap * scale;
        }
    }

    private static FrameworkElement? FindAncestorByName(DependencyObject start, string name)
    {
        var current = start;
        while (current is not null)
        {
            if (current is FrameworkElement fe && string.Equals(fe.Name, name, StringComparison.Ordinal))
                return fe;
            current = VisualTreeHelper.GetParent(current) ?? LogicalTreeHelper.GetParent(current);
        }
        return null;
    }

    /// <summary>Flat, borderless button used for flyout rows and tiles.</summary>
    public static Button FlatButton(object content, RoutedEventHandler click, string? toolTip = null)
    {
        var button = new Button { Content = content, ToolTip = toolTip, Cursor = System.Windows.Input.Cursors.Hand };
        button.SetResourceReference(FrameworkElement.StyleProperty, "FlyoutButton");
        button.Click += click;
        if (toolTip is not null) System.Windows.Automation.AutomationProperties.SetName(button, toolTip);
        return button;
    }

    public static TextBlock Heading(string text)
    {
        var block = new TextBlock { Text = text, FontWeight = FontWeights.SemiBold, FontSize = 13, Margin = new Thickness(0, 0, 0, 8) };
        block.SetResourceReference(TextBlock.ForegroundProperty, "TextPrimary");
        return block;
    }

    public static DockPanel Header(string title, UIElement? trailing = null, double bottomMargin = 10)
    {
        var row = new DockPanel { LastChildFill = true, Margin = new Thickness(0, 0, 0, bottomMargin) };
        if (trailing is not null)
        {
            DockPanel.SetDock(trailing, Dock.Right);
            row.Children.Add(trailing);
        }
        var label = new TextBlock
        {
            Text = title,
            FontSize = 13,
            FontWeight = FontWeights.SemiBold,
            VerticalAlignment = VerticalAlignment.Center,
            TextTrimming = TextTrimming.CharacterEllipsis
        };
        label.SetResourceReference(TextBlock.ForegroundProperty, "TextPrimary");
        row.Children.Add(label);
        return row;
    }

    public static TextBlock SectionLabel(string text, Thickness? margin = null)
    {
        var block = new TextBlock
        {
            Text = text,
            FontSize = 11,
            FontWeight = FontWeights.SemiBold,
            Margin = margin ?? new Thickness(0, 0, 0, 6)
        };
        block.SetResourceReference(TextBlock.ForegroundProperty, "TextSecondary");
        return block;
    }

    public static Border Divider(double verticalMargin = 8)
    {
        var line = new Border
        {
            Height = 1,
            BorderThickness = new Thickness(0, 1, 0, 0),
            Margin = new Thickness(0, verticalMargin, 0, verticalMargin)
        };
        line.SetResourceReference(Border.BorderBrushProperty, "StrokeSubtle");
        return line;
    }

    public static Border FooterBar(params UIElement[] actions)
    {
        var grid = new UniformGrid { Columns = Math.Max(1, actions.Length) };
        for (var i = 0; i < actions.Length; i++)
        {
            if (actions[i] is FrameworkElement fe)
            {
                fe.Margin = new Thickness(i == 0 ? 0 : 4, 0, i == actions.Length - 1 ? 0 : 4, 0);
            }
            grid.Children.Add(actions[i]);
        }

        var bar = new Border
        {
            BorderThickness = new Thickness(0, 1, 0, 0),
            Margin = new Thickness(0, 10, 0, 0),
            Padding = new Thickness(0, 8, 0, 0),
            Child = grid
        };
        bar.SetResourceReference(Border.BorderBrushProperty, "StrokeSubtle");
        return bar;
    }

    public static TextBlock Caption(string text)
    {
        var block = new TextBlock { Text = text, FontSize = 11, TextWrapping = TextWrapping.Wrap, Margin = new Thickness(0, 2, 0, 2) };
        block.SetResourceReference(TextBlock.ForegroundProperty, "TextSecondary");
        return block;
    }
}
