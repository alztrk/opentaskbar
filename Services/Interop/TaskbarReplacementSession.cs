using System.Globalization;
using System.Diagnostics;
using System.IO;
using System.Runtime.InteropServices;
using System.Text;
using System.Windows;
using System.Windows.Threading;

namespace OpenTaskbar.Services;

public sealed class TaskbarReplacementSession : IDisposable
{
    // AppBar message ids live in AppBarClient; only edge/state constants are used by layout math here.
    private const uint AbeLeft = 0;
    private const uint AbeRight = 2;
    private const uint AbsAutoHide = 0x00000001;
    private const uint AbsAlwaysOnTop = 0x00000002;
    private const uint AbeBottom = 3;
    private const int GwlStyle = -16;
    private const int GwlExStyle = -20;
    private const int WindowOwner = 4;
    private const long WsCaption = 0x00C00000;
    // Wire format shared with Rust `journal.rs` and the C watchdog: ABS_AUTOHIDE | ABS_ALWAYSONTOP only.
    internal const uint SupportedTaskbarState = AbsAutoHide | AbsAlwaysOnTop;
    /// <summary>Missing/damaged journal must restore a *visible* taskbar, never a guessed hidden state.</summary>
    internal const uint DefaultTaskbarState = 0;
    internal const string JournalFileName = "taskbar-state.txt";
    internal const string JournalTemporaryExtension = ".txt.tmp";
    private const uint AbnStateChange = 0;
    private const uint AbnPosChanged = 1;
    private const uint AbnFullScreenApp = 2;
    private const uint WmDisplayChange = 0x007E;
    private const uint WmDpiChanged = 0x02E0;
    private const uint CallbackMessage = 0x8000 + 0x52;
    private const uint MonitorDefaultToNearest = 2;
    private const uint MonitorDefaultToPrimary = 1;
    private const uint GaRootOwner = 3;
    private const uint SwpNoSize = 0x0001;
    private const uint SwpNoMove = 0x0002;
    private const uint SwpNoZOrder = 0x0004;
    private const uint SwpNoActivate = 0x0010;
    private const uint SwpFrameChanged = 0x0020;
    private const uint SwpShowWindow = 0x0040;
    private const uint SwpHideWindow = 0x0080;
    private const uint SwpNoOwnerZOrder = 0x0200;
    private const uint EventObjectShow = 0x8002;
    private const uint WineventOutOfContext = 0x0000;
    private const uint WineventSkipOwnProcess = 0x0002;
    private const int SwHide = 0;
    private const int SwShowNoActivate = 4;
    private const int SwShow = 5;
    private const uint WmNull = 0x0000;
    private const uint SmtoAbortIfHung = 0x0002;
    private const uint ExplorerProbeTimeoutMs = 250;
    private const uint WinEventOutOfContext = 0x0000;
    private const uint WinEventSkipOwnProcess = 0x0002;
    private const uint WinEventFlags = WinEventOutOfContext | WinEventSkipOwnProcess;
    private const int ObjidWindow = 0;
    private const int ChildidSelf = 0;
    private static readonly nint HwndTopmost = new(-1);
    // Verification is immediate and bounded; no Thread.Sleep on the UI dispatcher during shutdown.
    private const int RestoreAttempts = 3;

    internal const int MaxDegradedReconcileAttempts = 45;
    internal const int NormalHideRequiredCleanPasses = 2;
    internal const int StartupSettleRequiredCleanPasses = 10;
    internal const int NormalHideMaxAttempts = 20;
    internal const int StartupSettleMaxAttempts = 40;

    /// <summary>
    /// The single source of truth for this session. Bits 0..3 hold the lifecycle phase; higher bits record
    /// orthogonal state (full-screen suppression and live AppBar registration) so they cannot drift out of sync
    /// with the lifecycle (for example, entering Degraded on cold boot before ABM_NEW ever succeeded).
    /// </summary>
    [Flags]
    private enum SessionState
    {
        Uninitialized = 0,
        PhaseMask = 0x0F,
        Hiding = 1,
        Registered = 2,
        Positioning = 3,
        Active = 4,
        ClearingReservation = 5,
        Degraded = 6,
        Stopping = 7,
        Restoring = 8,
        StoppedForRestart = 9,
        Failed = 10,
        Disposed = 11,
        FullScreenSuppressed = 0x10,
        AppBarRegistered = 0x20,
        FlagsMask = FullScreenSuppressed | AppBarRegistered
    }

    internal enum HideObservation
    {
        MissingTaskbar,
        AlreadyHidden,
        HidVisible,
        StillVisible
    }

    internal readonly record struct RetryDecision(bool ShouldRetry, int DelayMs, bool Exhausted);

    internal readonly record struct HideRetryStep(
        bool StopTimer,
        bool Settled,
        bool NeedsDegradedReconcile,
        bool Exhausted,
        int NextDelayMs,
        int NextConsecutiveClean);

    private readonly Window _window;
    private readonly nint _handle;
    private readonly WinEventDelegate _shellShowCallback;
    private SessionState _state;
    private uint _previousTaskbarState;
    private bool _previousTaskbarStateCaptured;
    private bool _disposed;
    private double _nativeBottomTaskbarHeight;
    private bool _nativeTaskbarAtBottom = true;
    private DispatcherTimer? _hideRetryTimer;
    private DispatcherTimer? _fullScreenPollTimer;
    private long _lastForegroundTrayEnforceMs;
    private int _hideRetryAttempts;
    private int _hideRetryConsecutiveClean;
    private bool _hideRetryStableSettle;
    private string _hideRetryReason = "Hide";
    private bool _hideRetryPending;
    private int _degradedRetryAttempts;
    private double _shelfHeight = DockMetrics.ShelfHeight;
    private double _headroomHeight = DockMetrics.Headroom;
    private (int Coordinate, int Top, int Edge, int Reserved)? _sideReservation;
    private bool _autoHideEnabled;
    private (int Left, int Bottom, int NativePixels, int ExtraHeight)? _reservation;
    private bool _lastBottomReserveSynced;
    private (int Left, int Top, int Width, int Height, bool FullScreenSuppressed)? _lastWindowPos;
    private Size _lastLayout;
    private int _stopInProgress;
    private int _reconcileActive;
    private string? _deferredReconcileReason;
    private int _deferredReconcileDelayMs = 300;
    private DispatcherTimer? _reconcileTimer;
    private nint _registeredTaskbarHandle;
    private uint _registeredTaskbarPid;
    private nint _shellShowHook;
    private readonly bool _ownsNativeTaskbar;
    private int _secondaryShelfPixels;

    public TaskbarReplacementSession(Window window, nint handle, bool autoHideEnabled, bool ownsNativeTaskbar = true)
    {
        _window = window;
        _handle = handle;
        _autoHideEnabled = autoHideEnabled;
        _ownsNativeTaskbar = ownsNativeTaskbar;
        _shellShowCallback = OnShellWindowShown;
        if (ownsNativeTaskbar) AppDomain.CurrentDomain.ProcessExit += Process_Exit;
    }

    /// <summary>
    /// True while this instance owns (or is in the middle of acquiring/releasing) Explorer's taskbar state.
    /// Mirrors the previous public contract without separate ownership/transition flags.
    /// </summary>
    public bool IsActive => IsRunning || Phase is SessionState.Stopping or SessionState.Restoring;

    private SessionState Phase => _state & SessionState.PhaseMask;

    private bool SuppressesFullScreen => (_state & SessionState.FullScreenSuppressed) != 0;

    private bool IsRunning => Phase is SessionState.Hiding or SessionState.Registered or SessionState.Positioning
        or SessionState.Active or SessionState.ClearingReservation or SessionState.Degraded;

    private bool IsRegisteredPhase => (_state & SessionState.AppBarRegistered) != 0;

    private bool CanOperate => IsRunning && !_disposed && Volatile.Read(ref _stopInProgress) == 0;

    /// <summary>True when <see cref="StopForRestart"/> ended this session for an immediate replacement restart.</summary>
    internal bool StoppedForRestart => Phase == SessionState.StoppedForRestart;

    private void SetPhase(SessionState phase) => _state = (_state & SessionState.FlagsMask) | phase;

    private void MarkRegistered(HwndLease taskbarLease = default)
    {
        if (!IsRunning) return;
        _state |= SessionState.AppBarRegistered;
        if (taskbarLease.IsValid)
        {
            _registeredTaskbarHandle = taskbarLease.Handle;
            _registeredTaskbarPid = taskbarLease.ProcessId;
        }
        if (Phase is not (SessionState.Positioning or SessionState.ClearingReservation))
        {
            SetPhase(SessionState.Registered);
    }
    }

    private void MarkUnregistered()
    {
        _state &= ~SessionState.AppBarRegistered;
        _registeredTaskbarHandle = 0;
        _registeredTaskbarPid = 0;
        if (!IsRunning || Phase is SessionState.Positioning or SessionState.ClearingReservation) return;
        SetPhase(SessionState.Hiding);
    }

    private void MarkActive()
    {
        if (IsRunning) SetPhase(SessionState.Active);
    }

    /// <summary>Enters a transient phase (positioning or reservation teardown); nested attempts are rejected.</summary>
    private bool TryEnterTransient(SessionState phase, out SessionState previous)
    {
        previous = _state;
        if (!CanOperate || Phase is SessionState.Positioning or SessionState.ClearingReservation) return false;
        _state = (previous & SessionState.FlagsMask) | phase;
        return true;
    }

    private void LeaveTransient(SessionState phase, SessionState previous)
    {
        if (Phase == phase)
        {
            // Keep full-screen and registration flag changes made while the transient phase was active.
            _state = (previous & SessionState.PhaseMask) | (_state & SessionState.FlagsMask);
        }
    }

    /// <summary>Skip AppBar work-area updates while the user is still dragging a size/scale slider.</summary>
    public bool DeferReservation { get; set; }

    // Explorer's hidden taskbar keeps reserving the work area; auto-hide releases it for the dock's hide modes and
    // for side docks, which would otherwise leave an empty strip along the bottom. It is also released when the
    // user scales the shelf below the native taskbar height, otherwise that strip would pin the work area there.
    private uint ReplacementState => (_previousTaskbarState & AbsAlwaysOnTop)
        | (_autoHideEnabled || IsVertical || !_nativeTaskbarAtBottom || ReleaseNativeStrip ? AbsAutoHide : 0);

    // Also released when the dock lives on a monitor other than the primary one: Explorer's primary taskbar would
    // otherwise keep reserving an empty strip on a display that has no dock at all (PR #165).
    private bool ReleaseNativeStrip => !IsVertical && _nativeTaskbarAtBottom
        && (ShelfHeight < _nativeBottomTaskbarHeight || !DockOnPrimaryMonitor);

    private bool DockOnPrimaryMonitor =>
        _targetMonitor == 0 || _targetMonitor == MonitorFromPoint(new NativePoint(0, 0), MonitorDefaultToPrimary);

    private Models.DockEdge _edge = Models.DockEdge.Bottom;
    private bool IsVertical => _edge != Models.DockEdge.Bottom;

    /// <summary>The screen edge the dock occupies.</summary>
    public Models.DockEdge Edge
    {
        get => _edge;
        set
        {
            if (_edge == value) return;
            if (!CanOperate)
            {
                _edge = value;
                return;
            }
            var wasRegistered = IsRegisteredPhase;
            if (wasRegistered)
            {
                // Drop the old edge's reservation while _edge still describes it, so the
                // zero-thickness ABM_SETPOS targets the correct side strip.
                ZeroAndRemoveAppBar(_edge);
            }
            _edge = value;
            _reservation = null;
            _sideReservation = null;
            _lastWindowPos = null;
            _lastLayout = default;
            if (wasRegistered && AppBarClient.Register(_handle, CallbackMessage, "Edge") != 0)
            {
                MarkRegistered(CurrentTaskbarLease());
            }

            if (_ownsNativeTaskbar)
            {
                var edgeState = SetTaskbarState(ReplacementState, _handle);
                if (edgeState != TaskbarStateResult.Applied)
                {
                    EnterDegraded(edgeState == TaskbarStateResult.MissingTaskbar
                        ? "Edge: Shell_TrayWnd missing"
                        : "Edge: SetTaskbarState failed");
                }
            }
            EnforceNativeTaskbarHidden();
            PositionDockWindow();
            ScheduleEdgeReservationSync();
        }
    }

    // Explorer releases its bottom strip asynchronously after ABS_AUTOHIDE; re-apply the new edge's AppBar
    // reservation once after a short settle delay so the work area moves immediately without a restart.
    private void ScheduleEdgeReservationSync()
    {
        var timer = new DispatcherTimer(DispatcherPriority.Background, _window.Dispatcher)
        {
            Interval = TimeSpan.FromMilliseconds(120)
        };
        timer.Tick += (_, _) =>
        {
            timer.Stop();
            if (!CanOperate) return;
            _reservation = null;
            _sideReservation = null;
            EnforceNativeTaskbarHidden();
            PositionDockWindow();
        };
        timer.Start();
    }

    private nint _targetMonitor;

    /// <summary>The monitor handle where the dock is positioned, or 0 to use nearest monitor.</summary>
    public nint TargetMonitor
    {
        get => _targetMonitor;
        set => SetTargetMonitor(value);
    }

    public void SetTargetMonitor(nint monitorHandle)
    {
        if (_targetMonitor == monitorHandle) return;
        var wasReleasingNative = ReleaseNativeStrip;
        _targetMonitor = monitorHandle;
        _reservation = null;
        _sideReservation = null;
        _lastWindowPos = null;
        _lastLayout = default;
        if (!CanOperate) return;
        if (IsRegisteredPhase)
        {
            // Zero the old monitor's strip before re-registering, so its work-area reservation
            // is released instead of lingering after the move.
            ZeroAndRemoveAppBar();
            if (AppBarClient.Register(_handle, CallbackMessage, "TargetMonitor") != 0)
            {
                MarkRegistered(CurrentTaskbarLease());
            }
        }

        if (_ownsNativeTaskbar && wasReleasingNative != ReleaseNativeStrip)
        {
            SetTaskbarState(ReplacementState, _handle);
            if (HideNativeTaskbarDetailed() != HideObservation.AlreadyHidden)
            {
                RetryHideNativeTaskbar("TargetMonitor");
            }
        }

        PositionDockWindow();
    }

    private static TaskbarReplacementSession? _current;

    /// <summary>
    /// Restores the Windows taskbar from any thread. Used by crash handlers, where the UI thread may be unusable.
    /// </summary>
    public static string? EmergencyRestore() => Volatile.Read(ref _current)?.Stop();

    /// <summary>
    /// True when a previous instance left a recovery journal behind. The journal is only removed after a verified
    /// restore, so its presence always means "the native taskbar may still be hidden".
    /// </summary>
    public static bool HasPendingRecovery => File.Exists(JournalPath);

    /// <summary>
    /// Startup self-heal: a crashed or killed instance can leave the taskbar hidden. Restore it before the dock
    /// starts, including for --inspect sessions.
    /// </summary>
    public static string? HealPendingRecovery() => HasPendingRecovery ? ForceRestoreNativeTaskbar() : null;

    /// <summary>
    /// Forcefully restores the Windows taskbar and unhides all tray windows, even if no session is active.
    /// Safe to call from any thread and from a separate process (--restore / watchdog). The journal is deleted
    /// only after the taskbar is verified visible, so a transient Explorer restart cannot lose the recovery state.
    /// </summary>
    public static string? ForceRestoreNativeTaskbar()
    {
        var state = 0u;
        var hasJournal = File.Exists(JournalPath);
        if (hasJournal)
        {
            try
            {
                // A damaged journal must fail safe: a visible taskbar with default state beats no taskbar.
                state = FallbackState(File.ReadAllText(JournalPath));
            }
            catch
            {
                state = 0;
            }

            // A journal left behind when a previous clean exit failed to delete it must not overwrite a taskbar
            // state the user has since changed. After a hard kill the taskbar is hidden, so the recorded state
            // is still applied in that case.
            if (IsNativeTaskbarVisible())
            {
                var current = AppBarClient.GetState("ForceRestore/GetState") & SupportedTaskbarState;
                if (current != (state & SupportedTaskbarState))
                {
                    AppLog.Info("Ignoring a stale taskbar journal: taskbar is already visible with a different state.");
                    state = current;
                }
            }
        }

        var error = RestoreWithVerification(state, 0, "ForceRestore");
        if (error is not null) return error;
        // The managed recovery paths (--restore, startup heal, fallback watchdog) have no in-memory registry.
        ShellShortcuts.RestoreFlyoutPositionsFromJournal();
        // A killed tray host leaves icons registered with its destroyed window; ask apps to re-add them.
        // Tagged as a restore broadcast so the live dock does not mistake it for its own icon-collection ping.
        TrayHost.BroadcastTaskbarCreated("restore");
        return DeleteJournal();
    }

    public static bool HasActiveSession => Volatile.Read(ref _current)?.IsActive == true;

    public event Action<string, bool>? OperationReported;
    public event Action<Size>? LayoutChanged;
    public event Action<bool>? FullScreenChanged;

    internal static bool IsAnySessionActive => Volatile.Read(ref _current)?.CanOperate == true;

    public void UpdateForeground(nint windowHandle)
    {
        if (!CanOperate) return;
        // The WinEvent hook handles the immediate case; this is only a safety net and enumerating every
        // secondary tray on each focus change is needlessly expensive (#128).
        if (Environment.TickCount64 - _lastForegroundTrayEnforceMs >= 500)
        {
            _lastForegroundTrayEnforceMs = Environment.TickCount64;
            EnforceSecondaryTraysHidden();
        }
        // Do not trust that the passed HWND is still the fullscreen window: Chromium's Esc bubble can briefly own
        // focus, and focus can fall back to this process without the video leaving fullscreen.
        SetFullScreenSuppression(IsAnyFullScreenOnDockMonitor());
    }

    private static bool HasPendingRecoveryFile(out string? error, out uint state)
    {
        error = null;
        state = 0;
        if (!File.Exists(JournalPath))
        {
            return false;
        }

        try
        {
            // Return the parsed value so the journal is never re-read after another recovery path deleted it.
            state = ReadJournal();
            return true;
        }
        catch (InvalidDataException)
        {
            error = "ErrorTaskbarJournalInvalid";
        }
        catch (IOException)
        {
            error = "ErrorTaskbarJournalRead";
        }
        catch (UnauthorizedAccessException)
        {
            error = "ErrorTaskbarJournalAccess";
        }

        return false;
    }

    public void Start()
    {
        ObjectDisposedException.ThrowIf(_disposed, this);
        if (IsActive)
        {
            return;
        }
        if (!_ownsNativeTaskbar)
        {
            if (AppBarClient.Register(_handle, CallbackMessage, "SecondaryDock") == 0)
                throw new InvalidOperationException("ErrorAppBarRegister");
            // MarkRegistered ignores sessions that are not running yet. Enter a running phase first, otherwise
            // the AppBarRegistered flag is never set: ReserveShelf then skips the mirrored dock's shelf and
            // ReapplyReservation sends ABM_NEW for a window that is still registered, which fails.
            SetPhase(SessionState.Registered);
            MarkRegistered();
            SetPhase(SessionState.Active);
            try { StartPositioning(); }
            catch
            {
                Stop();
                throw;
            }
            return;
        }

        var pending = HasPendingRecoveryFile(out var recoveryError, out var pendingState);
        if (recoveryError is not null)
        {
            throw new InvalidOperationException(recoveryError);
        }

        _previousTaskbarState = pending ? pendingState : 0u;
        _previousTaskbarStateCaptured = pending;
        _degradedRetryAttempts = 0;
        _registeredTaskbarHandle = 0;
        _registeredTaskbarPid = 0;
        if (!pending && NativeTaskbarWindow.TryAcquire(out var initialLease))
        {
            TryCapturePreviousTaskbarState(initialLease, updateJournal: false);
        }

        ReadNativeTaskbarHeight();
        try
        {
            if (!pending)
            {
                WriteJournal(_previousTaskbarState);
            }
        }
        catch (IOException ex)
        {
            throw new InvalidOperationException("ErrorTaskbarJournalWrite", ex);
        }
        catch (UnauthorizedAccessException ex)
        {
            throw new InvalidOperationException("ErrorTaskbarJournalWriteAccess", ex);
        }

        // Arm recovery as soon as the journal exists: a kill between the writes below and dock positioning must
        // still have an out-of-process restorer. Extraction/SmartScreen scanning runs off the UI thread.
        _ = Task.Run(() => TaskbarRecoveryWatchdog.Start(Environment.ProcessId));

        try
        {
            SetPhase(SessionState.Hiding);
            Volatile.Write(ref _current, this);
            // Watch for Explorer showing Shell_TrayWnd / Shell_SecondaryTrayWnd after logon or shell restart.
            InstallShellShowHook();
            var state = SetTaskbarState(ReplacementState, _handle);
            if (state == TaskbarStateResult.MissingTaskbar)
            {
                // No Explorer taskbar to own yet (e.g. cold Windows logon): degrade and reconcile until it appears.
                EnterDegraded("Start: Shell_TrayWnd missing");
            }
            else if (state == TaskbarStateResult.Failed)
            {
                // Explorer's Shell_TrayWnd exists but did not apply state (e.g. busy/initializing during logon).
                // Degrade and reconcile rather than aborting replacement mode and falling back to native taskbar.
                EnterDegraded("Start: SetTaskbarState failed");
            }

            var hideObservation = HideNativeTaskbarDetailed();
            if (hideObservation == HideObservation.MissingTaskbar && Phase != SessionState.Degraded)
            {
                EnterDegraded("Start: Shell_TrayWnd missing");
            }

            if (Phase != SessionState.Degraded)
            {
                if (AppBarClient.Register(_handle, CallbackMessage, "Start") != 0)
                {
                    MarkRegistered(CurrentTaskbarLease());
                }
                else
                {
                    EnterDegraded("Start: AppBarRegister failed");
                    ShellTelemetry.Result("AppBarRegister", "Start", success: false, verified: false,
                        ("cause", "degraded"));
                }
            }
            else
            {
                ShellTelemetry.Result("AppBarRegister", "Start", success: false, verified: false,
                    ("cause", "degraded"));
            }

            _reservation = null;
            _lastWindowPos = null;
            // A missing or busy Explorer taskbar leaves the session Degraded until Reconcile verifies recovery.
            if (Phase != SessionState.Degraded)
            {
                MarkActive();
                // Even when the initial hide succeeded, Explorer at Windows logon frequently finishes shell/XAML
                // initialization and shows Shell_TrayWnd or creates Shell_SecondaryTrayWnd seconds later. Run a
                // bounded startup settle check until trays stay hidden, then stop (zero idle cost).
                RetryHideNativeTaskbar("Start", requireStableSettle: true);
            }

            ShellTelemetry.Result("SessionStart", "Start", success: true,
                verified: Phase != SessionState.Degraded, ("state", _state));
            StartPositioning();
        }
        catch
        {
            Stop();
            if (IsActive) SetPhase(SessionState.Failed);
            throw;
        }
    }

    private void StartPositioning()
    {
        // Browsers enter fullscreen by resizing the already-foreground HWND (no foreground-change event) and
        // only notify the native taskbar via ITaskbarList2. Poll the window list so our dock hides too.
        if (_fullScreenPollTimer is null)
        {
            _fullScreenPollTimer = new DispatcherTimer(DispatcherPriority.Background, _window.Dispatcher)
            {
                Interval = TimeSpan.FromMilliseconds(500)
            };
            _fullScreenPollTimer.Tick += FullScreenPollTimer_Tick;
        }
        _fullScreenPollTimer.Start();
        PositionDockWindow(throwOnFailure: true);
    }

    /// <summary>
    /// Explorer broadcast <c>TaskbarCreated</c>. OS events never write state directly: they feed the same
    /// idempotent reconcile path used for ABN_* notifications and recovery.
    /// </summary>
    public void HandleTaskbarCreated()
    {
        if (!IsRunning)
        {
            return;
        }

        _degradedRetryAttempts = 0;
        ShellTelemetry.Event("TaskbarCreated", ("seq", ShellTelemetry.NextSequence()), ("state", _state));
        Reconcile("TaskbarCreated");
    }

    /// <summary>
    /// Bounds nested reconciles caused by synchronous ABN_* callbacks. The outer pass consumes at most one
    /// deferred reason; anything further is retried on a dispatcher timer instead of recursing.
    /// </summary>
    private void Reconcile(string reason)
    {
        if (!CanOperate)
        {
            ShellTelemetry.Event("ReconcileSkipped", ("reason", reason), ("state", _state));
            return;
        }
        if (!_ownsNativeTaskbar)
        {
            if (reason == "TaskbarCreated") ReapplyReservation();
            else PositionDockWindow();
            return;
        }

        if (Interlocked.Exchange(ref _reconcileActive, 1) != 0)
        {
            if (reason == "TaskbarCreated")
            {
                Volatile.Write(ref _deferredReconcileReason, reason);
                Volatile.Write(ref _deferredReconcileDelayMs, 60);
            }
            else
            {
                Interlocked.CompareExchange(ref _deferredReconcileReason, reason, null);
            }
            return;
        }

        try
        {
            for (var pass = 0; pass < 2; pass++)
            {
                var deferred = Interlocked.Exchange(ref _deferredReconcileReason, null);
                ReconcileOnce(reason);
                if (deferred is null) break;
                reason = deferred;
            }
        }
        finally
        {
            Volatile.Write(ref _reconcileActive, 0);
        }

        // Only schedule the deferred pass AFTER releasing the guard: QueueReconcile refuses to arm the timer
        // while _reconcileActive is set, so scheduling it inside the try silently dropped the late pass that
        // re-hides a secondary tray recreated by Explorer (#124).
        var pendingReason = Volatile.Read(ref _deferredReconcileReason);
        if (pendingReason is not null)
        {
            var pendingDelay = Volatile.Read(ref _deferredReconcileDelayMs);
            QueueReconcile(pendingReason, pendingDelay > 0 ? pendingDelay : 300);
        }
    }

    private void ReconcileOnce(string reason)
    {
        var sequence = ShellTelemetry.NextSequence();
        ShellTelemetry.Event("Reconcile", ("seq", sequence), ("reason", reason), ("state", _state));

        var wasDegraded = Phase == SessionState.Degraded;
        var isTaskbarCreated = reason == "TaskbarCreated";
        if (isTaskbarCreated)
        {
            // A stale reservation from the previous registration can survive an Explorer restart.
            ZeroAndRemoveAppBar();
            _registeredTaskbarHandle = 0;
            _registeredTaskbarPid = 0;
            _reservation = null;
            _sideReservation = null;
            _lastWindowPos = null;
        }

        if (!NativeTaskbarWindow.TryAcquire(out var lease))
        {
            EnterDegraded(reason);
            ShellTelemetry.Result("Reconcile", reason, success: false, verified: false,
                ("seq", sequence), ("cause", "no-taskbar"));
            return;
        }

        // If Explorer recreated Shell_TrayWnd without us receiving TaskbarCreated first (e.g. cold boot or
        // missed broadcast), drop stale registration and geometry caches bound to the old taskbar instance.
        var taskbarReplaced = IsRegisteredPhase
            && ShouldReRegisterTaskbar(IsRegisteredPhase, _registeredTaskbarHandle, _registeredTaskbarPid,
                lease.Handle, lease.ProcessId);
        if (taskbarReplaced)
        {
            _state &= ~SessionState.AppBarRegistered;
            _registeredTaskbarHandle = 0;
            _registeredTaskbarPid = 0;
            _reservation = null;
            _sideReservation = null;
            _lastWindowPos = null;
        }

        if (!_previousTaskbarStateCaptured)
        {
            TryCapturePreviousTaskbarState(lease, updateJournal: true);
        }

        try
        {
            ReadNativeTaskbarHeight();
        }
        catch (InvalidOperationException ex)
        {
            OperationReported?.Invoke(ex.Message, true);
            ShellTelemetry.Result("Reconcile", reason, success: false, verified: false,
                ("seq", sequence), ("error", ex.Message));
            return;
        }

        // Reconcile is the only path that repairs taskbar state, registration, hiding and placement together.
        var state = SetTaskbarState(ReplacementState, _handle);
        if (state == TaskbarStateResult.MissingTaskbar)
        {
            EnterDegraded(reason);
            ShellTelemetry.Result("Reconcile", reason, success: false, verified: false,
                ("seq", sequence), ("cause", "no-taskbar"));
            return;
        }

        if (state == TaskbarStateResult.Failed)
        {
            // During Windows logon or Explorer restart, Shell_TrayWnd can exist before Explorer answers
            // SHAppBarMessage. Still hide asynchronously if visible, then retry within the bounded budget.
            HideNativeTaskbarDetailed();
            EnterDegraded(reason);
            ShellTelemetry.Result("Reconcile", reason, success: false, verified: false,
                ("seq", sequence), ("cause", "setstate-failed"));
            return;
        }

        if (!IsRegisteredPhase)
        {
            if (AppBarClient.Register(_handle, CallbackMessage, reason) != 0)
            {
                MarkRegistered(lease);
            }
            else
            {
                HideNativeTaskbarDetailed();
                EnterDegraded(reason);
                ShellTelemetry.Result("Reconcile", reason, success: false, verified: false,
                    ("seq", sequence), ("cause", "register-failed"));
                return;
            }
        }

        var hideObservation = HideNativeTaskbarDetailed();
        if (hideObservation == HideObservation.MissingTaskbar)
        {
            EnterDegraded(reason);
            ShellTelemetry.Result("Reconcile", reason, success: false, verified: false,
                ("seq", sequence), ("cause", "hide-no-taskbar"));
            return;
        }

        // After cold-boot recovery, TaskbarCreated, or a replaced taskbar, require a bounded stable settle
        // because Explorer often finishes initializing XAML/secondary taskbars several seconds later.
        var requireStableSettle = wasDegraded || isTaskbarCreated || taskbarReplaced;
        if (requireStableSettle || hideObservation != HideObservation.AlreadyHidden)
        {
            RetryHideNativeTaskbar(reason, requireStableSettle);
        }

        _degradedRetryAttempts = 0;
        if (Phase == SessionState.Degraded) SetPhase(SessionState.Active);
        PositionDockWindow();
        ShellTelemetry.Result("Reconcile", reason, success: true,
            verified: IsRegisteredPhase && lease.IsValid, ("seq", sequence), ("state", _state));
    }

    private void EnterDegraded(string reason)
    {
        if (!IsRunning) return;
        if (Phase != SessionState.Degraded)
        {
            SetPhase(SessionState.Degraded);
            ShellTelemetry.Event("TaskbarDegraded", ("reason", reason), ("state", _state));
        }

        _degradedRetryAttempts++;
        var decision = EvaluateDegradedReconcileRetry(_degradedRetryAttempts);
        if (decision.ShouldRetry)
        {
            QueueReconcile(reason, decision.DelayMs);
            return;
        }

        // Bounded startup/degraded budget exhausted (~60s): stop polling so idle cost is zero.
        // EVENT_OBJECT_SHOW and TaskbarCreated remain armed to wake reconciliation on demand.
        StopReconcileTimer();
        ShellTelemetry.Event("TaskbarDegradedExhausted", ("reason", reason),
            ("attempts", _degradedRetryAttempts), ("state", _state));
        OperationReported?.Invoke("ErrorTaskbarRehide", true);
    }

    private void QueueReconcile(string reason, int delayMs = 120, bool resetDegradedBudget = false)
    {
        if (resetDegradedBudget) _degradedRetryAttempts = 0;
        if (reason == "TaskbarCreated")
        {
            Volatile.Write(ref _deferredReconcileReason, reason);
        }
        else
        {
            Interlocked.CompareExchange(ref _deferredReconcileReason, reason, null);
        }
        Volatile.Write(ref _deferredReconcileDelayMs, delayMs);
        if (Volatile.Read(ref _reconcileActive) != 0 || _disposed) return;
        if (_reconcileTimer is null)
        {
            _reconcileTimer = new DispatcherTimer(DispatcherPriority.Background, _window.Dispatcher);
            _reconcileTimer.Tick += ReconcileTimer_Tick;
        }

        var interval = TimeSpan.FromMilliseconds(delayMs);
        if (!_reconcileTimer.IsEnabled)
        {
            _reconcileTimer.Interval = interval;
            _reconcileTimer.Start();
        }
        else if (interval < _reconcileTimer.Interval)
        {
            // An urgent event (e.g. EVENT_OBJECT_SHOW) arrived while a slower degraded backoff was pending.
            _reconcileTimer.Stop();
            _reconcileTimer.Interval = interval;
            _reconcileTimer.Start();
        }
    }

    private void ReconcileTimer_Tick(object? sender, EventArgs e)
    {
        _reconcileTimer?.Stop();
        var reason = Interlocked.Exchange(ref _deferredReconcileReason, null);
        if (reason is not null) Reconcile(reason);
    }

    public bool HandleMessage(uint message, nint wParam, nint lParam)
    {
        if (!CanOperate)
        {
            return false;
        }

        if (message == CallbackMessage)
        {
            var code = unchecked((uint)wParam.ToInt64());
            var reason = code switch
            {
                AbnPosChanged => "ABN_POSCHANGED",
                AbnFullScreenApp => "ABN_FULLSCREENAPP",
                AbnStateChange => "ABN_STATECHANGE",
                _ => string.Empty
            };
            if (reason.Length == 0) return false;

            // AppBarClient suppresses inline handling while an SHAppBarMessage call is on the stack; Windows
            // dispatches sent messages during that wait, and a nested POSCHANGED/STATECHANGE must not recurse.
            if (AppBarClient.NotificationsSuspended || Phase is SessionState.Positioning
                or SessionState.ClearingReservation)
            {
                QueueReconcile(reason + "/deferred");
                return true;
            }

            switch (code)
            {
                case AbnPosChanged:
                    _lastWindowPos = null;
                    Reconcile(reason);
                    return true;
                case AbnFullScreenApp:
                    // Another app leaving fullscreen must not clear ours if a different fullscreen window is
                    // still covering the dock. Re-scan instead of trusting lParam from a single app.
                    SetFullScreenSuppression(IsAnyFullScreenOnDockMonitor());
                    PositionDockWindow();
                    return true;
                case AbnStateChange:
                    Reconcile(reason);
                    return true;
            }
        }

        if (message == WmDisplayChange)
        {
            if (_targetMonitor != 0)
            {
                var monitorInfo = new MonitorInfo { cbSize = (uint)Marshal.SizeOf<MonitorInfo>() };
                if (!GetMonitorInfo(_targetMonitor, ref monitorInfo))
                {
                    _targetMonitor = 0;
                }
            }
            _lastWindowPos = null;
            _reservation = null;
            _sideReservation = null;
            Reconcile("WM_DISPLAYCHANGE");
        }
        else if (message == WmDpiChanged)
        {
            _lastWindowPos = null;
            _reservation = null;
            _sideReservation = null;
            Reconcile("WM_DPICHANGED");
        }

        return false;
    }

    /// <summary>
    /// Ends the session without restoring and re-verifying Explorer's taskbar. Used when the dock is about to
    /// re-register immediately (e.g. a taskbar scale change): the taskbar stays hidden, so the slow show/verify
    /// cycle is unnecessary and would visibly delay the work-area resize.
    /// </summary>
    public string? StopForRestart()
    {
        if (Interlocked.CompareExchange(ref _stopInProgress, 1, 0) != 0)
        {
            return "ErrorTaskbarRestoreInProgress";
        }

        try
        {
            SetPhase(SessionState.Stopping);
            UninstallShellShowHook();
            _fullScreenPollTimer?.Stop();
            if (SuppressesFullScreen) SetFullScreenSuppression(false);
            StopHideRetryTimer();
            StopReconcileTimer();
            if (IsRegisteredPhase)
            {
                ZeroAndRemoveAppBar();
            }
            _state &= ~SessionState.AppBarRegistered;
            _registeredTaskbarHandle = 0;
            _registeredTaskbarPid = 0;
            _degradedRetryAttempts = 0;
            SetPhase(SessionState.StoppedForRestart);
            ShellTelemetry.Event("SessionStoppedForRestart", ("state", _state));
            return null;
        }
        finally
        {
            Volatile.Write(ref _stopInProgress, 0);
        }
    }

    public string? Stop()
    {
        if (Interlocked.CompareExchange(ref _stopInProgress, 1, 0) != 0)
        {
            return "ErrorTaskbarRestoreInProgress";
        }

        try
        {
            if (!_ownsNativeTaskbar)
            {
                _fullScreenPollTimer?.Stop();
                StopReconcileTimer();
                if (IsRegisteredPhase)
                    AppBarClient.Unregister(_handle, "SecondaryDockStop");
                _secondaryShelfPixels = 0;
                SetPhase(SessionState.Uninitialized);
                return null;
            }
            // A journal on disk is enough reason to restore: a previous Start() may have died between writing it
            // and entering the Hiding phase, or Explorer may have re-hidden the tray after a partial stop.
            if (!IsActive && !File.Exists(JournalPath)) return null;

            // Stop enforcing hidden state before Shell calls can reenter our AppBar callback.
            SetPhase(SessionState.Stopping);
            UninstallShellShowHook();
            _fullScreenPollTimer?.Stop();
            if (SuppressesFullScreen) SetFullScreenSuppression(false);
            StopHideRetryTimer();
            StopReconcileTimer();
            if (IsRegisteredPhase)
            {
                ZeroAndRemoveAppBar();
            }
            _state &= ~SessionState.AppBarRegistered;
            _registeredTaskbarHandle = 0;
            _registeredTaskbarPid = 0;
            _degradedRetryAttempts = 0;

            SetPhase(SessionState.Restoring);
            var restoreError = RestoreWithVerification(_previousTaskbarState, _handle);
            if (restoreError is null)
            {
                // Restore the shell flyouts before dismissing the watchdog: a kill in this window must still
                // leave both journals and an armed out-of-process restorer.
                ShellShortcuts.RestoreShellFlyoutPositions();
                restoreError = DeleteJournal();
            }
            // The watchdog must keep watching until both the taskbar and its journals are gone. Signaling earlier
            // leaves no safety net when Explorer is restarting and the in-process restore fails.
            if (restoreError is null) TaskbarRecoveryWatchdog.NotifyCleanExit();
            SetPhase(restoreError is null ? SessionState.Uninitialized : SessionState.Failed);
            return restoreError;
        }
        finally
        {
            Volatile.Write(ref _stopInProgress, 0);
        }
    }

    public void RefreshPosition()
    {
        PositionDockWindow();
    }

    /// <summary>
    /// Commits any pending AppBar work-area update after a dock geometry change (#86, #92). Applies the update
    /// in place first (smooth single-step resize); only falls back to re-registering the AppBar if Explorer
    /// still kept the previous work-area bottom.
    /// </summary>
    public void EnsureReservationSynced()
    {
        if (!CanOperate) return;
        PositionDockWindow();
        if (!IsVertical && !_lastBottomReserveSynced)
        {
            ReapplyReservation();
        }
    }

    /// <summary>
    /// Re-registers the AppBar in place (ABM_SETPOS(0) -> ABM_REMOVE -> ABM_NEW -> ABM_SETPOS) without touching
    /// Shell_TrayWnd or restarting the session. Windows 11 only recalculates the monitor work area reliably when
    /// an AppBar is freshly registered; doing this in place updates maximized-window bounds immediately while
    /// keeping the native Windows taskbar hidden (#86, #92).
    /// </summary>
    public void ReapplyReservation()
    {
        if (!CanOperate) return;
        if (IsRegisteredPhase)
        {
            ZeroAndRemoveAppBar();
        }
        if (AppBarClient.Register(_handle, CallbackMessage, "ReapplyReservation") != 0)
        {
            MarkRegistered(CurrentTaskbarLease());
        }
        else
        {
            AppLog.Warning("ReapplyReservation: ABM_NEW failed; falling back to existing window placement.");
            ShellTelemetry.Result("AppBarRegister", "ReapplyReservation", success: false, verified: false);
        }
        _reservation = null;
        _sideReservation = null;
        _lastWindowPos = null;
        PositionDockWindow();
    }

    public void UpdateAutoHide(bool autoHide)
    {
        _autoHideEnabled = autoHide;
        if (!CanOperate) return;
        var state = _ownsNativeTaskbar ? SetTaskbarState(ReplacementState, _handle) : TaskbarStateResult.Applied;
        if (state == TaskbarStateResult.MissingTaskbar)
        {
            EnterDegraded("UpdateAutoHide: Shell_TrayWnd missing");
            return;
        }
        if (state == TaskbarStateResult.Failed)
        {
            OperationReported?.Invoke("ErrorTaskbarAutoHideToggle", true);
            return;
        }
        _reservation = null;
        _sideReservation = null;
        PositionDockWindow();
    }

    private void PositionDockWindow(bool throwOnFailure = false)
    {
        if (!CanOperate || Phase is SessionState.Positioning or SessionState.ClearingReservation) return;
        if (!TryEnterTransient(SessionState.Positioning, out var previous)) return;

        // Shell positioning can synchronously notify all appbars, including this one; the phase guard above
        // makes that notification defer instead of nesting another positioning pass.
        try
        {
            PositionDock();
        }
        catch (InvalidOperationException ex)
        {
            if (throwOnFailure) throw;
            AppLog.Warning($"Dock positioning skipped: {ex.Message}");
            OperationReported?.Invoke(ex.Message, true);
        }
        finally
        {
            LeaveTransient(SessionState.Positioning, previous);
        }
    }

    private void PositionDock()
    {
        var monitor = _targetMonitor != 0 ? _targetMonitor : MonitorFromWindow(_handle, MonitorDefaultToNearest);
        var monitorInfo = new MonitorInfo { cbSize = (uint)Marshal.SizeOf<MonitorInfo>() };
        if (!GetMonitorInfo(monitor, ref monitorInfo))
        {
            throw new InvalidOperationException("ErrorDisplayDimensions");
        }

        // In mixed-DPI multi-monitor setups (#95), read the target monitor's DPI directly rather than
        // GetDpiForWindow(_handle), which still reports the previous monitor's DPI before SetWindowPos moves it.
        var dpi = monitor != 0 ? DisplayService.GetDpi(monitor) : GetDpiForWindow(_handle);
        if (dpi == 0) dpi = GetDpiForWindow(_handle);
        if (dpi == 0) throw new InvalidOperationException("ErrorDisplayDimensions");
        if (IsVertical)
        {
            PositionSideDock(monitorInfo.rcMonitor, monitorInfo.rcWork, dpi);
            return;
        }

        var headroomDips = Math.Max(DockMetrics.Headroom, HeadroomHeight);
        // The shelf comes from the user's size + scale profile. Any Explorer strip that is still present is
        // accounted for in ReserveShelf via the current work area, so it must not impose a height floor here.
        var metrics = DockLayoutMetrics.Calculate(ShelfHeight, 0, headroomDips, dpi);
        ReserveShelf(monitor, monitorInfo.rcMonitor, monitorInfo.rcWork, metrics, dpi);
        var height = metrics.ShelfPixels;
        var totalHeight = metrics.WindowPixels;
        var layout = new Size((monitorInfo.rcMonitor.Right - monitorInfo.rcMonitor.Left) * 96d / dpi,
            height * 96d / dpi);
        var totalHeightDips = totalHeight * 96d / dpi;
        if (_lastLayout != layout)
        {
            _lastLayout = layout;
            _window.Width = layout.Width;
            _window.Height = totalHeightDips;
            LayoutChanged?.Invoke(layout);
        }
        var monitorLeft = monitorInfo.rcMonitor.Left;
        var monitorRight = monitorInfo.rcMonitor.Right;
        var width = monitorRight - monitorLeft;
        var left = monitorLeft;
        UpdateForeground(GetForegroundWindow());
        var top = monitorInfo.rcMonitor.Bottom - totalHeight;
        var desiredPos = (left, top, width, totalHeight, SuppressesFullScreen);
        if (_lastWindowPos == desiredPos) return;
        _lastWindowPos = desiredPos;
        if (!SetWindowPos(_handle, WindowZOrder(SuppressesFullScreen), left, top,
                width, totalHeight,
                SwpNoActivate | SwpNoOwnerZOrder))
        {
            _lastWindowPos = null;
            throw new InvalidOperationException("ErrorDockPosition");
        }
    }

    private void ReadNativeTaskbarHeight()
    {
        var data = AppBarClient.CreateData(_handle);
        if (NativeTaskbarWindow.TryAcquire(out var taskbar)
            && AppBarClient.Send(AppBarClient.AbmGetTaskbarPos, ref data, "GetTaskbarPos") != 0)
        {
            var dpi = GetDpiForWindow(taskbar.Handle);
            if (dpi != 0 && data.rc.Bottom > data.rc.Top && data.rc.Right > data.rc.Left)
            {
                _nativeTaskbarAtBottom = data.uEdge == AppBarClient.AbeBottom;
                _nativeBottomTaskbarHeight = _nativeTaskbarAtBottom ? (data.rc.Bottom - data.rc.Top) * 96d / dpi : 0;
                AppLog.Info($"Native taskbar measured: {_nativeBottomTaskbarHeight:0.#} DIP, bottom={_nativeTaskbarAtBottom}.");
                return;
            }
        }

        // When Explorer's taskbar is hidden (e.g. recovering from a killed session or transient restart),
        // ABM_GETTASKBARPOS can fail. Fall back to standard dimensions so the replacement session can still start.
        _nativeTaskbarAtBottom = true;
        _nativeBottomTaskbarHeight = 48;
    }

    /// <summary>
    /// Computes deterministic bottom-edge AppBar reservation components from the measured native taskbar state
    /// so lagging <c>rcWork</c> values during <c>ABN_POSCHANGED</c> cannot create a feedback loop.
    /// </summary>
    internal static (int NativePixels, int ExtraHeight, int ExpectedWorkBottom) ComputeBottomReservation(
        int monitorBottom, int shelfPixels, double nativeBottomTaskbarDips, uint dpi,
        bool autoHideEnabled, bool releaseNativeStrip, bool nativeTaskbarAtBottom, bool previousWasAutoHide)
    {
        var nativeOwnsStrip = !autoHideEnabled
            && !releaseNativeStrip
            && nativeTaskbarAtBottom
            && !previousWasAutoHide;
        var nativePixels = nativeOwnsStrip
            ? Math.Min((int)Math.Ceiling(nativeBottomTaskbarDips * dpi / 96d), shelfPixels)
            : 0;
        var extraHeight = autoHideEnabled ? 0 : Math.Max(0, shelfPixels - nativePixels);
        var expectedWorkBottom = monitorBottom - (nativePixels + extraHeight);
        return (nativePixels, extraHeight, expectedWorkBottom);
    }

    private void ReserveShelf(nint monitorHandle, Rect monitor, Rect workArea, DockLayoutMetrics metrics, uint dpi)
    {
        if (!IsRegisteredPhase || DeferReservation) return;
        var (nativePixels, extraHeight, expectedWorkBottom) = ComputeBottomReservation(
            monitor.Bottom, metrics.ShelfPixels, _nativeBottomTaskbarHeight, dpi,
            _autoHideEnabled, ReleaseNativeStrip, _nativeTaskbarAtBottom,
            (_previousTaskbarState & AppBarClient.AbsAutoHide) != 0);
        if (!_ownsNativeTaskbar)
        {
            // Exclude this AppBar's existing reservation from the monitor's work-area strip.
            nativePixels = Math.Max(0, monitor.Bottom - workArea.Bottom - _secondaryShelfPixels);
            extraHeight = _autoHideEnabled ? 0 : Math.Max(0, metrics.ShelfPixels - nativePixels);
            expectedWorkBottom = monitor.Bottom - nativePixels - extraHeight;
        }
        var request = (monitor.Left, monitor.Bottom, nativePixels, extraHeight);
        if (_reservation == request) return;
        _reservation = request;
        // Official two-step Win32 AppBar protocol: query against higher-priority bars, pin the bottom edge so a
        // transient screen-bottom response cannot drop stacking, then commit via ABM_SETPOS before moving the dock.
        var bottom = monitor.Bottom - nativePixels;
        var requested = new Rect(monitor.Left, bottom - extraHeight, monitor.Right, bottom);
        var committed = AppBarClient.SetPosition(_handle, AppBarClient.AbeBottom, requested,
            (ref Rect queryResult) =>
            {
                var resolvedBottom = Math.Min(queryResult.Bottom, bottom);
                queryResult = new Rect(monitor.Left, resolvedBottom - extraHeight, monitor.Right, resolvedBottom);
            }, "ReserveShelf", out var resolved);
        if (!committed && _reservation == request)
        {
            AppLog.Warning($"Could not reserve the {extraHeight}px taskbar shelf; the work area may keep its old height.");
            ShellTelemetry.Result("AppBarSetPos", "ReserveShelf", success: false, verified: false,
                ("extra", extraHeight), ("edge", "bottom"), ("pid", Environment.ProcessId));
            _reservation = null;
            return;
        }

        var verified = new MonitorInfo { cbSize = (uint)Marshal.SizeOf<MonitorInfo>() };
        if (!_ownsNativeTaskbar && committed) _secondaryShelfPixels = extraHeight;
        var workAfter = GetMonitorInfo(monitorHandle, ref verified) ? verified.rcWork.Bottom : workArea.Bottom;
        _lastBottomReserveSynced = workAfter == expectedWorkBottom;
        AppLog.Info($"ReserveShelf: shelf={metrics.ShelfPixels}px native={nativePixels}px extra={extraHeight}px setPos=({resolved.Top}..{resolved.Bottom}) workBottom={workAfter} (expected={expectedWorkBottom}).");
        ShellTelemetry.Result("AppBarSetPos", "ReserveShelf", success: committed, verified: _lastBottomReserveSynced,
            ("shelf", metrics.ShelfPixels), ("native", nativePixels), ("extra", extraHeight),
            ("before", $"{requested.Top}..{requested.Bottom}"), ("after", $"{resolved.Top}..{resolved.Bottom}"),
            ("workBottom", workAfter), ("expected", expectedWorkBottom));
    }

    /// <summary>
    /// Left/right dock: a full-height column. The shelf width is reserved as an AppBar unless auto-hide is on;
    /// the headroom beside it (for magnified icons) overlaps windows like the bottom dock's headroom does.
    /// </summary>
    private void PositionSideDock(Rect monitor, Rect workArea, uint dpi)
    {
        var thickness = Math.Max(1, (int)Math.Round(ShelfHeight * dpi / 96d));
        var headroom = (int)Math.Round(Math.Max(DockMetrics.Headroom, HeadroomHeight) * dpi / 96d);
        var total = thickness + headroom;
        var height = monitor.Bottom - monitor.Top;
        var edge = _edge == Models.DockEdge.Left ? AbeLeft : AbeRight;
        var reserved = _autoHideEnabled ? 0 : thickness;
        var actualReserved = edge == AbeLeft
            ? Math.Max(0, workArea.Left - monitor.Left)
            : Math.Max(0, monitor.Right - workArea.Right);
        var request = (edge == AbeLeft ? monitor.Left : monitor.Right, monitor.Top, (int)edge, reserved);
        // Re-apply when our cached request changed OR Windows has not actually moved the work area yet
        // (e.g. Explorer just released its bottom strip asynchronously after switching to a side edge).
        if (IsRegisteredPhase && !DeferReservation && (_sideReservation != request || actualReserved != reserved))
        {
            _sideReservation = request;
            var requested = edge == AbeLeft
                ? new Rect(monitor.Left, monitor.Top, monitor.Left + reserved, monitor.Bottom)
                : new Rect(monitor.Right - reserved, monitor.Top, monitor.Right, monitor.Bottom);
            var committed = AppBarClient.SetPosition(_handle, edge, requested,
                (ref Rect queryResult) =>
                {
                    if (edge == AbeLeft) queryResult.Right = queryResult.Left + reserved;
                    else queryResult.Left = queryResult.Right - reserved;
                }, "PositionSideDock", out var resolved);
            if (!committed)
            {
                ShellTelemetry.Result("AppBarSetPos", "PositionSideDock", success: false, verified: false,
                    ("edge", edge), ("reserved", reserved));
                if (_sideReservation == request) _sideReservation = null;
            }
            else
            {
                ShellTelemetry.Result("AppBarSetPos", "PositionSideDock", success: true,
                    verified: resolved.Top == requested.Top && resolved.Bottom == requested.Bottom,
                    ("edge", edge), ("reserved", reserved),
                    ("before", $"{requested.Left}..{requested.Right}"), ("after", $"{resolved.Left}..{resolved.Right}"));
            }
        }

        var layout = new Size(thickness * 96d / dpi, height * 96d / dpi);
        if (_lastLayout != layout)
        {
            _lastLayout = layout;
            _window.Width = total * 96d / dpi;
            _window.Height = layout.Height;
            LayoutChanged?.Invoke(layout);
        }

        UpdateForeground(GetForegroundWindow());
        var left = edge == AbeLeft ? monitor.Left : monitor.Right - total;
        var desiredPos = (left, monitor.Top, total, height, SuppressesFullScreen);
        if (_lastWindowPos == desiredPos) return;
        _lastWindowPos = desiredPos;
        if (!SetWindowPos(_handle, WindowZOrder(SuppressesFullScreen), left, monitor.Top, total, height,
                SwpNoActivate | SwpNoOwnerZOrder))
        {
            _lastWindowPos = null;
            throw new InvalidOperationException("ErrorDockPosition");
        }
    }

    /// <summary>Shelf height in DIPs for the selected dock size; the window adds headroom above it.</summary>
    public double ShelfHeight
    {
        get => _shelfHeight;
        set
        {
            if (_shelfHeight == value) return;
            var wasReleasingNative = ReleaseNativeStrip;
            _shelfHeight = value;
            if (CanOperate)
            {
                if (_ownsNativeTaskbar && wasReleasingNative != ReleaseNativeStrip)
                {
                    SetTaskbarState(ReplacementState, _handle);
                    if (HideNativeTaskbarDetailed() != HideObservation.AlreadyHidden)
                    {
                        RetryHideNativeTaskbar("ShelfHeight");
                    }
                    _reservation = null;
                }
                PositionDockWindow();
            }
        }
    }

    /// <summary>
    /// Headroom in DIPs above the shelf. Driven by the user's external top-gap setting so the dock window grows
    /// with the gap instead of compressing the bar inside a fixed-height window.
    /// </summary>
    public double HeadroomHeight
    {
        get => _headroomHeight;
        set
        {
            if (Math.Abs(_headroomHeight - value) < 0.01) return;
            _headroomHeight = value;
            if (CanOperate) PositionDockWindow();
        }
    }

    private bool IsFullScreen(nint windowHandle)
    {
        var dockMonitor = _targetMonitor != 0 ? _targetMonitor : MonitorFromWindow(_handle, MonitorDefaultToNearest);
        return IsFullScreenOnMonitor(windowHandle, dockMonitor);
    }

    internal static bool IsFullScreenOnMonitor(nint windowHandle, nint monitorHandle)
    {
        if (windowHandle == 0 || monitorHandle == 0 || !IsWindowVisible(windowHandle) || IsIconic(windowHandle))
            return false;

        var root = GetAncestor(windowHandle, GaRootOwner);
        var targetHandle = root != 0 ? root : windowHandle;

        // Cloaked UWP/WinUI windows and click-through transparent overlays can report IsWindowVisible=true
        // with full-monitor bounds on Windows 11 26300 (#95); they must never hide the dock.
        if (IsWindowCloaked(targetHandle)) return false;
        const long WsExTransparent = 0x00000020L;
        if ((GetWindowLongPtr(targetHandle, GwlExStyle).ToInt64() & WsExTransparent) != 0) return false;

        var targetMonitor = MonitorFromWindow(targetHandle, MonitorDefaultToNearest);
        if (targetMonitor != monitorHandle) return false;

        var monitor = new MonitorInfo { cbSize = (uint)Marshal.SizeOf<MonitorInfo>() };
        if (!GetMonitorInfo(targetMonitor, ref monitor)
            || !GetWindowRect(targetHandle, out var rect))
            return false;

        var style = GetWindowLongPtr(targetHandle, GwlStyle).ToInt64();
        var zoomed = IsZoomed(targetHandle);
        if (!IsFullScreenBounds(rect, style, zoomed, monitor.rcMonitor, monitor.rcWork))
            return false;

        if (IsExcludedFullScreenClass(targetHandle, includeCoreWindow: false))
            return false;

        // The lock screen covers the whole monitor but is not a full-screen app. Treating it as one hides the dock
        // with SW_HIDE and desynchronizes it from the auto-hide state, so it pops back on its own after unlocking.
        return !IsLockScreenWindow(targetHandle);
    }

    private void FullScreenPollTimer_Tick(object? sender, EventArgs e)
    {
        if (!CanOperate) return;
        // Explorer may recreate a secondary tray without a delivered WinEvent. Keep the native taskbar hidden
        // throughout the active session, including when the event hook could not be installed.
        EnforceSecondaryTraysHidden();
        SetFullScreenSuppression(IsAnyFullScreenOnDockMonitor());
    }

    /// <summary>
    /// Checks every visible top-level window on the dock's monitor. A fullscreen browser owns the foreground for
    /// its video, but its Esc bubble or another popup can briefly take focus; testing only GetForegroundWindow()
    /// would flicker the dock back on top of the video.
    /// </summary>
    // PositionDock and the 500 ms poll both scan fullscreen state; coalesce bursts so a single
    // reposition does not enumerate windows several times within the same tick.
    private long _lastFullScreenScanMs;
    private bool _lastFullScreenScanResult;

    private bool IsAnyFullScreenOnDockMonitor()
    {
        var now = Environment.TickCount64;
        if (now - _lastFullScreenScanMs < 200) return _lastFullScreenScanResult;
        _lastFullScreenScanMs = now;

        var monitor = _targetMonitor != 0 ? _targetMonitor : MonitorFromWindow(_handle, MonitorDefaultToNearest);
        _lastFullScreenScanResult = IsAnyFullScreenOnMonitor(monitor);
        return _lastFullScreenScanResult;
    }

    internal static bool IsAnyFullScreenOnMonitor(nint monitorHandle)
    {
        if (monitorHandle == 0) return false;
        var found = false;
        EnumWindows((handle, _) =>
        {
            if (found) return false;
            GetWindowThreadProcessId(handle, out var processId);
            if (processId == Environment.ProcessId) return true;
            if (!IsFullScreenOnMonitor(handle, monitorHandle)) return true;
            // Monitor-sized shell surfaces (desktop, IME, GPU overlays) must never hide the dock. Only a real
            // application window that owns a taskbar button counts as a fullscreen app. Static shell classes
            // are excluded even when their style would otherwise look eligible (e.g. TextInputHost).
            if (!HasTaskbarButtonWindow(handle)) return true;
            if (IsExcludedFullScreenClass(handle, includeCoreWindow: true))
            {
                return true;
            }
            found = true;
            return false;
        }, 0);
        return found;
    }

    private static bool HasTaskbarButtonWindow(nint handle)
    {
        var extendedStyle = GetWindowLongPtr(handle, GwlExStyle).ToInt64();
        var appWindow = (extendedStyle & TaskbarWindowRules.AppWindow) != 0;
        var noActivate = (extendedStyle & TaskbarWindowRules.NoActivate) != 0;
        var toolWindow = (extendedStyle & TaskbarWindowRules.ToolWindow) != 0;
        return GetWindow(handle, WindowOwner) == 0
            ? (!noActivate || appWindow) && !toolWindow
            : appWindow;
    }

    private static bool IsExcludedFullScreenClass(nint handle, bool includeCoreWindow)
    {
        // Local buffer: this helper is called from both the poll timer and WindowProc for every
        // enumerated window; a shared field would be fragile if either ever moves off the dispatcher.
        var className = new char[128];
        var length = GetClassName(handle, className, className.Length);
        return length > 0 && IsExcludedFullScreenClass(className.AsSpan(0, length), includeCoreWindow);
    }

    internal static bool IsExcludedFullScreenClass(ReadOnlySpan<char> className, bool includeCoreWindow) =>
        className is "Progman" or "WorkerW" or "Shell_TrayWnd" or "LogonUI" or "LockAppWindow"
        || (includeCoreWindow && className is "Windows.UI.Core.CoreWindow");

    private static bool IsLockScreenWindow(nint handle)
    {
        GetWindowThreadProcessId(handle, out var processId);
        if (processId == 0) return false;
        try
        {
            using var process = Process.GetProcessById((int)processId);
            return process.ProcessName is "LogonUI" or "LockApp";
        }
        catch (ArgumentException) { return false; }
        catch (InvalidOperationException) { return false; }
    }

    internal static bool IsFullScreenBounds(Rect windowRect, long windowStyle, bool isZoomed, Rect monitorBounds, Rect workBounds)
    {
        var coversMonitor = windowRect.Left <= monitorBounds.Left + 2 && windowRect.Top <= monitorBounds.Top + 2
            && windowRect.Right >= monitorBounds.Right - 2 && windowRect.Bottom >= monitorBounds.Bottom - 2;
        if (!coversMonitor) return false;

        var workAreaNearMonitor = Math.Abs(workBounds.Left - monitorBounds.Left) <= 2
            && Math.Abs(workBounds.Top - monitorBounds.Top) <= 2
            && Math.Abs(workBounds.Right - monitorBounds.Right) <= 2
            && Math.Abs(workBounds.Bottom - monitorBounds.Bottom) <= 2;

        // Maximized desktop windows (including custom-chrome Chromium/Electron apps without WS_CAPTION)
        // carry resize-border overhang beyond monitorBounds or retain WS_CAPTION; true fullscreen apps do not.
        var hasMaximizedBorderOverhang = windowRect.Left < monitorBounds.Left - 2
            || windowRect.Top < monitorBounds.Top - 2
            || windowRect.Right > monitorBounds.Right + 2
            || windowRect.Bottom > monitorBounds.Bottom + 2;

        if (workAreaNearMonitor && isZoomed
            && ((windowStyle & WsCaption) == WsCaption || hasMaximizedBorderOverhang))
        {
            return false;
        }

        return true;
    }

    internal static nint WindowZOrder(bool fullScreenSuppressed) =>
        fullScreenSuppressed ? new nint(1) : HwndTopmost;

    internal static bool ApplyFullScreenTransition(
        ref bool currentSuppressed,
        ref (int Left, int Top, int Width, int Height, bool FullScreenSuppressed)? lastWindowPos,
        bool suppressed,
        Action<int> showWindow,
        Action reposition,
        Action<bool>? notifyChanged)
    {
        if (currentSuppressed == suppressed) return false;
        currentSuppressed = suppressed;
        // Invalidate the cached placement so leaving fullscreen always re-applies HWND_TOPMOST after any
        // prior HWND_BOTTOM demotion, instead of staying behind other windows.
        lastWindowPos = null;
        showWindow(suppressed ? SwHide : SwShowNoActivate);
        if (!suppressed)
        {
            reposition();
        }
        notifyChanged?.Invoke(suppressed);
        return true;
    }

    private void SetFullScreenSuppression(bool suppressed)
    {
        if (SuppressesFullScreen == suppressed) return;
        AppLog.Info($"FullScreenSuppression changed: suppressed={suppressed}.");
        var current = SuppressesFullScreen;
        ApplyFullScreenTransition(
            ref current,
            ref _lastWindowPos,
            suppressed,
            cmd => ShowWindow(_handle, cmd),
            () =>
            {
                // Update the state bit before repositioning so the Z-order matches the new suppression value.
                ApplySuppressionFlag(suppressed);
                PositionDockWindow();
            },
            FullScreenChanged);
        ApplySuppressionFlag(suppressed);
        ShellTelemetry.Event("FullScreenSuppression", ("suppressed", suppressed), ("state", _state));
    }

    private void ApplySuppressionFlag(bool suppressed) =>
        _state = suppressed
            ? _state | SessionState.FullScreenSuppressed
            : _state & ~SessionState.FullScreenSuppressed;

    private static bool IsWindowCloaked(nint handle)
    {
        const int DwmwaCloaked = 14;
        return DwmGetWindowAttribute(handle, DwmwaCloaked, out int cloaked, sizeof(int)) == 0 && cloaked != 0;
    }

    internal static bool IsPrimaryTaskbarClassName(ReadOnlySpan<char> className) =>
        className.Equals("Shell_TrayWnd", StringComparison.OrdinalIgnoreCase);

    internal static bool IsSecondaryTaskbarClassName(ReadOnlySpan<char> className) =>
        className.Equals("Shell_SecondaryTrayWnd", StringComparison.OrdinalIgnoreCase);

    internal static bool IsNativeTaskbarClassName(ReadOnlySpan<char> className) =>
        IsPrimaryTaskbarClassName(className) || IsSecondaryTaskbarClassName(className);

    /// <summary>
    /// True when the session needs to register (or re-register) its AppBar with Explorer: either ABM_NEW has not
    /// succeeded yet (e.g. started Degraded at cold boot), or Explorer replaced Shell_TrayWnd with a new HWND/PID.
    /// </summary>
    internal static bool ShouldReRegisterTaskbar(
        bool isAppBarRegistered,
        nint registeredTaskbarHandle,
        uint registeredTaskbarPid,
        nint currentTaskbarHandle,
        uint currentTaskbarPid)
    {
        if (!isAppBarRegistered) return true;
        if (registeredTaskbarHandle == 0 || currentTaskbarHandle == 0) return false;
        return registeredTaskbarHandle != currentTaskbarHandle || registeredTaskbarPid != currentTaskbarPid;
    }

    /// <summary>
    /// Bounded backoff schedule for recovering from a missing or busy Explorer taskbar (e.g. cold Windows logon
    /// or shell restart). Covers ~60 s (10×500 ms + 15×1000 ms + 20×2000 ms), then stops with zero idle timer
    /// cost while EVENT_OBJECT_SHOW and TaskbarCreated remain armed to wake recovery on demand.
    /// </summary>
    internal static RetryDecision EvaluateDegradedReconcileRetry(int attemptNumber)
    {
        if (attemptNumber < 1) attemptNumber = 1;
        if (attemptNumber > MaxDegradedReconcileAttempts)
        {
            return new RetryDecision(ShouldRetry: false, DelayMs: 0, Exhausted: true);
        }

        var delayMs = attemptNumber <= 10
            ? 500
            : attemptNumber <= 25
                ? 1000
                : 2000;
        return new RetryDecision(ShouldRetry: true, DelayMs: delayMs, Exhausted: false);
    }

    internal static int HideRetryInitialDelayMs(bool requireStableSettle) =>
        requireStableSettle ? 500 : 250;

    /// <summary>
    /// Pure step function for bounded hide enforcement. Requires consecutive <see cref="HideObservation.AlreadyHidden"/>
    /// observations so startup/TaskbarCreated enforcement does not stop before Explorer finishes logon shell init,
    /// while still stopping with zero idle cost once trays stay hidden or the bounded budget ends.
    /// </summary>
    internal static HideRetryStep EvaluateHideRetryStep(
        HideObservation observation,
        int attemptNumber,
        int consecutiveClean,
        bool requireStableSettle)
    {
        if (attemptNumber < 1) attemptNumber = 1;
        if (consecutiveClean < 0) consecutiveClean = 0;

        if (observation == HideObservation.MissingTaskbar)
        {
            return new HideRetryStep(
                StopTimer: true,
                Settled: false,
                NeedsDegradedReconcile: true,
                Exhausted: false,
                NextDelayMs: 0,
                NextConsecutiveClean: 0);
        }

        var requiredClean = requireStableSettle ? StartupSettleRequiredCleanPasses : NormalHideRequiredCleanPasses;
        var maxAttempts = requireStableSettle ? StartupSettleMaxAttempts : NormalHideMaxAttempts;
        var nextDelayMs = requireStableSettle
            ? (attemptNumber <= 6 ? 500 : attemptNumber <= 20 ? 1000 : 1500)
            : (attemptNumber <= 6 ? 250 : attemptNumber <= 14 ? 500 : 1000);

        if (observation == HideObservation.AlreadyHidden)
        {
            var nextClean = consecutiveClean + 1;
            if (nextClean >= requiredClean || attemptNumber >= maxAttempts)
            {
                return new HideRetryStep(
                    StopTimer: true,
                    Settled: true,
                    NeedsDegradedReconcile: false,
                    Exhausted: false,
                    NextDelayMs: 0,
                    NextConsecutiveClean: nextClean);
            }

            return new HideRetryStep(
                StopTimer: false,
                Settled: false,
                NeedsDegradedReconcile: false,
                Exhausted: false,
                NextDelayMs: nextDelayMs,
                NextConsecutiveClean: nextClean);
        }

        // A tray was visible (HidVisible or StillVisible): reset the stability streak.
        if (attemptNumber >= maxAttempts)
        {
            var settled = observation == HideObservation.HidVisible;
            return new HideRetryStep(
                StopTimer: true,
                Settled: settled,
                NeedsDegradedReconcile: false,
                Exhausted: !settled,
                NextDelayMs: 0,
                NextConsecutiveClean: 0);
        }

        return new HideRetryStep(
            StopTimer: false,
            Settled: false,
            NeedsDegradedReconcile: false,
            Exhausted: false,
            NextDelayMs: nextDelayMs,
            NextConsecutiveClean: 0);
    }

    private static HwndLease CurrentTaskbarLease() =>
        NativeTaskbarWindow.TryAcquire(out var lease) ? lease : HwndLease.Invalid;

    private static bool IsWindowResponsive(nint handle) =>
        handle != 0 && SendMessageTimeout(handle, WmNull, 0, 0, SmtoAbortIfHung, ExplorerProbeTimeoutMs, out _) != 0;

    private bool TryCapturePreviousTaskbarState(HwndLease lease, bool updateJournal)
    {
        if (!lease.IsValid || !IsWindowResponsive(lease.Handle))
        {
            return false;
        }

        var captured = AppBarClient.GetState("CapturePreviousState") & SupportedTaskbarState;
        _previousTaskbarState = captured;
        _previousTaskbarStateCaptured = true;
        if (updateJournal)
        {
            try
            {
                WriteJournal(captured);
            }
            catch (Exception ex) when (ex is IOException or UnauthorizedAccessException)
            {
                AppLog.Warning($"Could not update taskbar recovery journal after delayed Explorer capture: {ex.Message}");
            }
        }
        return true;
    }

    private void InstallShellShowHook()
    {
        if (_shellShowHook != 0 || _disposed) return;
        _shellShowHook = SetWinEventHook(EventObjectShow, EventObjectShow, 0, _shellShowCallback, 0, 0, WinEventFlags);
        if (_shellShowHook == 0)
        {
            AppLog.Warning("Shell taskbar show-event hook unavailable; relying on TaskbarCreated and bounded hide retries.");
        }
    }

    private void UninstallShellShowHook()
    {
        var hook = Interlocked.Exchange(ref _shellShowHook, 0);
        if (hook == 0) return;
        if (_window.Dispatcher.CheckAccess())
        {
            UnhookWinEvent(hook);
            return;
        }

        try
        {
            if (!_window.Dispatcher.HasShutdownStarted)
            {
                _window.Dispatcher.BeginInvoke(DispatcherPriority.Send, () => UnhookWinEvent(hook));
                return;
            }
        }
        catch
        {
            // Dispatcher unavailable during crash or process exit.
        }

        UnhookWinEvent(hook);
    }

    // Invoked from a native WinEvent hook: an escaping exception crosses the unmanaged boundary and can take
    // down the process, and blocking cross-process ShowWindow calls would freeze the UI dispatcher if Explorer
    // is busy during logon. Keep the fast path allocation-free and strictly non-blocking.
    private void OnShellWindowShown(nint hook, uint eventType, nint windowHandle, int objectId, int childId,
        uint eventThread, uint eventTime)
    {
        try
        {
            if (eventType != EventObjectShow || objectId != ObjidWindow || childId != ChildidSelf
                || windowHandle == 0 || !CanOperate)
            {
                return;
            }

            Span<ushort> classBuffer = stackalloc ushort[64];
            var length = GetClassNameRef(windowHandle, ref MemoryMarshal.GetReference(classBuffer), classBuffer.Length);
            if (length <= 0) return;

            var className = MemoryMarshal.Cast<ushort, char>(classBuffer[..length]);
            if (!IsNativeTaskbarClassName(className)) return;

            var lease = HwndLease.Capture(windowHandle);
            if (!lease.IsValid || !lease.IsForeign) return;

            var isPrimary = IsPrimaryTaskbarClassName(className);
            var visible = IsWindowVisible(windowHandle);
            var needsFullReconcile = isPrimary
                && (Phase == SessionState.Degraded
                    || ShouldReRegisterTaskbar(IsRegisteredPhase, _registeredTaskbarHandle, _registeredTaskbarPid,
                        lease.Handle, lease.ProcessId));

            if (!visible && !needsFullReconcile) return;

            ShellTelemetry.Event("NativeTaskbarShown",
                ("class", className.ToString()), ("hwnd", windowHandle.ToInt64()),
                ("pid", lease.ProcessId), ("reconcile", needsFullReconcile), ("state", _state));

            if (visible)
            {
                ShowWindowAsync(windowHandle, SwHide);
            }

            if (needsFullReconcile)
            {
                QueueReconcile("EVENT_OBJECT_SHOW/Shell_TrayWnd", 60, resetDegradedBudget: true);
            }
            else
            {
                RetryHideNativeTaskbar(
                    isPrimary ? "EVENT_OBJECT_SHOW/Shell_TrayWnd" : "EVENT_OBJECT_SHOW/Shell_SecondaryTrayWnd",
                    requireStableSettle: false);
            }
        }
        catch (Exception ex)
        {
            AppLog.Warning($"Shell show event failed: {ex.Message}");
        }
    }

    private static bool TryHideWindow(nint handle)
    {
        if (IsWindowResponsive(handle))
        {
            ShowWindow(handle, SwHide);
            return !IsWindowVisible(handle);
        }

        // Explorer is busy/hung (common during Windows logon): avoid blocking the UI dispatcher in ShowWindow.
        ShowWindowAsync(handle, SwHide);
        return false;
    }

    /// <summary>
    /// Periodic safety net for secondary docks: Explorer can recreate a Shell_SecondaryTrayWnd long after
    /// startup (e.g. after a TaskbarCreated broadcast). Hides any that are still visible without touching
    /// the primary taskbar or the session's state machine.
    /// </summary>
    private static void HideTrayWindow(nint handle)
    {
        ShowWindow(handle, SwHide);
        SetWindowPos(handle, 0, 0, 0, 0, 0,
            SwpNoMove | SwpNoSize | SwpNoZOrder | SwpNoActivate | SwpHideWindow);
    }

    internal static void EnforceSecondaryTraysHidden()
    {
        foreach (var secondary in NativeTaskbarWindow.FindSecondaryLeases())
        {
            if (IsWindowVisible(secondary.Handle)) TryHideWindow(secondary.Handle);
        }
    }

    private static HideObservation HideNativeTaskbarDetailed()
    {
        if (!NativeTaskbarWindow.TryAcquire(out var taskbar))
        {
            return HideObservation.MissingTaskbar;
        }

        var hadVisible = false;
        var asyncFallback = false;

        if (IsWindowVisible(taskbar.Handle))
        {
            hadVisible = true;
            if (!TryHideWindow(taskbar.Handle)) asyncFallback = true;
        }

        var secondaries = NativeTaskbarWindow.FindSecondaryLeases();
        foreach (var secondary in secondaries)
        {
            if (IsWindowVisible(secondary.Handle))
            {
                hadVisible = true;
                if (!TryHideWindow(secondary.Handle)) asyncFallback = true;
            }
        }

        // Every tray must be gone before reporting hidden. Checking only the primary used to stop the
        // retry loop while a secondary taskbar was still painted over the secondary dock (#124).
        if (asyncFallback || IsWindowVisible(taskbar.Handle)) return HideObservation.StillVisible;
        foreach (var secondary in secondaries)
        {
            if (IsWindowVisible(secondary.Handle)) return HideObservation.StillVisible;
        }

        return hadVisible ? HideObservation.HidVisible : HideObservation.AlreadyHidden;
    }

    private static bool ShowNativeTaskbar()
    {
        if (!NativeTaskbarWindow.TryAcquire(out var taskbar))
        {
            return false;
        }

        ShowWindow(taskbar.Handle, SwShow);
        SetWindowPos(taskbar.Handle, 0, 0, 0, 0, 0,
            SwpNoMove | SwpNoSize | SwpNoZOrder | SwpNoActivate | SwpShowWindow | SwpFrameChanged);

        foreach (var secondary in NativeTaskbarWindow.FindSecondaryLeases())
        {
            ShowWindow(secondary.Handle, SwShow);
            SetWindowPos(secondary.Handle, 0, 0, 0, 0, 0,
                SwpNoMove | SwpNoSize | SwpNoZOrder | SwpNoActivate | SwpShowWindow | SwpFrameChanged);
        }

        return true;
    }

    private static bool IsNativeTaskbarVisible()
    {
        if (!NativeTaskbarWindow.TryAcquire(out var taskbar) || !IsWindowVisible(taskbar.Handle)) return false;
        // A primary-only check could delete the journal while secondary trays are still hidden.
        foreach (var secondary in NativeTaskbarWindow.FindSecondaryLeases())
        {
            if (!IsWindowVisible(secondary.Handle)) return false;
        }
        return true;
    }

    /// <summary>
    /// Re-asserts state and visibility and requires two consecutive clean observations before declaring success.
    /// Verification is immediate and bounded: it never sleeps on the UI dispatcher (the previous implementation
    /// slept 40 ms between passes during shutdown). Background crash/watchdog threads may yield a few milliseconds so
    /// Explorer can process an earlier ABN_STATECHANGE before the next observation.
    /// </summary>
    private static string? RestoreWithVerification(uint state, nint dockHandle, string reason = "Stop")
    {
        var sequence = ShellTelemetry.NextSequence();
        var backgroundThread = Application.Current?.Dispatcher.CheckAccess() != true;
        var consecutive = 0;
        for (var attempt = 0; attempt < RestoreAttempts; attempt++)
        {
            var restored = TryRestoreOnce(state, dockHandle);
            consecutive = restored ? consecutive + 1 : 0;
            if (consecutive >= 2)
            {
                ShellTelemetry.Result("TaskbarRestore", reason, success: true, verified: true,
                    ("seq", sequence), ("attempts", attempt + 1));
                return null;
            }

            // Explorer can re-apply auto-hide right after we show the taskbar, and secondary tray windows
            // can appear a moment later. Give them time to settle before the next verification pass.
            if (backgroundThread && attempt < RestoreAttempts - 1) Thread.Sleep(40);
        }

        ShellTelemetry.Result("TaskbarRestore", reason, success: false, verified: false,
            ("seq", sequence), ("attempts", RestoreAttempts), ("state", state));
        return "ErrorTaskbarRestoreFailed";
    }

    private static bool TryRestoreOnce(uint state, nint dockHandle) =>
        SetTaskbarState(state, dockHandle) == TaskbarStateResult.Applied
        && ShowNativeTaskbar()
        && IsNativeTaskbarVisible();

    private void EnforceNativeTaskbarHidden()
    {
        if (!_ownsNativeTaskbar || !CanOperate) return;
        var result = SetTaskbarState(ReplacementState, _handle);
        if (result == TaskbarStateResult.MissingTaskbar)
        {
            EnterDegraded("EnforceHidden: Shell_TrayWnd missing");
            return;
        }
        if (result == TaskbarStateResult.Failed)
        {
            OperationReported?.Invoke("TaskbarAutoHideReenabled", false);
            ShellTelemetry.Result("TaskbarAutoHide", "EnforceHidden", success: false, verified: false);
            return;
        }

        var observation = HideNativeTaskbarDetailed();
        if (observation == HideObservation.MissingTaskbar)
        {
            EnterDegraded("EnforceHidden: Shell_TrayWnd missing");
        }
        else if (observation != HideObservation.AlreadyHidden)
        {
            RetryHideNativeTaskbar("EnforceHidden");
        }
    }

    // Explorer briefly re-shows an auto-hidden taskbar when the pointer touches the screen edge, and at
    // Windows logon or after TaskbarCreated it can finish XAML shell init / create secondary trays seconds later.
    // Enforce hiding until consecutive clean observations confirm stability, then stop with zero idle cost.
    private void RetryHideNativeTaskbar(string reason = "Hide", bool requireStableSettle = false)
    {
        if (!CanOperate) return;
        var wasPending = _hideRetryPending;
        _hideRetryPending = true;
        _hideRetryAttempts = 0;
        _hideRetryConsecutiveClean = 0;
        // Never downgrade an active startup/TaskbarCreated settle window to a shorter retry.
        _hideRetryStableSettle = wasPending ? (_hideRetryStableSettle || requireStableSettle) : requireStableSettle;
        _hideRetryReason = reason;
        ShellTelemetry.Event("NativeTaskbarHideRetry", ("reason", reason), ("attempt", 0),
            ("stableSettle", _hideRetryStableSettle));

        var initialDelay = TimeSpan.FromMilliseconds(HideRetryInitialDelayMs(_hideRetryStableSettle));
        if (_hideRetryTimer is null)
        {
            _hideRetryTimer = new DispatcherTimer(DispatcherPriority.Background, _window.Dispatcher)
            {
                Interval = initialDelay
            };
            _hideRetryTimer.Tick += HideRetryTimer_Tick;
        }
        else if (_hideRetryTimer.Interval != initialDelay)
        {
            _hideRetryTimer.Interval = initialDelay;
        }

        if (!_hideRetryTimer.IsEnabled)
        {
            _hideRetryTimer.Start();
        }
    }

    private void StopHideRetryTimer()
    {
        _hideRetryTimer?.Stop();
        _hideRetryPending = false;
        _hideRetryAttempts = 0;
        _hideRetryConsecutiveClean = 0;
        _hideRetryStableSettle = false;
    }

    private void StopReconcileTimer()
    {
        _reconcileTimer?.Stop();
        Interlocked.Exchange(ref _deferredReconcileReason, null);
    }

    private void HideRetryTimer_Tick(object? sender, EventArgs e)
    {
        if (!CanOperate)
        {
            StopHideRetryTimer();
            return;
        }

        _hideRetryAttempts++;
        var observation = HideNativeTaskbarDetailed();
        var step = EvaluateHideRetryStep(observation, _hideRetryAttempts, _hideRetryConsecutiveClean, _hideRetryStableSettle);
        _hideRetryConsecutiveClean = step.NextConsecutiveClean;

        if (step.NeedsDegradedReconcile)
        {
            StopHideRetryTimer();
            EnterDegraded("HideRetry: Shell_TrayWnd missing");
            return;
        }

        if (step.StopTimer)
        {
            var attempts = _hideRetryAttempts;
            var clean = _hideRetryConsecutiveClean;
            var stableSettle = _hideRetryStableSettle;
            var reason = _hideRetryReason;
            StopHideRetryTimer();
            ShellTelemetry.Result("NativeTaskbarHide", reason,
                success: step.Settled, verified: step.Settled && observation != HideObservation.StillVisible,
                ("attempts", attempts), ("clean", clean), ("stableSettle", stableSettle));
            if (step.Exhausted) OperationReported?.Invoke("ErrorTaskbarRehide", true);
            return;
        }

        if (_hideRetryTimer is not null)
        {
            var nextInterval = TimeSpan.FromMilliseconds(step.NextDelayMs);
            if (_hideRetryTimer.Interval != nextInterval) _hideRetryTimer.Interval = nextInterval;
        }
    }

    private enum TaskbarStateResult
    {
        Applied,
        MissingTaskbar,
        Failed
    }

    /// <summary>
    /// Publishes the taskbar state to Explorer's validated window only. The dock/spy HWND is never a fallback:
    /// without Shell_TrayWnd there is nobody to own the state, so callers enter Degraded and schedule a reconcile.
    /// </summary>
    private static TaskbarStateResult SetTaskbarState(uint state, nint dockHandle)
    {
        // Kept in the signature for call-site clarity; deliberately unused (no self-fallback).
        _ = dockHandle;
        var hasTaskbar = NativeTaskbarWindow.TryAcquire(out var taskbar);
        // Explorer can notify us about our own update; an identical state must not publish another one.
        var current = AppBarClient.GetState("SetState/current");
        // Comparing only the auto-hide bit silently loses ABS_ALWAYSONTOP on restore.
        if ((current & SupportedTaskbarState) == (state & SupportedTaskbarState))
        {
            return hasTaskbar ? TaskbarStateResult.Applied : TaskbarStateResult.MissingTaskbar;
        }

        if (!hasTaskbar)
        {
            ShellTelemetry.Event("SetTaskbarStateSkipped", ("cause", "no-taskbar"), ("state", state));
            return TaskbarStateResult.MissingTaskbar;
        }

        var data = AppBarClient.CreateData(taskbar.Handle);
        data.lParam = unchecked((nint)state);
        var reply = AppBarClient.SetState(ref data, "SetTaskbarState");
        current = AppBarClient.GetState("SetState/verify");
        var applied = (current & SupportedTaskbarState) == (state & SupportedTaskbarState);
        ShellTelemetry.Result("SetTaskbarState", "SetState", success: applied, verified: applied,
            ("state", state), ("reply", (long)reply), ("pid", taskbar.ProcessId));
        return applied ? TaskbarStateResult.Applied : TaskbarStateResult.Failed;
    }

    /// <summary>
    /// Collapses the AppBar reservation to zero thickness via ABM_SETPOS before calling ABM_REMOVE so
    /// Explorer immediately restores the work area and resizes maximized windows on exit or edge switch.
    /// The ClearingReservation phase makes a synchronous ABN_POSCHANGED defer instead of re-applying a strip.
    /// </summary>
    private void ZeroAndRemoveAppBar(Models.DockEdge? edgeOverride = null)
    {
        if (!IsRegisteredPhase) return;
        var edge = edgeOverride ?? _edge;
        var appBarEdge = edge switch
        {
            Models.DockEdge.Left => AbeLeft,
            Models.DockEdge.Right => AbeRight,
            _ => AbeBottom
        };

        SessionState previous;
        if (Phase is SessionState.Stopping or SessionState.Restoring)
        {
            previous = _state;
            SetPhase(SessionState.ClearingReservation);
        }
        else if (!TryEnterTransient(SessionState.ClearingReservation, out previous))
        {
            return;
        }

        try
        {
            var collapsed = AppBarClient.SetPosition(_handle, appBarEdge, ZeroThicknessRect(appBarEdge), null,
                "ZeroAndRemove/SetPos", out _);
            ShellTelemetry.Result("AppBarSetPos", "ZeroAndRemove", success: collapsed, verified: collapsed,
                ("edge", appBarEdge));
            var removeResult = AppBarClient.Unregister(_handle, "ZeroAndRemove");
            ShellTelemetry.Result("AppBarRemove", "ZeroAndRemove", success: removeResult != 0,
                verified: removeResult != 0);
        }
        finally
        {
            if (Phase == SessionState.ClearingReservation)
            {
                _state = (previous & SessionState.PhaseMask)
                    | (_state & SessionState.FlagsMask);
            }
        }

        MarkUnregistered();
        _secondaryShelfPixels = 0;
        _reservation = null;
        _sideReservation = null;
    }

    /// <summary>
    /// A zero-thickness rectangle that the shell accepts for the given edge. Side appbars must span the
    /// monitor's full height; a degenerate Rect(0,0,0,0) is rejected/clamped and leaves the work-area strip in place.
    /// </summary>
    private Rect ZeroThicknessRect(uint appBarEdge)
    {
        var monitor = _targetMonitor != 0 ? _targetMonitor : MonitorFromWindow(_handle, MonitorDefaultToNearest);
        var info = new MonitorInfo { cbSize = (uint)Marshal.SizeOf<MonitorInfo>() };
        if (!GetMonitorInfo(monitor, ref info)) return new Rect(0, 0, 0, 0);

        var bounds = info.rcMonitor;
        return appBarEdge switch
        {
            AbeLeft => new Rect(bounds.Left, bounds.Top, bounds.Left, bounds.Bottom),
            AbeRight => new Rect(bounds.Right, bounds.Top, bounds.Right, bounds.Bottom),
            _ => new Rect(bounds.Left, bounds.Bottom, bounds.Right, bounds.Bottom)
        };
    }

    private static string JournalPath => Path.Combine(
        Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData),
        "OpenTaskbar",
        JournalFileName);

    /// <summary>The journal path read by the native watchdog and by Rust after a hard kill.</summary>
    internal static string JournalFilePath => JournalPath;

    /// <summary>Rust uses <c>path.with_extension("txt.tmp")</c>; keep the exact same atomic-write target.</summary>
    internal static string JournalTemporaryFilePath => Path.ChangeExtension(JournalPath, JournalTemporaryExtension);

    /// <summary>Single decimal number, no trailing newline, no BOM, only the two supported AppBar bits.</summary>
    internal static string FormatJournal(uint state) =>
        (state & SupportedTaskbarState).ToString(CultureInfo.InvariantCulture);

    private static void WriteJournal(uint state)
    {
        var path = JournalPath;
        var directory = Path.GetDirectoryName(path);
        if (directory is null)
        {
            throw new IOException("ErrorTaskbarJournalPath");
        }

        Directory.CreateDirectory(directory);
        var temporaryPath = JournalTemporaryFilePath;
        File.WriteAllText(temporaryPath, FormatJournal(state), new UTF8Encoding(encoderShouldEmitUTF8Identifier: false));
        File.Move(temporaryPath, path, overwrite: true);
    }

    private static uint ReadJournal() => ParseJournal(File.ReadAllText(JournalPath));

    internal static uint ParseJournal(string text)
    {
        if (!uint.TryParse(text.Trim(), NumberStyles.None, CultureInfo.InvariantCulture, out var state)
            || (state & ~SupportedTaskbarState) != 0)
        {
            throw new InvalidDataException("ErrorTaskbarJournalInvalid");
        }

        return state;
    }

    /// <summary>A missing or damaged journal must restore a visible taskbar, never a guessed hidden state.</summary>
    internal static uint FallbackState(string? journalText)
    {
        if (journalText is null) return DefaultTaskbarState;
        try
        {
            return ParseJournal(journalText);
        }
        catch (InvalidDataException)
        {
            return DefaultTaskbarState;
        }
    }

    private static string? DeleteJournal()
    {
        try
        {
            if (File.Exists(JournalPath))
            {
                File.Delete(JournalPath);
            }

            return null;
        }
        catch (IOException)
        {
            return "ErrorTaskbarJournalDelete";
        }
        catch (UnauthorizedAccessException)
        {
            return "ErrorTaskbarJournalAccess";
        }
    }

    public void Dispose()
    {
        if (_disposed)
        {
            return;
        }

        // StopForRestart intentionally leaves the native taskbar hidden and the journal on disk so the
        // replacement session that starts immediately afterward can re-register its AppBar without flashing
        // Explorer's taskbar. Calling Stop() here would see the journal and restore/show the taskbar (#86).
        if (!StoppedForRestart)
        {
            var error = Stop();
            if (error is not null) AppLog.Error(error);
            if (IsActive)
            {
                // Stop() can leave the session active on failure; release hooks and timers so they do not
                // leak for the lifetime of the process, while keeping ProcessExit registered for final recovery.
                ReleaseHooksAndTimers();
                return;
            }
        }
        _disposed = true;
        ReleaseHooksAndTimers();
        _state = SessionState.Disposed;
        Interlocked.CompareExchange(ref _current, null, this);
        AppDomain.CurrentDomain.ProcessExit -= Process_Exit;
    }

    private void ReleaseHooksAndTimers()
    {
        UninstallShellShowHook();
        _fullScreenPollTimer?.Stop();
        if (_fullScreenPollTimer is not null)
        {
            _fullScreenPollTimer.Tick -= FullScreenPollTimer_Tick;
            _fullScreenPollTimer = null;
        }
        StopHideRetryTimer();
        if (_hideRetryTimer is not null)
        {
            _hideRetryTimer.Tick -= HideRetryTimer_Tick;
            _hideRetryTimer = null;
        }
        StopReconcileTimer();
        if (_reconcileTimer is not null)
        {
            _reconcileTimer.Tick -= ReconcileTimer_Tick;
            _reconcileTimer = null;
        }
    }

    private void Process_Exit(object? sender, EventArgs e)
    {
        var error = Stop();
        if (error is not null) AppLog.Error(error);
        // ProcessExit can race with a Stop() on another thread. A leftover journal means the taskbar may still be
        // hidden, so use the journal-based path which does not depend on instance state or _stopInProgress.
        if (File.Exists(JournalPath))
        {
            error = ForceRestoreNativeTaskbar();
            if (error is not null) AppLog.Error(error);
        }
    }

    [StructLayout(LayoutKind.Sequential)]
    internal struct Rect(int left, int top, int right, int bottom)
    {
        public int Left = left;
        public int Top = top;
        public int Right = right;
        public int Bottom = bottom;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct MonitorInfo
    {
        public uint cbSize;
        public Rect rcMonitor;
        public Rect rcWork;
        public uint dwFlags;
    }

    [DllImport("user32.dll")]
    private static extern nint MonitorFromWindow(nint windowHandle, uint flags);

    [StructLayout(LayoutKind.Sequential)]
    private readonly struct NativePoint(int x, int y)
    {
        public readonly int X = x;
        public readonly int Y = y;
    }

    [DllImport("user32.dll")]
    private static extern nint MonitorFromPoint(NativePoint point, uint flags);

    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool GetMonitorInfo(nint monitor, ref MonitorInfo monitorInfo);

    private delegate bool EnumWindowsProc(nint windowHandle, nint lParam);

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool EnumWindows(EnumWindowsProc callback, nint lParam);

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool IsWindowVisible(nint windowHandle);

    [DllImport("user32.dll")]
    private static extern uint GetDpiForWindow(nint windowHandle);

    [DllImport("user32.dll")]
    private static extern nint GetAncestor(nint hwnd, uint flags);

    [DllImport("user32.dll")]
    private static extern nint GetWindow(nint hwnd, int command);

    [DllImport("user32.dll")]
    private static extern nint GetForegroundWindow();

    [DllImport("user32.dll")]
    private static extern uint GetWindowThreadProcessId(nint windowHandle, out uint processId);

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool IsIconic(nint windowHandle);

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool IsZoomed(nint windowHandle);

    private static nint GetWindowLongPtr(nint windowHandle, int index) =>
        IntPtr.Size == 8 ? GetWindowLongPtr64(windowHandle, index) : GetWindowLong32(windowHandle, index);

    [DllImport("user32.dll", EntryPoint = "GetWindowLongPtrW")]
    private static extern nint GetWindowLongPtr64(nint windowHandle, int index);

    [DllImport("user32.dll", EntryPoint = "GetWindowLongW")]
    private static extern int GetWindowLong32(nint windowHandle, int index);

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool GetWindowRect(nint windowHandle, out Rect rect);

    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    private static extern int GetClassName(nint windowHandle, char[] name, int length);

    [DllImport("user32.dll", EntryPoint = "GetClassNameW", CharSet = CharSet.Unicode)]
    private static extern int GetClassNameRef(nint windowHandle, ref ushort name, int length);

    [DllImport("user32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool SetWindowPos(nint windowHandle, nint insertAfter, int x, int y, int width, int height, uint flags);

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool ShowWindow(nint windowHandle, int command);

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool ShowWindowAsync(nint windowHandle, int command);

    [DllImport("user32.dll")]
    private static extern nint SendMessageTimeout(nint windowHandle, uint message, nint wParam, nint lParam,
        uint flags, uint timeoutMs, out nint result);

    private delegate void WinEventDelegate(nint hook, uint eventType, nint windowHandle, int objectId,
        int childId, uint eventThread, uint eventTime);

    [DllImport("user32.dll")]
    private static extern nint SetWinEventHook(uint eventMin, uint eventMax, nint moduleHandle,
        WinEventDelegate callback, uint processId, uint threadId, uint flags);

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool UnhookWinEvent(nint hook);

    [DllImport("dwmapi.dll")]
    private static extern int DwmGetWindowAttribute(nint hwnd, int dwAttribute, out int pvAttribute, int cbAttribute);
}
