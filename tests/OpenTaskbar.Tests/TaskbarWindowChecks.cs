using OpenTaskbar.Services;

internal static class TaskbarWindowChecks
{
    private const long AppWindow = TaskbarWindowRules.AppWindow;
    private const long ToolWindow = TaskbarWindowRules.ToolWindow;
    private const long NoActivate = TaskbarWindowRules.NoActivate;

    public static int Run()
    {
        var checks = 0;
        void Check(bool condition, string message)
        {
            if (!condition) throw new InvalidOperationException(message);
            checks++;
        }

        Check(TaskbarWindowRules.HasTaskbarButton(0, false, false),
            "A normal visible window must keep its taskbar button.");
        Check(!TaskbarWindowRules.HasTaskbarButton(ToolWindow, false, false),
            "Tool windows are background helpers and must not appear as normal app buttons.");
        Check(TaskbarWindowRules.HasTaskbarButton(ToolWindow | AppWindow, false, false),
            "A tool window that explicitly opts in with WS_EX_APPWINDOW must still appear.");
        Check(!TaskbarWindowRules.HasTaskbarButton(NoActivate, false, false),
            "Non-activatable overlay and helper windows must not appear as normal app buttons.");
        Check(!TaskbarWindowRules.HasTaskbarButton(ToolWindow | NoActivate, false, false),
            "Helper windows need the explicit opt-in even when they carry both helper styles.");
        Check(TaskbarWindowRules.HasTaskbarButton(NoActivate | AppWindow, false, false),
            "A non-activatable window with WS_EX_APPWINDOW must still appear.");
        Check(!TaskbarWindowRules.HasTaskbarButton(0, true, false),
            "An owned window has no taskbar button unless it opts in.");
        Check(TaskbarWindowRules.HasTaskbarButton(AppWindow, true, false),
            "An owned window with WS_EX_APPWINDOW follows the shell rule and must appear.");
        Check(!TaskbarWindowRules.HasTaskbarButton(0, false, true),
            "A window removed with ITaskbarList::DeleteTab must stay hidden even when otherwise eligible.");
        Check(!TaskbarWindowRules.HasTaskbarButton(AppWindow, true, true),
            "ITaskbarList::DeleteTab must override both WS_EX_APPWINDOW and the owner opt-in.");

        Check(TaskbarWindowRules.IsHiddenCloak(1) && TaskbarWindowRules.IsHiddenCloak(4),
            "App- and inherited-cloaked windows are hidden from the taskbar.");
        Check(!TaskbarWindowRules.IsHiddenCloak(0) && !TaskbarWindowRules.IsHiddenCloak(TaskbarWindowRules.CloakedShell),
            "Visible windows and windows cloaked by the shell for another virtual desktop are kept.");

        Check(TaskbarWindowRules.IsHiddenImmersiveFrame("Windows.UI.Core.CoreWindow", 0),
            "A core window without WS_EX_WINDOWEDGE is an internal shell surface.");
        Check(TaskbarWindowRules.IsHiddenImmersiveFrame("ApplicationFrameWindow", 0),
            "A hidden UWP frame is not an app button.");
        Check(!TaskbarWindowRules.IsHiddenImmersiveFrame("ApplicationFrameWindow", TaskbarWindowRules.WindowEdge),
            "A real Store app frame carries WS_EX_WINDOWEDGE and must be tracked.");
        Check(!TaskbarWindowRules.IsHiddenImmersiveFrame("MozillaWindowClass", 0),
            "Normal application classes must never be treated as immersive frames.");

        return checks;
    }
}
