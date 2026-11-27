using System.Runtime.InteropServices;

namespace OpenTaskbar.Services;

/// <summary>Cheap user-idle reading for the ambient display mode. No timers, no background work.</summary>
internal static class UserIdleMonitor
{
    public static TimeSpan IdleTime()
    {
        var info = new LastInputInfo { cbSize = (uint)Marshal.SizeOf<LastInputInfo>() };
        if (!GetLastInputInfo(ref info)) return TimeSpan.Zero;
        // Both values are 32-bit millisecond tick counts and wrap about every 49 days.
        var elapsed = unchecked((uint)Environment.TickCount - info.dwTime);
        return TimeSpan.FromMilliseconds(elapsed);
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct LastInputInfo
    {
        public uint cbSize;
        public uint dwTime;
    }

    [DllImport("user32.dll", SetLastError = true)]
    private static extern bool GetLastInputInfo(ref LastInputInfo lastInputInfo);
}
