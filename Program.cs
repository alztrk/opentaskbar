using System;
using System.Linq;
using System.Windows;
using System.Windows.Interop;
using System.Windows.Media;
using OpenTaskbar.Services;

namespace OpenTaskbar;

public static class Program
{
    [STAThread]
    public static void Main(string[] args)
    {
        var openFiles = args is ["--files"] or ["--files", _];
        var filesPath = openFiles ? args.ElementAtOrDefault(1) : null;
        var filesStore = openFiles ? new DockSettingsStore() : null;
        var filesSettings = filesStore?.Load().Settings;
        if (filesSettings is { FjordFilesEnabled: false })
        {
            FjordFilesActivation.OpenExplorerFallback(filesPath);
            return;
        }
        if (openFiles && FjordFilesActivation.TrySend(filesPath)) return;

        // Watchdog and --restore paths log before App.OnStartup; initialize first so their
        // diagnostics are not silently dropped (shell recovery was previously invisible in log.txt).
        AppLog.Initialize();

        // Must be set before App or any WPF visual tree initializes. Hardware rendering is the default;
        // --software-render and the persisted preference trade frame rate for ~100 MB less private memory.
        if (args.Contains("--software-render", StringComparer.OrdinalIgnoreCase) || RenderModePreference.IsSoftware())
        {
            RenderOptions.ProcessRenderMode = RenderMode.SoftwareOnly;
        }

        if (filesStore is not null && filesSettings is not null)
        {
            UiText.Current.SetLanguage(filesSettings.Language);
            var files = new Application { ShutdownMode = ShutdownMode.OnMainWindowClose };
            using var availability = new FjordFilesAvailability(filesStore, filesSettings.FjordFilesEnabled);
            if (!availability.Refresh())
            {
                FjordFilesActivation.OpenExplorerFallback(filesPath);
                return;
            }
            availability.Changed += enabled =>
            {
                if (!enabled) FjordFilesWindow.CloseAll();
            };
            files.Run(new FjordFilesWindow(filesPath));
            return;
        }

        if (args is ["--fjord-bar"])
        {
            var store = new DockSettingsStore();
            var settings = store.Load().Settings;
            UiText.Current.SetLanguage(settings.Language);
            FjordBarHost.Run(store, settings.FjordFilesEnabled);
            return;
        }

        var app = new App();
        app.InitializeComponent();
        // Used by Windows CI to verify the published EXE can load WPF without starting a dock
        // or changing the native Windows taskbar.
        if (args.Contains("--smoke-test", StringComparer.OrdinalIgnoreCase)) return;
        app.Run();
    }
}
