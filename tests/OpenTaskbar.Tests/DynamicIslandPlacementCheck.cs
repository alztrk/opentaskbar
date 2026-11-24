using System.Runtime.InteropServices;
using System.Windows;
using System.Windows.Interop;
using OpenTaskbar.Models;
using OpenTaskbar.Services;
using OpenTaskbar.Views.Windows;

internal static class DynamicIslandPlacementCheck
{
    public static void Run()
    {
        Exception? failure = null;
        var thread = new Thread(() =>
        {
            SetThreadDpiAwarenessContext(-4);
            var app = new OpenTaskbar.App { ShutdownMode = ShutdownMode.OnExplicitShutdown };
            try
            {
                app.InitializeComponent();
                ThemeManager.Apply(AppTheme.Dark, new DockSettings());
                var displays = DisplayService.GetDisplays();
                if (displays.Count < 2 || displays.Select(d => d.Dpi).Distinct().Count() < 2)
                    throw new InvalidOperationException("This desktop check requires two monitors with different scaling.");
                foreach (var display in displays)
                {
                    using var island = new DynamicIslandWindow(display, _ => { }, _ => { }, _ => { }, _ => []);
                    island.Apply(display, [], new Dictionary<string, DockWidgetItem>(),
                        new DockSettings(DynamicIslandEnabled: true, DynamicIslandHideInFullscreen: false));
                    if (!island.IsVisible) island.Show();
                    island.UpdateLayout();
                    var hwnd = new WindowInteropHelper(island).EnsureHandle();
                    if (!GetWindowRect(hwnd, out var rect)) throw new System.ComponentModel.Win32Exception();
                    var expectedTop = (int)Math.Round(display.Bounds.Top * display.Dpi / 96);
                    var expectedCenter = (display.Bounds.Left + display.Bounds.Width / 2) * display.Dpi / 96;
                    Console.WriteLine($"{display.DeviceName}: DPI={display.Dpi}, top={rect.Top}, expected={expectedTop}, center={(rect.Left + rect.Right) / 2d}, expected={expectedCenter}");
                    if (rect.Top != expectedTop || Math.Abs((rect.Left + rect.Right) / 2d - expectedCenter) > 1 || DisplayService.GetWindowMonitor(hwnd) != display.Handle)
                        throw new InvalidOperationException($"Island is not centered at the top of {display.DeviceName}.");
                }
            }
            catch (Exception ex) { failure = ex; }
            finally { app.Shutdown(); }
        });
        thread.SetApartmentState(ApartmentState.STA);
        thread.Start();
        thread.Join();
        if (failure is not null) throw new InvalidOperationException("Dynamic Island placement failed.", failure);
    }

    [DllImport("user32.dll")]
    private static extern nint SetThreadDpiAwarenessContext(nint context);

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool GetWindowRect(nint hwnd, out DisplayService.NativeRect rect);
}
