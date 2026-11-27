using System.Windows;
using System.Windows.Interop;
using OpenTaskbar.Models;
using OpenTaskbar.Services;

namespace OpenTaskbar;

public partial class MainWindow
{
    private AppWindowPicker? _windowPicker;
    private DockApp? _windowPickerApp;
    private nint _windowPickerMonitor;

    internal bool TryShowWindowPicker(DockApp app, FrameworkElement anchor)
    {
        if (!_preferences.Current.PreviewsEnabled || PresentationSource.FromVisual(anchor) is not HwndSource source)
            return false;
        var monitor = DisplayService.GetWindowMonitor(source.Handle);
        var display = DisplayService.GetDisplay(monitor);
        if (display is null) return false;
        var windows = GetPickerWindows(app, monitor);
        if (windows.Count < 2) return false;

        _previewHoverTimer.Stop();
        _pendingPreviewButton = null;
        _previewCloseTimer.Stop();
        _thumbnailPreview.Hide();
        _windowPicker?.Close();
        _windowPickerApp = app;
        _windowPickerMonitor = monitor;
        var picker = new AppWindowPicker(app.Name, app.Icon, display, windows, handle =>
        {
            if (!NativeWindowTracker.Activate(handle)) ReportError("ErrorPreviewActivate");
        });
        _windowPicker = picker;
        picker.Closed += (_, _) =>
        {
            if (!ReferenceEquals(_windowPicker, picker)) return;
            _windowPicker = null;
            _windowPickerApp = null;
            if (!_closed) _autoHideController?.EvaluateState();
        };
        picker.Show();
        return true;
    }

    private IReadOnlyList<TrackedWindow> GetPickerWindows(DockApp app, nint monitor) =>
        AppWindowPicker.OrderWindows((_mirrorOf ?? this).VisibleSnapshot, app, monitor, DisplayService.GetWindowMonitor);

    private void RefreshWindowPicker()
    {
        if (_windowPicker is { } picker && _windowPickerApp is { } app)
            picker.Refresh(GetPickerWindows(app, _windowPickerMonitor));
    }
}
