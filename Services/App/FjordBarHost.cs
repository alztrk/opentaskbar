using System.Windows;
using System.Windows.Threading;

namespace OpenTaskbar.Services;

internal static class FjordBarHost
{
    internal static void Run(DockSettingsStore store, bool filesEnabled)
    {
        using var instance = new Mutex(true, @"Local\OpenTaskbar.FjordBar", out var first);
        if (!first) return;

        var app = new Application { ShutdownMode = ShutdownMode.OnExplicitShutdown };
        using var files = new FjordFilesAvailability(store, filesEnabled);
        files.Changed += enabled =>
        {
            if (!enabled) FjordFilesWindow.CloseAll();
        };
        var windows = new Dictionary<nint, FjordBarWindow>();
        var timer = new DispatcherTimer(DispatcherPriority.Background)
        {
            Interval = TimeSpan.FromMilliseconds(750)
        };
        timer.Tick += Update;
        app.Startup += (_, _) =>
        {
            Update(null, EventArgs.Empty);
            timer.Start();
        };
        app.Exit += (_, _) => timer.Stop();
        app.Run();

        void Update(object? sender, EventArgs args)
        {
            var displays = DisplayService.GetDisplays();
            foreach (var monitor in windows.Keys.Where(key => displays.All(display => display.Handle != key)).ToArray())
            {
                windows[monitor].Close();
                windows.Remove(monitor);
            }

            var foreground = FjordBarNative.GetForegroundWindow();
            foreach (var display in displays)
            {
                if (!windows.TryGetValue(display.Handle, out var window))
                {
                    window = new FjordBarWindow(display, app.Shutdown, files);
                    windows.Add(display.Handle, window);
                    window.Show();
                }
                window.Update(display, FjordBarNative.IsFullscreen(foreground, display));
            }
        }
    }
}
