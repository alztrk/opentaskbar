using System.ComponentModel;
using System.IO;
using System.Runtime.InteropServices;
using System.Windows;
using System.Windows.Controls.Primitives;
using System.Windows.Input;
using System.Windows.Media;
using System.Windows.Media.Animation;
using System.Windows.Media.Imaging;
using System.Windows.Threading;
using OpenTaskbar.Models;
using OpenTaskbar.Services;
using Windows.Foundation;
using Windows.Media.Control;
using MediaSession = Windows.Media.Control.GlobalSystemMediaTransportControlsSession;
using MediaSessionManager = Windows.Media.Control.GlobalSystemMediaTransportControlsSessionManager;

namespace OpenTaskbar.Widgets;

/// <summary>
/// V19 horizontal taskbar media player: Thumbnail -> Song Info -> Progress -> Prev / Play-Pause / Next -> Volume hover popover.
/// Backed by Windows GSMTC and Core Audio.
/// </summary>
public partial class MediaWidget : DockWidget
{
    private const int AlbumArtDecodePixels = 68;
    private static readonly Color DefaultArtTint = Color.FromRgb(0x52, 0x6D, 0x84);

    private MediaSessionManager? _manager;
    private MediaSession? _session;
    private AudioEndpointMonitor.SharedHandle? _audio;
    private readonly DispatcherTimer _volumeHoverCloseTimer;
    private int _activationVersion;
    private int _refreshVersion;
    private TimeSpan _startTime;
    private TimeSpan _endTime;
    private TimeSpan _position;
    private DateTimeOffset _positionUpdatedAt;
    private bool _playing;
    private bool _canSeek;
    private bool _updatingProgress;
    private bool _draggingProgress;
    private bool _updatingVolume;
    private bool? _lastPlayGlyph;

    private bool? _lastAudioHasDevice;
    private int _lastAudioLevel = -1;
    private bool _lastAudioMuted;
    private string? _lastAudioDeviceName;
    private string? _lastAudioIconKey;
    private AppLanguage _lastAudioLanguage;

    public override bool SupportsCompactMode => true;
    public override int ShrinkPriority => 10;
    public override double FullWidthEstimate => 420;
    public override double CompactWidthEstimate => 164;

    public MediaWidget()
    {
        InitializeComponent();
        VolumePopup.CustomPopupPlacementCallback = PlaceVolumePopover;
        VolumePopup.Opened += VolumePopup_Opened;
        _volumeHoverCloseTimer = new DispatcherTimer(DispatcherPriority.Background, Dispatcher)
        {
            Interval = TimeSpan.FromMilliseconds(160)
        };
        _volumeHoverCloseTimer.Tick += VolumeHoverCloseTimer_Tick;
        HasData = false;
    }

    protected override void OnCompactChanged(bool compact) => ApplyDensityLayout();
    protected override void OnHoverExpandedChanged(bool expanded) => ApplyDensityLayout();

    private void ApplyDensityLayout()
    {
        var compact = IsCompact && !IsHoverExpanded;
        Card.Padding = compact ? new Thickness(6, 0, 6, 0) : new Thickness(9, 0, 9, 0);
        InfoPanel.Width = compact ? 76 : 96;
        ArtistText.Visibility = compact ? Visibility.Collapsed : Visibility.Visible;
        ProgressPanel.Visibility = compact ? Visibility.Collapsed : Visibility.Visible;
        PreviousButton.Visibility = compact ? Visibility.Collapsed : Visibility.Visible;
        NextButton.Visibility = compact ? Visibility.Collapsed : Visibility.Visible;
        VolumePanel.Visibility = compact ? Visibility.Collapsed : Visibility.Visible;
        if (compact && VolumePopup.IsOpen) CloseVolumePopover();
    }

    // V19: Media widget uses its own compact hover volume popover and does not open a large generic flyout.
    public override UIElement? BuildHoverFlyoutContent() => null;

    protected override async void OnActivated()
    {
        var activationVersion = ++_activationVersion;
        UiText.Current.PropertyChanged += UiText_PropertyChanged;

        try
        {
            _audio = AudioEndpointMonitor.AcquireShared();
            _audio.VolumeChanged += Audio_VolumeChanged;
            _audio.DefaultDeviceChanged += Audio_DefaultDeviceChanged;
        }
        catch (COMException ex)
        {
            AppLog.Warning($"Media widget audio unavailable: {ex.Message}");
            _audio = null;
        }

        InvalidateAudioCache();
        RenderVolume();

        try
        {
            var manager = await MediaSessionManager.RequestAsync();
            if (!IsActive || activationVersion != _activationVersion) return;
            _manager = manager;
            _manager.CurrentSessionChanged += Manager_CurrentSessionChanged;
            _manager.SessionsChanged += Manager_SessionsChanged;
            BindSession(GetActiveSession());
        }
        catch (Exception ex) when (ex is COMException or UnauthorizedAccessException)
        {
            if (activationVersion == _activationVersion)
            {
                AppLog.Warning($"Media session manager unavailable: {ex.Message}");
                HasData = false;
            }
        }
    }

    protected override void OnDeactivated()
    {
        _activationVersion++;
        CloseVolumePopover();
        UiText.Current.PropertyChanged -= UiText_PropertyChanged;
        BindSession(null);
        ClearAlbumArt();
        if (_manager is not null)
        {
            _manager.CurrentSessionChanged -= Manager_CurrentSessionChanged;
            _manager.SessionsChanged -= Manager_SessionsChanged;
            _manager = null;
        }

        if (_audio is not null)
        {
            _audio.VolumeChanged -= Audio_VolumeChanged;
            _audio.DefaultDeviceChanged -= Audio_DefaultDeviceChanged;
            _audio.Dispose();
            _audio = null;
        }
    }

    protected override void OnDisposed()
    {
        _volumeHoverCloseTimer.Stop();
        _volumeHoverCloseTimer.Tick -= VolumeHoverCloseTimer_Tick;
        VolumePopup.Opened -= VolumePopup_Opened;
        CloseVolumePopover();
    }

    protected override void OnTick(DateTime now)
    {
        if (!IsActive || _manager is null) return;

        var candidate = GetActiveSession();
        if (ShouldSwitchSession(candidate))
        {
            BindSession(candidate);
            return;
        }

        if (_session is null || _draggingProgress) return;

        RefreshTimeline();

        try
        {
            var pb = _session.GetPlaybackInfo();
            if (pb is not null)
            {
                var isPlaying = pb.PlaybackStatus == GlobalSystemMediaTransportControlsSessionPlaybackStatus.Playing;
                if (_playing != isPlaying)
                {
                    _playing = isPlaying;
                    if (_lastPlayGlyph != _playing)
                    {
                        _lastPlayGlyph = _playing;
                        PlayPauseGlyph.Content = FindResource(_playing ? "IconPauseFill" : "IconPlayFill");
                    }
                }
                var controls = pb.Controls;
                if (controls is not null)
                {
                    _canSeek = controls.IsPlaybackPositionEnabled;
                    ProgressSlider.IsHitTestVisible = _canSeek;
                    ProgressSlider.Focusable = _canSeek;
                }
            }
        }
        catch
        {
            // Transient COM failure during app transition.
        }

        if (_playing)
        {
            UpdateProgressUI();
        }
    }

    private void UiText_PropertyChanged(object? sender, PropertyChangedEventArgs e) => RenderVolume();

    private void InvalidateAudioCache()
    {
        _lastAudioHasDevice = null;
        _lastAudioLevel = -1;
        _lastAudioIconKey = null;
        _lastAudioDeviceName = null;
    }

    private void ClearAlbumArt()
    {
        ArtImage.Source = null;
        ArtPlaceholder.Visibility = Visibility.Visible;
        AnimateArtTint(DefaultArtTint);
    }

    private void AnimateArtTint(Color targetColor)
    {
        if (!Settings.AnimationsEnabled || !SystemParameters.ClientAreaAnimation)
        {
            ArtTintBrush.BeginAnimation(SolidColorBrush.ColorProperty, null);
            ArtTintBrush.Color = targetColor;
            return;
        }

        var anim = new ColorAnimation(targetColor, TimeSpan.FromMilliseconds(280))
        {
            EasingFunction = new CubicEase { EasingMode = EasingMode.EaseOut }
        };
        ArtTintBrush.BeginAnimation(SolidColorBrush.ColorProperty, anim);
    }

    private static Color DeriveSubtleTintColor(BitmapSource bitmap)
    {
        try
        {
            var bgra = new FormatConvertedBitmap(bitmap, PixelFormats.Bgra32, null, 0);
            var w = bgra.PixelWidth;
            var h = bgra.PixelHeight;
            if (w <= 0 || h <= 0) return DefaultArtTint;
            var stride = w * 4;
            var pixels = new byte[stride * h];
            bgra.CopyPixels(pixels, stride, 0);

            long sumR = 0, sumG = 0, sumB = 0, count = 0;
            var stepY = Math.Max(1, h / 16);
            var stepX = Math.Max(1, w / 16);
            for (var y = 0; y < h; y += stepY)
            {
                var row = y * stride;
                for (var x = 0; x < w; x += stepX)
                {
                    var idx = row + x * 4;
                    if (pixels[idx + 3] < 128) continue;
                    sumB += pixels[idx];
                    sumG += pixels[idx + 1];
                    sumR += pixels[idx + 2];
                    count++;
                }
            }
            if (count == 0) return DefaultArtTint;

            // Clamp luminance and softly mute saturation so the thumbnail tint stays refined and readable.
            var r = (byte)Math.Clamp(sumR / count * 0.72 + 24, 34, 155);
            var g = (byte)Math.Clamp(sumG / count * 0.72 + 24, 34, 155);
            var b = (byte)Math.Clamp(sumB / count * 0.72 + 24, 34, 155);
            return Color.FromRgb(r, g, b);
        }
        catch
        {
            return DefaultArtTint;
        }
    }

    private static CustomPopupPlacement[] PlaceVolumePopover(System.Windows.Size popupSize, System.Windows.Size targetSize, System.Windows.Point offset)
    {
        return DockFlyout.Edge switch
        {
            DockEdge.Left =>
            [
                new CustomPopupPlacement(new System.Windows.Point(targetSize.Width + 4, (targetSize.Height - popupSize.Height) / 2), PopupPrimaryAxis.Vertical)
            ],
            DockEdge.Right =>
            [
                new CustomPopupPlacement(new System.Windows.Point(-popupSize.Width - 4, (targetSize.Height - popupSize.Height) / 2), PopupPrimaryAxis.Vertical)
            ],
            _ =>
            [
                new CustomPopupPlacement(new System.Windows.Point((targetSize.Width - popupSize.Width) / 2, -popupSize.Height - 4), PopupPrimaryAxis.Horizontal),
                new CustomPopupPlacement(new System.Windows.Point((targetSize.Width - popupSize.Width) / 2, targetSize.Height + 4), PopupPrimaryAxis.Horizontal)
            ]
        };
    }

    private void VolumePopup_Opened(object? sender, EventArgs e)
    {
        if (!Settings.AnimationsEnabled || !SystemParameters.ClientAreaAnimation)
        {
            VolumePopoverSurface.Opacity = 1;
            VolumePopoverTranslate.Y = 0;
            VolumePopoverScale.ScaleX = 1;
            VolumePopoverScale.ScaleY = 1;
            return;
        }

        var duration = TimeSpan.FromMilliseconds(160);
        var ease = new CubicEase { EasingMode = EasingMode.EaseOut };
        VolumePopoverTranslate.BeginAnimation(TranslateTransform.YProperty, new DoubleAnimation(6, 0, duration) { EasingFunction = ease });
        VolumePopoverScale.BeginAnimation(ScaleTransform.ScaleXProperty, new DoubleAnimation(0.96, 1, duration) { EasingFunction = ease });
        VolumePopoverScale.BeginAnimation(ScaleTransform.ScaleYProperty, new DoubleAnimation(0.96, 1, duration) { EasingFunction = ease });
        VolumePopoverSurface.BeginAnimation(OpacityProperty, new DoubleAnimation(0, 1, duration) { EasingFunction = ease });
    }

    private void VolumeTrigger_MouseEnter(object sender, MouseEventArgs e)
    {
        if (_audio?.HasDevice != true) return;
        _volumeHoverCloseTimer.Stop();
        if (!VolumePopup.IsOpen) VolumePopup.IsOpen = true;
    }

    private void VolumeTrigger_MouseLeave(object sender, MouseEventArgs e)
    {
        if (VolumePopup.IsOpen && !_volumeHoverCloseTimer.IsEnabled)
            _volumeHoverCloseTimer.Start();
    }

    private void VolumePopover_MouseEnter(object sender, MouseEventArgs e)
    {
        _volumeHoverCloseTimer.Stop();
    }

    private void VolumePopover_MouseLeave(object sender, MouseEventArgs e)
    {
        if (VolumePopup.IsOpen && !_volumeHoverCloseTimer.IsEnabled)
            _volumeHoverCloseTimer.Start();
    }

    private void VolumeHoverCloseTimer_Tick(object? sender, EventArgs e)
    {
        if (!VolumePopup.IsOpen)
        {
            _volumeHoverCloseTimer.Stop();
            return;
        }

        // Keep open while dragging the slider thumb or while the cursor is anywhere in the trigger-to-popover bridge.
        if (IsSliderCaptured() || IsCursorInVolumeHoverBridge())
        {
            return;
        }

        CloseVolumePopover();
    }

    private void CloseVolumePopover()
    {
        _volumeHoverCloseTimer.Stop();
        if (VolumePopup.IsOpen) VolumePopup.IsOpen = false;
    }

    private bool IsSliderCaptured()
    {
        if (Mouse.Captured is not DependencyObject captured) return false;
        var current = captured;
        while (current is not null)
        {
            if (ReferenceEquals(current, VolumePopoverSurface) || ReferenceEquals(current, VolumeSlider))
                return true;
            current = VisualTreeHelper.GetParent(current) ?? LogicalTreeHelper.GetParent(current);
        }
        return false;
    }

    private bool IsCursorInVolumeHoverBridge()
    {
        if (VolumePanel.IsMouseOver || VolumePopoverSurface.IsMouseOver) return true;
        if (!GetCursorPos(out var pt)) return false;

        if (TryGetScreenRect(VolumePanel, 8, out var trigger)
            && TryGetScreenRect(VolumePopoverSurface, 10, out var popover))
        {
            if (Contains(trigger, pt) || Contains(popover, pt)) return true;

            // Continuous hover-safe corridor between the trigger icon and the popover above it.
            var bridgeLeft = Math.Min(trigger.Left, popover.Left);
            var bridgeRight = Math.Max(trigger.Right, popover.Right);
            var bridgeTop = Math.Min(trigger.Top, popover.Top);
            var bridgeBottom = Math.Max(trigger.Bottom, popover.Bottom);
            return pt.X >= bridgeLeft && pt.X <= bridgeRight && pt.Y >= bridgeTop && pt.Y <= bridgeBottom;
        }

        return false;
    }

    private static bool Contains((double Left, double Top, double Right, double Bottom) r, NativePoint pt) =>
        pt.X >= r.Left && pt.X <= r.Right && pt.Y >= r.Top && pt.Y <= r.Bottom;

    private static bool TryGetScreenRect(FrameworkElement element, double pad, out (double Left, double Top, double Right, double Bottom) rect)
    {
        rect = default;
        var source = PresentationSource.FromVisual(element);
        if (source?.CompositionTarget is null || element.ActualWidth <= 0 || element.ActualHeight <= 0) return false;
        try
        {
            var dx = source.CompositionTarget.TransformToDevice.M11 * pad;
            var dy = source.CompositionTarget.TransformToDevice.M22 * pad;
            var tl = element.PointToScreen(new System.Windows.Point(0, 0));
            var br = element.PointToScreen(new System.Windows.Point(element.ActualWidth, element.ActualHeight));
            rect = (tl.X - dx, tl.Y - dy, br.X + dx, br.Y + dy);
            return true;
        }
        catch (InvalidOperationException)
        {
            return false;
        }
    }

    private void Audio_VolumeChanged() => Dispatcher.BeginInvoke(DispatcherPriority.Background, () =>
    {
        if (IsActive) RenderVolume();
    });

    private void Audio_DefaultDeviceChanged() => Dispatcher.BeginInvoke(DispatcherPriority.Background, () =>
    {
        if (!IsActive || _audio is null) return;
        _audio.BindDefaultDevice();
        RenderVolume();
    });

    private void RenderVolume()
    {
        var hasDevice = _audio?.HasDevice == true;
        var lang = UiText.Current.Language;
        if (!hasDevice)
        {
            if (_lastAudioHasDevice == false && _lastAudioLanguage == lang) return;
            _lastAudioHasDevice = false;
            _lastAudioLanguage = lang;
            _lastAudioLevel = -1;
            MuteButton.IsEnabled = false;
            VolumeSlider.IsEnabled = false;
            VolumePercentText.Text = "—";
            SetVolumeIcon("IconVolumeMute");
            return;
        }

        var level = (int)Math.Round(_audio!.Volume * 100);
        var muted = _audio.Muted;
        var deviceName = _audio.DeviceName;
        if (_lastAudioHasDevice == true && _lastAudioLevel == level && _lastAudioMuted == muted
            && _lastAudioDeviceName == deviceName && _lastAudioLanguage == lang)
        {
            return;
        }

        _lastAudioHasDevice = true;
        _lastAudioLevel = level;
        _lastAudioMuted = muted;
        _lastAudioDeviceName = deviceName;
        _lastAudioLanguage = lang;

        MuteButton.IsEnabled = true;
        VolumeSlider.IsEnabled = true;
        SetVolumeIcon(muted || level == 0 ? "IconVolumeMute" : level < 50 ? "IconVolumeLow" : "IconVolume");
        var displayed = muted ? 0 : level;
        VolumePercentText.Text = $"{displayed}%";
        MuteButton.ToolTip = muted ? UiText.Get("MuteToggle") : $"{deviceName ?? UiText.Get("Volume")}: {UiText.Format("Percent", level)}";
        _updatingVolume = true;
        VolumeSlider.Value = displayed;
        _updatingVolume = false;
    }

    private void SetVolumeIcon(string key)
    {
        if (_lastAudioIconKey == key) return;
        _lastAudioIconKey = key;
        VolumeIcon.Content = FindResource(key);
    }

    private void Manager_CurrentSessionChanged(MediaSessionManager sender, CurrentSessionChangedEventArgs args) =>
        Dispatcher.BeginInvoke(DispatcherPriority.Background, () =>
        {
            if (!IsActive || _manager is null) return;
            var candidate = GetActiveSession();
            if (ShouldSwitchSession(candidate)) BindSession(candidate);
        });

    private void Manager_SessionsChanged(MediaSessionManager sender, SessionsChangedEventArgs args) =>
        Dispatcher.BeginInvoke(DispatcherPriority.Background, () =>
        {
            if (!IsActive || _manager is null) return;
            var candidate = GetActiveSession();
            if (ShouldSwitchSession(candidate)) BindSession(candidate);
        });

    private static bool IsSameSession(MediaSession? a, MediaSession? b)
    {
        if (ReferenceEquals(a, b)) return true;
        if (a is null || b is null) return false;
        return string.Equals(a.SourceAppUserModelId, b.SourceAppUserModelId, StringComparison.OrdinalIgnoreCase);
    }

    private bool ShouldSwitchSession(MediaSession? candidate)
    {
        if (candidate is null) return _session is not null;
        if (_session is null) return true;
        if (IsSameSession(candidate, _session)) return false;

        if (!_playing)
        {
            try
            {
                var pb = candidate.GetPlaybackInfo();
                if (pb?.PlaybackStatus == GlobalSystemMediaTransportControlsSessionPlaybackStatus.Playing)
                    return true;
            }
            catch { }
        }

        return false;
    }

    private MediaSession? GetActiveSession()
    {
        if (_manager is null) return null;

        MediaSession? current = null;
        try { current = _manager.GetCurrentSession(); } catch { }

        if (current is not null)
        {
            try
            {
                var pb = current.GetPlaybackInfo();
                if (pb?.PlaybackStatus == GlobalSystemMediaTransportControlsSessionPlaybackStatus.Playing)
                    return current;
            }
            catch { }
        }

        try
        {
            var sessions = _manager.GetSessions();
            if (sessions is { Count: > 0 })
            {
                foreach (var s in sessions)
                {
                    try
                    {
                        var pb = s.GetPlaybackInfo();
                        if (pb?.PlaybackStatus == GlobalSystemMediaTransportControlsSessionPlaybackStatus.Playing)
                            return s;
                    }
                    catch { }
                }

                if (current is not null) return current;

                foreach (var s in sessions)
                {
                    try
                    {
                        var pb = s.GetPlaybackInfo();
                        if (pb?.PlaybackStatus is not (GlobalSystemMediaTransportControlsSessionPlaybackStatus.Closed
                            or GlobalSystemMediaTransportControlsSessionPlaybackStatus.Stopped))
                        {
                            return s;
                        }
                    }
                    catch { }
                }

                return sessions[0];
            }
        }
        catch { }

        return current;
    }

    private void BindSession(MediaSession? session)
    {
        if (_session is not null)
        {
            _session.MediaPropertiesChanged -= Session_MediaPropertiesChanged;
            _session.PlaybackInfoChanged -= Session_PlaybackInfoChanged;
            _session.TimelinePropertiesChanged -= Session_TimelinePropertiesChanged;
        }

        _session = session;
        if (_session is not null)
        {
            _session.MediaPropertiesChanged += Session_MediaPropertiesChanged;
            _session.PlaybackInfoChanged += Session_PlaybackInfoChanged;
            _session.TimelinePropertiesChanged += Session_TimelinePropertiesChanged;
        }

        Refresh(includeProperties: true);
    }

    private void Session_MediaPropertiesChanged(MediaSession sender, MediaPropertiesChangedEventArgs args) =>
        Dispatcher.BeginInvoke(DispatcherPriority.Background, () =>
        {
            if (IsActive && IsSameSession(sender, _session)) Refresh(includeProperties: true);
        });

    private void Session_PlaybackInfoChanged(MediaSession sender, PlaybackInfoChangedEventArgs args) =>
        Dispatcher.BeginInvoke(DispatcherPriority.Background, () =>
        {
            if (IsActive && IsSameSession(sender, _session)) Refresh(includeProperties: false);
        });

    private void Session_TimelinePropertiesChanged(MediaSession sender, TimelinePropertiesChangedEventArgs args) =>
        Dispatcher.BeginInvoke(DispatcherPriority.Background, () =>
        {
            if (IsActive && IsSameSession(sender, _session)) RefreshTimeline();
        });

    private void RefreshTimeline()
    {
        if (_session is null) return;
        try
        {
            var timeline = _session.GetTimelineProperties();
            if (timeline is null) return;

            var start = timeline.StartTime;
            var end = timeline.EndTime;
            if (end <= TimeSpan.Zero && timeline.MaxSeekTime > TimeSpan.Zero)
                end = timeline.MaxSeekTime;

            var pos = timeline.Position;
            var updatedAt = timeline.LastUpdatedTime;

            _startTime = start;
            _endTime = end;
            _position = pos;
            _positionUpdatedAt = updatedAt != default ? updatedAt : DateTimeOffset.UtcNow;

            if (!_draggingProgress) UpdateProgressUI();
        }
        catch (Exception ex)
        {
            AppLog.Warning($"Media timeline refresh failed: {ex.Message}");
        }
    }

    private void UpdateProgressUI()
    {
        var duration = _endTime - _startTime;
        if (duration.TotalSeconds <= 0.5)
        {
            _updatingProgress = true;
            ProgressSlider.Maximum = 100;
            ProgressSlider.Value = 0;
            ProgressSlider.IsEnabled = false;
            ProgressSlider.ToolTip = null;
            ElapsedText.Text = "0:00";
            TotalText.Text = "0:00";
            _updatingProgress = false;
            return;
        }

        var elapsed = (_position - _startTime).TotalSeconds;
        if (_playing && _positionUpdatedAt != default)
        {
            var delta = (DateTimeOffset.UtcNow - _positionUpdatedAt).TotalSeconds;
            if (delta > -2)
                elapsed += Math.Max(0, delta);
        }

        elapsed = Math.Clamp(elapsed, 0, duration.TotalSeconds);
        var elapsedText = FormatTime(TimeSpan.FromSeconds(elapsed));
        var totalText = FormatTime(duration);

        _updatingProgress = true;
        ProgressSlider.Maximum = duration.TotalSeconds;
        ProgressSlider.Value = elapsed;
        ProgressSlider.IsEnabled = true;
        ProgressSlider.IsHitTestVisible = _canSeek;
        ProgressSlider.Focusable = _canSeek;
        ElapsedText.Text = elapsedText;
        TotalText.Text = totalText;
        ProgressSlider.ToolTip = $"{elapsedText} / {totalText}";
        _updatingProgress = false;
    }

    private static string FormatTime(TimeSpan span) =>
        span.TotalHours >= 1 ? span.ToString(@"h\:mm\:ss") : span.ToString(@"m\:ss");

    private async void Refresh(bool includeProperties)
    {
        var version = includeProperties ? ++_refreshVersion : _refreshVersion;
        var activationVersion = _activationVersion;
        var session = _session;
        if (session is null)
        {
            HasData = false;
            ClearAlbumArt();
            return;
        }

        try
        {
            var playback = session.GetPlaybackInfo();
            var controls = playback.Controls;
            _playing = playback.PlaybackStatus == GlobalSystemMediaTransportControlsSessionPlaybackStatus.Playing;
            _canSeek = controls.IsPlaybackPositionEnabled;
            if (_lastPlayGlyph != _playing)
            {
                _lastPlayGlyph = _playing;
                PlayPauseGlyph.Content = FindResource(_playing ? "IconPauseFill" : "IconPlayFill");
            }
            PlayPauseButton.IsEnabled = controls.IsPlayPauseToggleEnabled || controls.IsPlayEnabled || controls.IsPauseEnabled;
            PreviousButton.IsEnabled = controls.IsPreviousEnabled;
            NextButton.IsEnabled = controls.IsNextEnabled;
            RefreshTimeline();
            if (!includeProperties) return;

            var properties = await session.TryGetMediaPropertiesAsync();
            if (version != _refreshVersion || activationVersion != _activationVersion || !IsActive) return;
            var title = properties?.Title;
            if (string.IsNullOrWhiteSpace(title))
            {
                HasData = false;
                ClearAlbumArt();
                return;
            }

            TitleText.Text = title;
            ArtistText.Text = string.IsNullOrWhiteSpace(properties!.Artist) ? properties.AlbumTitle : properties.Artist;
            Card.ToolTip = string.IsNullOrWhiteSpace(ArtistText.Text) ? title : $"{title}\n{ArtistText.Text}";
            HasData = true;
            await LoadArtAsync(properties, version, activationVersion);
        }
        catch (Exception ex) when (ex is COMException or InvalidOperationException)
        {
            AppLog.Warning($"Media session refresh failed: {ex.Message}");
            if (version == _refreshVersion && activationVersion == _activationVersion)
            {
                HasData = false;
                ClearAlbumArt();
            }
        }
    }

    private async Task LoadArtAsync(GlobalSystemMediaTransportControlsSessionMediaProperties properties, int version, int activationVersion)
    {
        if (properties.Thumbnail is null)
        {
            ClearAlbumArt();
            return;
        }

        try
        {
            using var stream = await properties.Thumbnail.OpenReadAsync();
            if (version != _refreshVersion || activationVersion != _activationVersion || !IsActive) return;

            var (image, tint) = await Task.Run(() =>
            {
                using var managed = stream.AsStreamForRead();
                using var copy = new MemoryStream();
                managed.CopyTo(copy);
                copy.Position = 0;

                var bitmap = new BitmapImage();
                bitmap.BeginInit();
                bitmap.CacheOption = BitmapCacheOption.OnLoad;
                bitmap.DecodePixelWidth = AlbumArtDecodePixels;
                bitmap.StreamSource = copy;
                bitmap.EndInit();
                bitmap.Freeze();
                return (bitmap, DeriveSubtleTintColor(bitmap));
            });

            if (version != _refreshVersion || activationVersion != _activationVersion || !IsActive) return;
            ArtImage.Source = image;
            ArtPlaceholder.Visibility = Visibility.Collapsed;
            AnimateArtTint(tint);
        }
        catch (Exception ex) when (ex is COMException or IOException or NotSupportedException or FileFormatException)
        {
            AppLog.Warning($"Media artwork unavailable: {ex.Message}");
        }
    }

    private async void Previous_Click(object sender, RoutedEventArgs e) => await RunAsync(s => s.TrySkipPreviousAsync());
    private async void PlayPause_Click(object sender, RoutedEventArgs e) => await RunAsync(s => s.TryTogglePlayPauseAsync());
    private async void Next_Click(object sender, RoutedEventArgs e) => await RunAsync(s => s.TrySkipNextAsync());

    private void ProgressSlider_PreviewMouseLeftButtonDown(object sender, MouseButtonEventArgs e)
    {
        _draggingProgress = true;
    }

    private async void ProgressSlider_PreviewMouseLeftButtonUp(object sender, MouseButtonEventArgs e)
    {
        if (!_draggingProgress) return;
        _draggingProgress = false;
        await SeekToSliderValueAsync();
    }

    private void ProgressSlider_LostMouseCapture(object sender, MouseEventArgs e)
    {
        _draggingProgress = false;
    }

    private async void ProgressSlider_ValueChanged(object sender, RoutedPropertyChangedEventArgs<double> e)
    {
        if (_updatingProgress) return;
        if (_draggingProgress)
        {
            ElapsedText.Text = FormatTime(TimeSpan.FromSeconds(Math.Max(0, e.NewValue)));
            return;
        }
        var mouseSeek = ProgressSlider.IsMouseOver && Mouse.LeftButton == MouseButtonState.Pressed;
        var keyboardSeek = ProgressSlider.IsKeyboardFocusWithin;
        if (!mouseSeek && !keyboardSeek) return;
        await SeekToSliderValueAsync();
    }

    private async Task SeekToSliderValueAsync()
    {
        if (_session is null || !_canSeek) return;
        var target = _startTime + TimeSpan.FromSeconds(ProgressSlider.Value);
        _position = target;
        _positionUpdatedAt = DateTimeOffset.UtcNow;
        await RunAsync(s => s.TryChangePlaybackPositionAsync(target.Ticks));
    }

    private void VolumeSlider_ValueChanged(object sender, RoutedPropertyChangedEventArgs<double> e)
    {
        if (_updatingVolume || _audio is null) return;
        if (_audio.SetVolume((float)(e.NewValue / 100.0))) RenderVolume();
        else Report(UiText.Get("ErrorVolumeChange"));
    }

    private void MuteButton_Click(object sender, RoutedEventArgs e)
    {
        if (_audio is null) return;
        if (_audio.ToggleMute()) RenderVolume();
        else Report(UiText.Get("ErrorVolumeChange"));
    }

    private void Volume_MouseWheel(object sender, MouseWheelEventArgs e)
    {
        if (_audio?.HasDevice != true) return;
        if (_audio.SetVolume(Math.Clamp(_audio.Volume + Math.Sign(e.Delta) * 0.02f, 0f, 1f))) RenderVolume();
        else Report(UiText.Get("ErrorVolumeChange"));
        e.Handled = true;
    }

    private async Task RunAsync(Func<MediaSession, IAsyncOperation<bool>> command)
    {
        if (_session is null) return;
        try
        {
            if (!await command(_session)) Report(UiText.Get("ErrorMediaControl"));
        }
        catch (COMException ex)
        {
            AppLog.Warning($"Media command failed: {ex.Message}");
            Report(UiText.Get("ErrorMediaControl"));
        }
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct NativePoint
    {
        public int X;
        public int Y;
    }

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool GetCursorPos(out NativePoint pt);
}
