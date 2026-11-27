using System.Diagnostics;
using System.Runtime.InteropServices;
using System.Windows.Threading;

namespace OpenTaskbar.Services;

/// <summary>
/// Pure state machine that distinguishes a standalone Windows-key tap (press + release with no other key
/// pressed in between) from Win-key shortcuts such as Win+D, Win+E, or Win+Shift+S (#164).
/// </summary>
internal sealed class WinKeySequenceTracker
{
    public const int VkLeftWin = 0x5B;
    public const int VkRightWin = 0x5C;

    private bool _winDown;
    private bool _cancelledByOtherKey;

    public static bool IsWinKey(int vkCode) => vkCode is VkLeftWin or VkRightWin;

    public void OnKeyDown(int vkCode)
    {
        if (IsWinKey(vkCode))
        {
            if (!_winDown)
            {
                _winDown = true;
                _cancelledByOtherKey = false;
            }
            return;
        }

        if (_winDown)
        {
            _cancelledByOtherKey = true;
        }
    }

    /// <summary>Returns true when a standalone Win key was released without any intervening key.</summary>
    public bool OnKeyUp(int vkCode)
    {
        if (!IsWinKey(vkCode)) return false;

        var trigger = _winDown && !_cancelledByOtherKey;
        _winDown = false;
        _cancelledByOtherKey = false;
        return trigger;
    }

    public void Reset()
    {
        _winDown = false;
        _cancelledByOtherKey = false;
    }
}

/// <summary>
/// Watches for standalone Windows-key presses via a passive <c>WH_KEYBOARD_LL</c> hook so the auto-hidden
/// dock can reveal alongside the Start menu without intercepting or blocking any keyboard input (#164).
/// </summary>
internal sealed class WinKeyRevealMonitor : IDisposable
{
    private const int WhKeyboardLl = 13;
    private const int WmKeyDown = 0x0100;
    private const int WmKeyUp = 0x0101;
    private const int WmSysKeyDown = 0x0104;
    private const int WmSysKeyUp = 0x0105;
    private const uint LlkhfInjected = 0x00000010;

    private readonly Dispatcher _dispatcher;
    private readonly WinKeySequenceTracker _tracker = new();
    private readonly LowLevelKeyboardProc _hookProc;
    private nint _hook;
    private bool _disposed;

    public event Action? StandaloneWinKeyPressed;

    public bool IsRunning => _hook != 0;

    public WinKeyRevealMonitor(Dispatcher dispatcher)
    {
        _dispatcher = dispatcher;
        _hookProc = HookCallback;
    }

    public void SetEnabled(bool enabled)
    {
        if (_disposed) return;
        if (enabled) Start();
        else Stop();
    }

    public void ResetState() => _tracker.Reset();

    private void Start()
    {
        if (_hook != 0) return;
        _tracker.Reset();
        var moduleHandle = GetModuleHandle(null);
        _hook = SetWindowsHookEx(WhKeyboardLl, _hookProc, moduleHandle, 0);
        if (_hook == 0)
        {
            AppLog.Warning($"WinKeyRevealMonitor hook install failed: {Marshal.GetLastPInvokeError()}");
        }
    }

    private void Stop()
    {
        _tracker.Reset();
        if (_hook == 0) return;
        UnhookWindowsHookEx(_hook);
        _hook = 0;
    }

    // Invoked on the OS low-level keyboard hook path: never throw across unmanaged boundaries and always
    // forward to CallNextHookEx so Windows shortcuts and normal typing remain unaffected.
    private nint HookCallback(int nCode, nint wParam, nint lParam)
    {
        try
        {
            if (nCode >= 0 && lParam != 0)
            {
                var info = Marshal.PtrToStructure<KbdLlHookStruct>(lParam);
                // Ignore synthesized keystrokes (e.g. from ShellShortcuts) to avoid self-triggering loops.
                if ((info.Flags & LlkhfInjected) == 0)
                {
                    var message = wParam.ToInt32();
                    if (message is WmKeyDown or WmSysKeyDown)
                    {
                        _tracker.OnKeyDown((int)info.VkCode);
                    }
                    else if (message is WmKeyUp or WmSysKeyUp)
                    {
                        if (_tracker.OnKeyUp((int)info.VkCode))
                        {
                            _dispatcher.BeginInvoke(DispatcherPriority.Send, () => StandaloneWinKeyPressed?.Invoke());
                        }
                    }
                }
            }
        }
        catch (Exception ex)
        {
            AppLog.Warning($"WinKeyRevealMonitor callback error: {ex.Message}");
        }

        return CallNextHookEx(_hook, nCode, wParam, lParam);
    }

    public void Dispose()
    {
        if (_disposed) return;
        _disposed = true;
        Stop();
    }

    private delegate nint LowLevelKeyboardProc(int nCode, nint wParam, nint lParam);

    [StructLayout(LayoutKind.Sequential)]
    private struct KbdLlHookStruct
    {
        public uint VkCode;
        public uint ScanCode;
        public uint Flags;
        public uint Time;
        public nuint ExtraInfo;
    }

    [DllImport("user32.dll", SetLastError = true)]
    private static extern nint SetWindowsHookEx(int idHook, LowLevelKeyboardProc lpfn, nint hMod, uint dwThreadId);

    [DllImport("user32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool UnhookWindowsHookEx(nint hhk);

    [DllImport("user32.dll")]
    private static extern nint CallNextHookEx(nint hhk, int nCode, nint wParam, nint lParam);

    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    private static extern nint GetModuleHandle(string? moduleName);
}
