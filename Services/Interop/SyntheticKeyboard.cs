using System.Runtime.InteropServices;

namespace OpenTaskbar.Services;

/// <summary>
/// Sends keyboard chords to the shell with guaranteed modifier release.
///
/// A previous implementation pushed all keydowns and keyups into one SendInput batch and only logged a partial
/// result. If a secure-desktop transition (Win+L) or focus change happened while the batch was being processed,
/// the Win keyup could be lost, leaving Win logically down. The user's next ordinary keystroke was then treated
/// by Windows as Win+I (Settings), Win+E (Explorer), etc. — with no Settings code in our own process involved.
///
/// Here every modifier we press is tracked, released in a finally block, and released again on session unlock
/// if the OS still reports it as down.
/// </summary>
internal static class SyntheticKeyboard
{
    private const ushort VkShift = 0x10;
    private const ushort VkAlt = 0x12;
    private const ushort VkControl = 0x11;
    private const ushort VkRightWindows = 0x5C;
    private const ushort VkLeftWindows = 0x5B;
    private const uint InputKeyboard = 1;
    private const uint KeyEventKeyUp = 0x0002;
    private const uint KeyEventExtended = 0x0001;
    private const int ChordHoldMs = 20;

    // Released last first, so Win is the final modifier to go up.
    private static readonly ushort[] Modifiers = [VkShift, VkAlt, VkControl, VkRightWindows, VkLeftWindows];
    private static readonly object Gate = new();
    private static readonly HashSet<ushort> HeldModifiers = [];

    internal static IReadOnlyCollection<ushort> TrackedModifiers
    {
        get { lock (Gate) return HeldModifiers.ToArray(); }
    }

    internal static string? SendChord(params ushort[] keys)
    {
        if (keys.Length == 0) return null;

        var owned = keys.Where(IsModifier).Distinct().ToArray();
        string? error = null;
        try
        {
            foreach (var key in keys)
            {
                if (!SendKey(key, up: false))
                {
                    error = Failure(key, down: true);
                    break;
                }

                if (IsModifier(key))
                {
                    lock (Gate) HeldModifiers.Add(key);
                }
            }

            if (error is null)
            {
                // Give the shell a moment to observe the chord before the first release.
                Thread.Sleep(ChordHoldMs);
                for (var i = keys.Length - 1; i >= 0; i--)
                {
                    if (!SendKey(keys[i], up: true))
                    {
                        error = Failure(keys[i], down: false);
                        break;
                    }

                    if (IsModifier(keys[i]))
                    {
                        lock (Gate) HeldModifiers.Remove(keys[i]);
                    }
                }
            }
        }
        finally
        {
            // Even a partially sent chord must not leave a modifier held down.
            foreach (var key in owned.Reverse())
            {
                bool release;
                lock (Gate) release = HeldModifiers.Remove(key);
                if (release) SendKey(key, up: true);
            }
        }

        return error;
    }

    /// <summary>Called when the interactive desktop returns, so a modifier lost during a lock transition cannot leak into the next keystroke.</summary>
    internal static void ReleaseAll()
    {
        var keys = new HashSet<ushort>();
        lock (Gate)
        {
            keys.UnionWith(HeldModifiers);
            HeldModifiers.Clear();
        }

        foreach (var key in Modifiers)
        {
            if (GetAsyncKeyState(key) < 0) keys.Add(key);
        }

        foreach (var key in Modifiers.Reverse())
        {
            if (keys.Contains(key)) SendKey(key, up: true);
        }
    }

    private static string Failure(ushort key, bool down)
    {
        AppLog.Warning($"Synthetic keyboard: {key:X2} {(down ? "down" : "up")} failed: {Marshal.GetLastPInvokeError()}");
        return UiText.Get("ErrorShortcutSend");
    }

    private static bool IsModifier(ushort key) => key is VkShift or VkAlt or VkControl or VkRightWindows or VkLeftWindows;

    private static bool IsExtended(ushort key) => key is VkLeftWindows or VkRightWindows;

    private static bool SendKey(ushort key, bool up)
    {
        var input = new Input
        {
            Type = InputKeyboard,
            Keyboard = new KeyboardInput
            {
                VirtualKey = key,
                Flags = (up ? KeyEventKeyUp : 0) | (IsExtended(key) ? KeyEventExtended : 0)
            }
        };
        return SendInput(1, [input], Marshal.SizeOf<Input>()) == 1;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct Input
    {
        public uint Type;
        public KeyboardInput Keyboard;
        // Pads the union to the size of MOUSEINPUT, the largest member.
        private readonly long _padding;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct KeyboardInput
    {
        public ushort VirtualKey;
        public ushort ScanCode;
        public uint Flags;
        public uint Time;
        public nint ExtraInfo;
    }

    [DllImport("user32.dll", SetLastError = true)]
    private static extern uint SendInput(uint count, Input[] inputs, int size);

    [DllImport("user32.dll")]
    private static extern short GetAsyncKeyState(int virtualKey);
}
