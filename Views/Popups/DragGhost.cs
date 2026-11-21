using System.Runtime.InteropServices;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Interop;
using System.Windows.Media;
using System.Windows.Media.Imaging;

namespace OpenTaskbar;

/// <summary>
/// OLE drag and drop only moves the cursor. This runs a drag with a snapshot of the dragged element
/// that follows the pointer, so the element feels attached to the mouse.
/// </summary>
public static class DragGhost
{
    private const int GwlExStyle = -20;
    private const long WsExTransparent = 0x20;
    private const long WsExToolWindow = 0x80;
    private const long WsExNoActivate = 0x08000000;
    private const uint SwpNoSize = 0x0001;
    private const uint SwpNoActivate = 0x0010;
    private static readonly nint HwndTopmost = new(-1);

    public static DragDropEffects DoDragDrop(FrameworkElement source, object data, DragDropEffects allowed)
    {
        var ghost = TryCreate(source, out var grabOffset);
        // Dim only after the snapshot so the ghost keeps full opacity; the dimmed original marks the vacated slot.
        var originalOpacity = source.Opacity;
        if (ghost is not null) source.Opacity = 0.3;
        GiveFeedbackEventHandler follow = (_, _) => Follow(ghost, grabOffset);
        source.GiveFeedback += follow;
        try
        {
            Follow(ghost, grabOffset);
            return DragDrop.DoDragDrop(source, data, allowed);
        }
        finally
        {
            source.GiveFeedback -= follow;
            source.Opacity = originalOpacity;
            ghost?.Close();
        }
    }

    private static Window? TryCreate(FrameworkElement source, out Point grabOffset)
    {
        grabOffset = default;
        if (source.ActualWidth < 1 || source.ActualHeight < 1
            || PresentationSource.FromVisual(source) is null
            || !GetCursorPos(out var cursor))
        {
            return null;
        }

        var dpi = VisualTreeHelper.GetDpi(source);
        var width = (int)Math.Ceiling(source.ActualWidth * dpi.DpiScaleX);
        var height = (int)Math.Ceiling(source.ActualHeight * dpi.DpiScaleY);
        var visual = new DrawingVisual();
        using (var context = visual.RenderOpen())
        {
            context.DrawRectangle(new VisualBrush(source), null, new Rect(0, 0, source.ActualWidth, source.ActualHeight));
        }

        var bitmap = new RenderTargetBitmap(width, height, dpi.PixelsPerInchX, dpi.PixelsPerInchY, PixelFormats.Pbgra32);
        bitmap.Render(visual);
        bitmap.Freeze();

        var origin = source.PointToScreen(new Point(0, 0));
        grabOffset = new Point(cursor.X - origin.X, cursor.Y - origin.Y);

        var ghost = new Window
        {
            Width = source.ActualWidth,
            Height = source.ActualHeight,
            WindowStyle = WindowStyle.None,
            ResizeMode = ResizeMode.NoResize,
            AllowsTransparency = true,
            Background = Brushes.Transparent,
            ShowInTaskbar = false,
            ShowActivated = false,
            Topmost = true,
            IsHitTestVisible = false,
            Opacity = 0.92,
            Content = new Image { Source = bitmap, Stretch = Stretch.Fill }
        };
        // Pass-through hit testing keeps the dock, not the ghost, as the drop target under the cursor.
        ghost.SourceInitialized += (_, _) =>
        {
            var handle = new WindowInteropHelper(ghost).Handle;
            var style = GetWindowLongPtr(handle, GwlExStyle).ToInt64();
            SetWindowLongPtr(handle, GwlExStyle, new nint(style | WsExTransparent | WsExToolWindow | WsExNoActivate));
        };
        ghost.Show();
        return ghost;
    }

    private static void Follow(Window? ghost, Point grabOffset)
    {
        if (ghost is null || !GetCursorPos(out var cursor)) return;
        var handle = new WindowInteropHelper(ghost).Handle;
        if (handle == 0) return;
        _ = SetWindowPos(handle, HwndTopmost, (int)Math.Round(cursor.X - grabOffset.X),
            (int)Math.Round(cursor.Y - grabOffset.Y), 0, 0, SwpNoSize | SwpNoActivate);
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct NativePoint
    {
        public int X;
        public int Y;
    }

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool GetCursorPos(out NativePoint point);

    [DllImport("user32.dll", EntryPoint = "GetWindowLongPtrW")]
    private static extern nint GetWindowLongPtr(nint window, int index);

    [DllImport("user32.dll", EntryPoint = "SetWindowLongPtrW")]
    private static extern nint SetWindowLongPtr(nint window, int index, nint value);

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool SetWindowPos(nint window, nint insertAfter, int x, int y, int width, int height, uint flags);
}
