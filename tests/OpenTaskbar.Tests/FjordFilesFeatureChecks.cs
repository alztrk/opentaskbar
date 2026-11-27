using System.IO;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Threading;
using OpenTaskbar;
using OpenTaskbar.Models;
using OpenTaskbar.Services;

internal static class FjordFilesFeatureChecks
{
    internal static int RunSettings() => WithStore((store, path) =>
    {
        File.WriteAllText(path, """{"ShowFjordFilesButton":true}""");
        var (legacy, error) = store.Load();
        Check(error is null && legacy.FjordFilesEnabled && legacy.IsFjordFilesButtonVisible,
            "Settings saved before the feature switch must keep Fjord Files available.");

        var disabled = legacy with { FjordFilesEnabled = false };
        Check(store.Save(disabled) is null, "The disabled preference must save.");
        var reloaded = store.Load().Settings;
        Check(!reloaded.FjordFilesEnabled && reloaded.ShowFjordFilesButton && !reloaded.IsFjordFilesButtonVisible,
            "Disabling Fjord Files must preserve the separate dock-button preference.");
        const string token = "fjord-files";
        Check(!DockItemOrdering.ApplyPinnedGroups([token], reloaded).Contains(token) &&
            DockItemOrdering.ApplyPinnedGroups([token], reloaded with { FjordFilesEnabled = true }).Contains(token),
            "The saved dock entry must disappear while disabled and return when enabled.");
        var folder = Path.GetDirectoryName(path)!;
        Check(FjordFilesActivation.GetExplorerFallbackLocation(folder) == folder &&
            FjordFilesActivation.GetExplorerFallbackLocation($"\"{folder}\"") == folder,
            "An explicit folder shortcut must retain its target when handed to Explorer.");
        Check(FjordFilesActivation.GetExplorerFallbackLocation("shell:MyComputerFolder") == "shell:MyComputerFolder" &&
            FjordFilesActivation.GetExplorerFallbackLocation("::{20D04FE0-3AEA-1069-A2D8-08002B30309D}") == "shell:MyComputerFolder",
            "This PC must retain its native Explorer target.");
        Check(FjordFilesActivation.GetExplorerFallbackLocation(null) is null &&
            FjordFilesActivation.GetExplorerFallbackLocation("  ") is null &&
            FjordFilesActivation.GetExplorerFallbackLocation(path) is null &&
            FjordFilesActivation.GetExplorerFallbackLocation(Path.Combine(folder, "missing")) is null,
            "A bare command, a file, or a missing folder must not trigger the Explorer fallback.");
        return 7;
    });

    internal static int RunUi() => WithStore((store, path) =>
    {
        using var files = new FjordFilesAvailability(store, enabled: true);
        var bounds = new Rect(0, 0, 1920, 1080);
        var display = new DisplayInfo(0, "Test", 1, "Test", bounds, bounds, true, 96);
        var bar = new FjordBarWindow(display, () => { }, files);
        try
        {
            var layout = (Grid)((Border)bar.Content).Child;
            var menu = ((Button)((DockPanel)layout.Children[0]).Children[0]).ContextMenu;
            var filesItem = (MenuItem)menu.Items[0];
            var closeItem = (MenuItem)menu.Items[1];
            Check(filesItem.Visibility == Visibility.Visible, "Fjord Files must be available by default in the top bar.");

            Check(store.Save(new DockSettings(FjordFilesEnabled: false)) is null, "Disabling Fjord Files must save.");
            var frame = new DispatcherFrame();
            var timeout = new DispatcherTimer { Interval = TimeSpan.FromSeconds(4) };
            timeout.Tick += (_, _) => frame.Continue = false;
            files.Changed += _ => frame.Continue = false;
            timeout.Start();
            try { Dispatcher.PushFrame(frame); }
            finally { timeout.Stop(); }
            Check(!files.Enabled && filesItem.Visibility == Visibility.Collapsed && closeItem.Visibility == Visibility.Visible,
                "The open top bar must hide the file entry on a settings change.");

            var disabledWrite = File.GetLastWriteTimeUtc(path);
            Check(store.Save(new DockSettings(FjordFilesEnabled: true)) is null, "Re-enabling Fjord Files must save.");
            File.SetLastWriteTimeUtc(path, disabledWrite.AddSeconds(2));
            Check(files.Refresh() && filesItem.Visibility == Visibility.Visible,
                "The open top bar must restore the file entry when re-enabled.");
            return 5;
        }
        finally { bar.Close(); }
    });

    private static int WithStore(Func<DockSettingsStore, string, int> test)
    {
        var directory = Directory.CreateTempSubdirectory("OpenTaskbar.FjordFiles.");
        var path = Path.Combine(directory.FullName, "settings.json");
        try { return test(new DockSettingsStore(path), path); }
        finally
        {
            File.Delete(path);
            directory.Delete();
        }
    }

    private static void Check(bool condition, string message)
    {
        if (!condition) throw new InvalidOperationException(message);
    }
}
