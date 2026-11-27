using System.Diagnostics;
using System.IO;
using System.Threading;
using System.Threading.Tasks;
using System.Windows;
using System.Windows.Threading;
using Microsoft.Win32;
using OpenTaskbar.Services;

namespace OpenTaskbar;

/// <summary>
/// Interaction logic for App.xaml
/// </summary>
public partial class App : Application
{
    private Mutex? _instanceMutex;
    private bool _ownsSingletonMutex;
    private int _crashHandled;
    private ApplicationPreferences? _preferences;
    public ApplicationPreferences Preferences => _preferences ??= new(new DockSettingsStore());
    public bool IsInspection { get; private set; }
    public bool IsDev { get; private set; }
    public bool OpenSettingsOnStartup { get; private set; }
    private DevReloadService? _devReload;
    private WindowsPaletteListener? _paletteListener;
    private WindowsTaskbarPinMonitor? _taskbarPinMonitor;
    private DisplayCareController? _displayCareController;

    protected override void OnStartup(StartupEventArgs e)
    {
        // Idempotent: Program.Main already initialized it for the watchdog/--restore paths.
        AppLog.Initialize();

        if (TaskbarRecoveryWatchdog.IsWatchdogInvocation(e.Args))
        {
            TaskbarRecoveryWatchdog.Run(e.Args);
            Environment.Exit(0);
            return;
        }

        if (e.Args.Contains("--restore", StringComparer.OrdinalIgnoreCase))
        {
            var restoreError = RestoreCoordinator.Restore("cli-restore");
            if (restoreError is not null) AppLog.Error(restoreError);
            // A crashed tray host leaves icons registered with its destroyed window; ask apps to re-add them to Explorer.
            OpenTaskbar.Services.TrayHost.BroadcastTaskbarCreated("restore");
            Shutdown(restoreError is null ? 0 : 1);
            return;
        }

        AppLog.Initialize();
        StartupStopwatch.Mark("OnStartup entered");
        DispatcherUnhandledException += App_DispatcherUnhandledException;
        AppDomain.CurrentDomain.UnhandledException += CurrentDomain_UnhandledException;
        TaskScheduler.UnobservedTaskException += TaskScheduler_UnobservedTaskException;
        SystemEvents.SessionSwitch += SystemEvents_SessionSwitch;
        Preferences.Changed += Preferences_Changed;
        _paletteListener = new WindowsPaletteListener();
        _paletteListener.Changed += PaletteListener_Changed;
        SystemEvents.UserPreferenceChanged += SystemEvents_UserPreferenceChanged;

        IsInspection = e.Args.Contains("--inspect", StringComparer.Ordinal);
        OpenSettingsOnStartup = e.Args.Contains("--settings", StringComparer.Ordinal);
        IsDev = e.Args.Contains("--dev", StringComparer.Ordinal);
        var sourceArg = e.Args.FirstOrDefault(arg => arg.StartsWith("--source=", StringComparison.OrdinalIgnoreCase));
        var sourceRoot = sourceArg?["--source=".Length..];
        UiText.Current.SetLanguage(Preferences.Current.Language);
        ThemeManager.Apply(Preferences.Current.Theme, Preferences.Current);
        ApplyWindowsPalette();
        AnimationFrameLimiter.Apply(Preferences.Current.AnimationFpsCap);
        _instanceMutex = new Mutex(initiallyOwned: true,
            name: IsDev ? "Local\\OpenTaskbar.Dev" : "Local\\OpenTaskbar.Singleton", out var createdNew);
        if (!createdNew)
        {
            AppLog.ReportError("AlreadyRunning");
            _instanceMutex.Dispose();
            _instanceMutex = null;
            Shutdown();
            return;
        }
        _ownsSingletonMutex = true;
        _taskbarPinMonitor = new WindowsTaskbarPinMonitor();
        _displayCareController = new DisplayCareController(Preferences);
        new StartupRegistration().SyncOnStartup(IsDev, IsInspection);

#if !DEBUG
        if (!IsDev && !IsInspection && StartupRegistration.RefreshPortableTarget() is { } startupError)
            AppLog.Warning($"Portable startup registration could not be refreshed: {startupError}.");
#endif

        // Heal only after winning the singleton race: a second instance must never consume the live instance's
        // recovery journal.
        var healError = RestoreCoordinator.HealPending("startup-heal");
        StartupStopwatch.Mark("taskbar recovery healed");
        if (healError is not null) AppLog.Error($"Startup taskbar self-heal failed: {healError}");

        var themeInstallError = ThemePacks.InstallBuiltInThemes();
        // Before any window exists, so a widget pack's sizes and fonts are in place when styles are first built.
        // Guard against legacy or corrupted theme packs left in %LOCALAPPDATA%\OpenTaskbar\Themes from older versions.
        try
        {
            ThemePacks.Select(Preferences.Current.ThemePack);
            ThemePacks.ApplyWidgets(Preferences.Current.Theme);
        }
        catch (Exception ex)
        {
            AppLog.Warning($"Failed to apply theme pack '{Preferences.Current.ThemePack}' on startup; falling back to built-in theme: {ex.Message}");
            ThemePacks.Select(null);
            ThemePacks.ApplyWidgets(Preferences.Current.Theme);
        }
        StartupStopwatch.Mark("themes and settings ready");
        base.OnStartup(e);

        MainWindow mainWindow;
        try
        {
            mainWindow = new MainWindow();
        }
        catch (Exception ex) when (ThemePacks.Current is not null)
        {
            AppLog.Warning($"MainWindow startup failed with active theme pack '{ThemePacks.Current.Id}'; retrying with built-in theme: {ex.Message}");
            ThemePacks.Select(null);
            ThemePacks.ApplyWidgets(Preferences.Current.Theme);
            mainWindow = new MainWindow();
        }
        StartupStopwatch.Mark("MainWindow constructed");
        MainWindow = mainWindow;
        mainWindow.Show();
        StartupStopwatch.Mark("MainWindow shown (first paint pending)");
        if (themeInstallError is not null)
            AppLog.Error($"Built-in theme installation: {themeInstallError}");

        if (IsDev)
            StartDevReload(sourceRoot);
    }

    private void SystemEvents_UserPreferenceChanged(object sender, UserPreferenceChangedEventArgs e)
    {
        if (e.Category is not (UserPreferenceCategory.General or UserPreferenceCategory.Color or UserPreferenceCategory.VisualStyle))
            return;
        if (Preferences.Current.Theme != OpenTaskbar.Models.AppTheme.System || Dispatcher.HasShutdownStarted) return;
        // SystemEvents uses a different thread; re-apply only if the resolved system light/dark mode actually changed.
        Dispatcher.BeginInvoke(new Action(() =>
        {
            if (Dispatcher.HasShutdownStarted || Preferences.Current.Theme != OpenTaskbar.Models.AppTheme.System) return;
            if (OpenTaskbar.Services.ThemeManager.WindowsTheme() == OpenTaskbar.Services.ThemeManager.EffectiveTheme) return;
            OpenTaskbar.Services.ThemeManager.Apply(OpenTaskbar.Models.AppTheme.System, Preferences.Current);
        }));
    }

    private void SystemEvents_SessionSwitch(object sender, SessionSwitchEventArgs e)
    {
        var unlocking = e.Reason is SessionSwitchReason.SessionUnlock or SessionSwitchReason.SessionLogon;
        var locking = e.Reason is SessionSwitchReason.SessionLock or SessionSwitchReason.SessionLogoff;
        if (!unlocking && !locking) return;

        // SystemEvents raises this on a thread-pool thread; every UI state change must happen on the dispatcher.
        Dispatcher.BeginInvoke(new Action(() =>
            (MainWindow as OpenTaskbar.MainWindow)?.ResetTransientUiStateForSessionSwitch(unlocking)));
    }

    private void ApplyWindowsPalette()
    {
        WindowsPalette.Refresh(ThemeManager.EffectiveTheme);
        ThemeManager.SetWindowsPalette(WindowsPalette.Current);
    }

    private void Preferences_Changed(object? sender, EventArgs e)
    {
        // A saved theme change rebuilds the palette; re-derive the Windows colors for the new mode.
        Dispatcher.BeginInvoke(new Action(ApplyWindowsPalette));
        AnimationFrameLimiter.Apply(Preferences.Current.AnimationFpsCap);
        _displayCareController?.Refresh();
    }

    private void PaletteListener_Changed(object? sender, EventArgs e)
    {
        // Broadcasts arrive on a worker thread; resource changes must happen on the dispatcher.
        Dispatcher.BeginInvoke(new Action(ApplyWindowsPalette));
    }

    private void StartDevReload(string? sourceRoot)
    {
        var root = sourceRoot;
        if (string.IsNullOrWhiteSpace(root) || !Directory.Exists(root))
        {
            // Default to the project root when launched from bin/<config>/<tfm>.
            root = Path.GetFullPath(Path.Combine(AppContext.BaseDirectory, "..", "..", "..", ".."));
        }

        if (!Directory.Exists(Path.Combine(root, "Design")))
        {
            AppLog.Warning($"DevReload: source root '{root}' has no Design folder; reload disabled.");
            return;
        }

        _devReload = new DevReloadService(Dispatcher, root);
        AppLog.Info($"DevReload: watching {root}");
    }

    protected override void OnExit(ExitEventArgs e)
    {
        // A second instance that lost the singleton race must never touch the live instance's
        // watchdog, native taskbar state, or recovery journal (#95).
        if (!_ownsSingletonMutex)
        {
            SystemEvents.SessionSwitch -= SystemEvents_SessionSwitch;
            Preferences.Changed -= Preferences_Changed;
            if (_paletteListener is not null)
            {
                _paletteListener.Changed -= PaletteListener_Changed;
                _paletteListener.Dispose();
                _paletteListener = null;
            }
            SystemEvents.UserPreferenceChanged -= SystemEvents_UserPreferenceChanged;
            _taskbarPinMonitor?.Dispose();
            _taskbarPinMonitor = null;
            _displayCareController?.Dispose();
            _displayCareController = null;
            AppLog.Shutdown();
            base.OnExit(e);
            return;
        }

        TaskbarRecoveryWatchdog.NotifyCleanExit();
        SystemEvents.SessionSwitch -= SystemEvents_SessionSwitch;
        Preferences.Changed -= Preferences_Changed;
        if (_paletteListener is not null)
        {
            _paletteListener.Changed -= PaletteListener_Changed;
            _paletteListener.Dispose();
            _paletteListener = null;
        }
        SystemEvents.UserPreferenceChanged -= SystemEvents_UserPreferenceChanged;
        _taskbarPinMonitor?.Dispose();
        _taskbarPinMonitor = null;
        _displayCareController?.Dispose();
        _displayCareController = null;
        var error = (MainWindow as OpenTaskbar.MainWindow)?.RestoreWindowsTaskbar();
        if (error is not null)
        {
            AppLog.Error(error);
            e.ApplicationExitCode = 1;
        }

        // The window-level restore can fail while Explorer is restarting. A leftover journal is the reliable
        // signal that the taskbar may still be hidden, so try the journal-based path before shutting down.
        if (TaskbarReplacementSession.HasPendingRecovery)
        {
            var recoveryError = RestoreCoordinator.Restore("exit-journal-heal");
            if (recoveryError is not null)
            {
                AppLog.Error($"Exit taskbar self-heal failed: {recoveryError}");
                e.ApplicationExitCode = 1;
            }
        }

        _devReload?.Dispose();
        _devReload = null;
        _instanceMutex?.ReleaseMutex();
        _instanceMutex?.Dispose();
        _instanceMutex = null;
        AppLog.Info("OpenTaskbar exited.");
        AppLog.Shutdown();
        base.OnExit(e);
    }

    protected override void OnSessionEnding(SessionEndingCancelEventArgs e)
    {
        // Another application can veto logoff after this event. Tearing the replacement down here would leave
        // the dock running with the native taskbar restored and no watchdog. OnExit performs the real restore;
        // if the session ends before it runs, the still-armed watchdog is the safety net.
        AppLog.Info("Session ending; taskbar restore deferred to OnExit.");
        base.OnSessionEnding(e);
    }

    private void App_DispatcherUnhandledException(object sender, DispatcherUnhandledExceptionEventArgs e)
    {
        e.Handled = true;
        HandleFatalError(e.Exception, "UI thread");
        Shutdown(1);
    }

    private void CurrentDomain_UnhandledException(object sender, UnhandledExceptionEventArgs e)
    {
        // The process is terminating; restore from this thread because the UI thread may be gone.
        HandleFatalError(e.ExceptionObject as Exception, "background thread");
    }

    private static void TaskScheduler_UnobservedTaskException(object? sender, UnobservedTaskExceptionEventArgs e)
    {
        AppLog.Warning($"Unobserved task exception: {e.Exception}");
        e.SetObserved();
    }

    private void HandleFatalError(Exception? exception, string origin)
    {
        if (Interlocked.Exchange(ref _crashHandled, 1) != 0) return;
        AppLog.Error($"Unhandled exception on {origin}: {exception}");
        // A crash must not leave the shell flyouts anchored to a dock that is about to disappear.
        ShellShortcuts.RestoreShellFlyoutPositions();
        var hasSession = TaskbarReplacementSession.HasActiveSession;
        if (!hasSession && !TaskbarReplacementSession.HasPendingRecovery)
        {
            AppLog.Shutdown();
            return;
        }

        var error = hasSession ? TaskbarReplacementSession.EmergencyRestore() : null;
        // EmergencyRestore reports "in progress" when another thread is already stopping. The journal is the
        // authoritative signal, so fall back to the journal-based path whenever one is still left behind.
        if (error is not null || TaskbarReplacementSession.HasPendingRecovery)
        {
            var forced = RestoreCoordinator.Restore("emergency-fallback");
            if (forced is null) error = null;
            else error ??= forced;
        }

        if (error is not null) AppLog.Error($"Emergency taskbar restore failed: {error}");
        else AppLog.Info("Windows taskbar restored after an unhandled exception.");
        AppLog.Shutdown();
    }
}
