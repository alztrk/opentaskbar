using System.ComponentModel;
using System.Diagnostics;
using System.IO;
using System.Runtime.InteropServices;
using System.Windows;
using System.Windows.Automation;
using System.Windows.Controls;
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
    private const long BrowserTimelineRefreshIntervalMs = 950;
    private const long VolumeSessionRetryIntervalMs = 3000;
    private static readonly Color DefaultArtTint = Color.FromRgb(0x52, 0x6D, 0x84);

    private MediaSessionManager? _manager;
    private MediaSession? _session;
    private AudioEndpointMonitor.SharedHandle? _audio;
    private AppAudioSession? _mediaVolumeSession;

    private readonly DispatcherTimer _volumeHoverCloseTimer;
    private int _activationVersion;
    private int _refreshVersion;
    private readonly MediaTimeline _timeline = new();
    private string? _mediaTitle;
    private bool _hasWindowsTimeline;
    private bool _usingBrowserTimeline;
    private bool _readingBrowserTimeline;
    private long _lastBrowserTimelineRead;
    private bool _canSeek;
    private bool _updatingProgress;
    private bool _draggingProgress;
    private bool _seekingProgress;
    private bool _updatingVolume;
    private bool? _lastPlayGlyph;
    private bool _islandDraggingProgress;
    private bool _updatingIslandProgress;
    private bool _updatingIslandVolume;

    private bool? _lastAudioHasDevice;
    private int _lastAudioLevel = -1;
    private bool _lastAudioMuted;
    private string? _lastAudioIconKey;
    private AppLanguage _lastAudioLanguage;
    private Border? _islandDetails;
    private Image? _islandArtImage;
    private TextBlock? _islandTitleText;
    private TextBlock? _islandArtistText;
    private TextBlock? _islandElapsedText;
    private TextBlock? _islandTotalText;
    private Slider? _islandProgressSlider;
    private Slider? _islandVolumeSlider;
    private Slider? _islandCompactVolumeSlider;
    private Button? _islandMuteButton;
    private ContentControl? _islandPlayPauseGlyph;
    private ContentControl? _islandVolumeGlyph;

    // Locally confirmed per-app volume state (AppAudioSession.Volume is a snapshot, not live).
    private float _mediaVolumeLevel = -1;
    private bool _mediaVolumeMuted;
    private long _lastVolumeSessionRetryMs;

    public override bool SupportsCompactMode => true;
    public override int ShrinkPriority => 10;
    public override double FullWidthEstimate => 420;
    public override double CompactWidthEstimate => 164;

    public MediaWidget()
    {
        InitializeComponent();
        _islandCompactVolumeSlider = IslandVolumeSlider;
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
    protected override void OnDynamicIslandChanged(bool dynamicIsland) => ApplyDensityLayout();

    private void ApplyDensityLayout()
    {
        var compact = IsCompact && !IsHoverExpanded;
        if (IsDynamicIsland)
        {
            Card.Padding = new Thickness(4, 0, 4, 0);
            ArtFrame.Visibility = Visibility.Visible;
            ArtFrame.Width = 22;
            ArtFrame.Height = 22;
            ArtImage.Width = 22;
            ArtImage.Height = 22;
            InfoPanel.Width = 82;
            InfoPanel.Margin = new Thickness(0, 0, 5, 0);
            ArtistText.Visibility = Visibility.Collapsed;
            ProgressPanel.MinWidth = 34;
            ProgressPanel.Margin = new Thickness(0, 0, 5, 0);
            ProgressPanel.Visibility = Visibility.Visible;
            PreviousButton.Visibility = Visibility.Visible;
            NextButton.Visibility = Visibility.Visible;
            PreviousButton.Width = 23;
            PlayPauseButton.Width = 25;
            NextButton.Width = 23;
            PreviousButton.Height = 25;
            PlayPauseButton.Height = 25;
            NextButton.Height = 25;
            TransportPanel.Margin = new Thickness(0);
            VolumePanel.Visibility = Visibility.Visible;
            VolumePanel.Width = 48;
            MuteButton.Visibility = Visibility.Collapsed;
            IslandVolumeSlider.Visibility = Visibility.Visible;
            ElapsedText.Visibility = Visibility.Collapsed;
            TotalText.Visibility = Visibility.Collapsed;
            if (VolumePopup.IsOpen) CloseVolumePopover();
            UpdateProgressUI();
            UpdateIslandVolumeControls();
            RaiseContentWidthChanged();
            return;
        }

        ArtFrame.Visibility = Visibility.Visible;
        ArtFrame.Width = 29;
        ArtFrame.Height = 29;
        ArtImage.Width = 29;
        ArtImage.Height = 29;
        ElapsedText.Visibility = Visibility.Visible;
        TotalText.Visibility = Visibility.Visible;
        Card.Padding = compact ? new Thickness(6, 0, 6, 0) : new Thickness(9, 0, 9, 0);
        InfoPanel.Width = compact ? 76 : 96;
        InfoPanel.Margin = compact ? new Thickness(7, 0, 6, 0) : new Thickness(9, 0, 10, 0);
        ProgressPanel.MinWidth = 138;
        ProgressPanel.Margin = new Thickness(0, 0, 8, 0);
        TransportPanel.Margin = compact ? new Thickness(0) : new Thickness(0, 0, 4, 0);
        ArtistText.Visibility = compact ? Visibility.Collapsed : Visibility.Visible;
        ProgressPanel.Visibility = compact ? Visibility.Collapsed : Visibility.Visible;
        PreviousButton.Visibility = compact ? Visibility.Collapsed : Visibility.Visible;
        NextButton.Visibility = compact ? Visibility.Collapsed : Visibility.Visible;
        VolumePanel.Visibility = compact ? Visibility.Collapsed : Visibility.Visible;
        VolumePanel.Width = double.NaN;
        MuteButton.Visibility = Visibility.Visible;
        IslandVolumeSlider.Visibility = Visibility.Collapsed;
        PreviousButton.Width = 25;
        PlayPauseButton.Width = 27;
        NextButton.Width = 25;
        PreviousButton.Height = 27;
        PlayPauseButton.Height = 27;
        NextButton.Height = 27;
        if (compact && VolumePopup.IsOpen) CloseVolumePopover();
        UpdateProgressUI();
        UpdateIslandVolumeControls();
        RaiseContentWidthChanged();
    }

    // V19: Media widget uses its own compact hover volume popover and does not open a large generic flyout.
    public override UIElement? BuildHoverFlyoutContent() => null;

    public override UIElement BuildDynamicIslandExpandedContent()
    {
        var surface = new Border
        {
            Width = 384,
            Padding = new Thickness(12),
            CornerRadius = new CornerRadius(9),
            Background = new LinearGradientBrush(
                Color.FromRgb(0x35, 0x23, 0x36), Color.FromRgb(0x1C, 0x1A, 0x24), 140)
        };

        var layout = new Grid();
        layout.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        layout.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        var top = new Grid { Margin = new Thickness(0, 0, 0, 12), MinHeight = 58 };
        top.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(56) });
        top.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        top.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        top.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(116) });

        var artFrame = new Border { Width = 56, Height = 56, CornerRadius = new CornerRadius(10), ClipToBounds = true };
        artFrame.SetResourceReference(Border.BackgroundProperty, "WidgetSelectedFill");
        var art = new Image { Width = 56, Height = 56, Stretch = Stretch.UniformToFill };
        _islandArtImage = art;
        artFrame.Child = art;
        top.Children.Add(artFrame);

        var metadata = new StackPanel { VerticalAlignment = VerticalAlignment.Center, Margin = new Thickness(14, 0, 12, 0) };
        var title = new TextBlock { FontSize = 15, FontWeight = FontWeights.SemiBold, TextTrimming = TextTrimming.CharacterEllipsis };
        title.SetResourceReference(TextBlock.ForegroundProperty, "TextPrimary");
        _islandTitleText = title;
        var artist = new TextBlock { FontSize = 12, Margin = new Thickness(0, 3, 0, 0), TextTrimming = TextTrimming.CharacterEllipsis };
        artist.SetResourceReference(TextBlock.ForegroundProperty, "TextSecondary");
        _islandArtistText = artist;
        metadata.Children.Add(title);
        metadata.Children.Add(artist);
        Grid.SetColumn(metadata, 1);
        top.Children.Add(metadata);

        var transport = new StackPanel { Orientation = Orientation.Horizontal, VerticalAlignment = VerticalAlignment.Center, Margin = new Thickness(0, 0, 10, 0) };
        transport.Children.Add(CreateIslandTransportButton("IconPreviousFill", UiText.Get("MediaPrevious"), Previous_Click, 24));
        var playPause = CreateIslandTransportButton("IconPlayFill", UiText.Get("MediaPlayPause"), PlayPause_Click, 30);
        _islandPlayPauseGlyph = (ContentControl)playPause.Content;
        transport.Children.Add(playPause);
        transport.Children.Add(CreateIslandTransportButton("IconNextFill", UiText.Get("MediaNext"), Next_Click, 24));
        Grid.SetColumn(transport, 2);
        top.Children.Add(transport);

        var volume = new Grid { VerticalAlignment = VerticalAlignment.Center };
        volume.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(28) });
        volume.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        var mute = new Button { Width = 26, Height = 26, Margin = new Thickness(0, 0, 6, 0), ToolTip = UiText.Get("MuteToggle") };
        mute.SetResourceReference(StyleProperty, "WidgetIconButton");
        mute.Click += MuteButton_Click;
        var volumeGlyph = new ContentControl { Width = 14, Height = 14 };
        volumeGlyph.SetResourceReference(StyleProperty, "WidgetIcon");
        volumeGlyph.Content = FindResource("IconVolume");
        _islandVolumeGlyph = volumeGlyph;
        mute.Content = volumeGlyph;
        _islandMuteButton = mute;
        volume.Children.Add(mute);
        var volumeSlider = new Slider { Minimum = 0, Maximum = 100, VerticalAlignment = VerticalAlignment.Center, Margin = new Thickness(2, 0, 0, 0) };
        volumeSlider.SetResourceReference(StyleProperty, "WidgetSlider");
        volumeSlider.ValueChanged += IslandVolumeSlider_ValueChanged;
        _islandVolumeSlider = volumeSlider;
        Grid.SetColumn(volumeSlider, 1);
        volume.Children.Add(volumeSlider);
        Grid.SetColumn(volume, 3);
        top.Children.Add(volume);
        layout.Children.Add(top);

        var progress = new Grid { VerticalAlignment = VerticalAlignment.Center };
        progress.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(44) });
        progress.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        progress.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(44) });
        var elapsed = CreateIslandTimeLabel();
        var total = CreateIslandTimeLabel();
        _islandElapsedText = elapsed;
        _islandTotalText = total;
        progress.Children.Add(elapsed);
        var progressSlider = new Slider { Minimum = 0, Maximum = 100, VerticalAlignment = VerticalAlignment.Center, Margin = new Thickness(8, 0, 8, 0) };
        progressSlider.SetResourceReference(StyleProperty, "MediaProgressSlider");
        SliderTrackDrag.SetIsEnabled(progressSlider, true);
        progressSlider.PreviewMouseLeftButtonDown += IslandProgressSlider_PreviewMouseLeftButtonDown;
        progressSlider.PreviewMouseLeftButtonUp += IslandProgressSlider_PreviewMouseLeftButtonUp;
        progressSlider.LostMouseCapture += IslandProgressSlider_LostMouseCapture;
        progressSlider.ValueChanged += IslandProgressSlider_ValueChanged;
        _islandProgressSlider = progressSlider;
        Grid.SetColumn(progressSlider, 1);
        progress.Children.Add(progressSlider);
        Grid.SetColumn(total, 2);
        progress.Children.Add(total);
        Grid.SetRow(progress, 1);
        layout.Children.Add(progress);

        surface.Child = layout;
        _islandDetails = surface;
        surface.Unloaded += IslandDetails_Unloaded;
        UpdateIslandMetadata();
        UpdateProgressUI();
        RenderVolume();
        return surface;
    }

    private Button CreateIslandTransportButton(string iconKey, string label, RoutedEventHandler click, double size)
    {
        var button = new Button { Width = size, Height = 30, ToolTip = label };
        button.SetResourceReference(StyleProperty, "WidgetIconButton");
        button.Click += click;
        var glyph = new ContentControl { Width = 13, Height = 13 };
        glyph.SetResourceReference(StyleProperty, "WidgetGlyph");
        glyph.Content = FindResource(iconKey);
        button.Content = glyph;
        AutomationProperties.SetName(button, label);
        return button;
    }

    private static TextBlock CreateIslandTimeLabel()
    {
        var label = new TextBlock
        {
            FontSize = 10,
            VerticalAlignment = VerticalAlignment.Center,
            Typography = { NumeralAlignment = FontNumeralAlignment.Tabular }
        };
        label.SetResourceReference(TextBlock.ForegroundProperty, "TextSecondary");
        return label;
    }

    private void IslandDetails_Unloaded(object sender, RoutedEventArgs e)
    {
        if (!ReferenceEquals(sender, _islandDetails)) return;
        _islandDetails = null;
        _islandArtImage = null;
        _islandTitleText = null;
        _islandArtistText = null;
        _islandElapsedText = null;
        _islandTotalText = null;
        _islandProgressSlider = null;
        _islandVolumeSlider = null;
        _islandMuteButton = null;
        _islandPlayPauseGlyph = null;
        _islandVolumeGlyph = null;
        _islandDraggingProgress = false;
    }

    private void IslandVolumeSlider_ValueChanged(object sender, RoutedPropertyChangedEventArgs<double> e)
    {
        if (!_updatingIslandVolume) SetMediaVolume((float)(e.NewValue / 100.0));
    }

    private void IslandCompactVolumeSlider_ValueChanged(object sender, RoutedPropertyChangedEventArgs<double> e)
    {
        if (!_updatingIslandVolume) SetMediaVolume((float)(e.NewValue / 100.0));
    }

    private void IslandProgressSlider_PreviewMouseLeftButtonDown(object sender, MouseButtonEventArgs e)
    {
        if (_islandProgressSlider is { IsMouseOver: true, IsHitTestVisible: true })
            _islandDraggingProgress = true;
    }

    private async void IslandProgressSlider_PreviewMouseLeftButtonUp(object sender, MouseButtonEventArgs e)
    {
        if (!_islandDraggingProgress || _islandProgressSlider is not { } slider) return;
        _islandDraggingProgress = false;
        await SeekToElapsedAsync(slider.Value);
    }

    private void IslandProgressSlider_LostMouseCapture(object sender, MouseEventArgs e)
    {
        if (!_islandDraggingProgress || _islandProgressSlider?.IsMouseCaptureWithin == true) return;
        _islandDraggingProgress = false;
        if (!_seekingProgress) UpdateProgressUI();
    }

    private async void IslandProgressSlider_ValueChanged(object sender, RoutedPropertyChangedEventArgs<double> e)
    {
        if (_updatingIslandProgress || _islandProgressSlider is not { } slider) return;
        if (_islandDraggingProgress)
        {
            if (_islandElapsedText is not null)
                _islandElapsedText.Text = FormatTime(TimeSpan.FromSeconds(Math.Max(0, e.NewValue)));
            return;
        }
        if (slider.IsKeyboardFocusWithin) await SeekToElapsedAsync(slider.Value);
    }

    private void UpdateIslandMetadata()
    {
        if (_islandTitleText is not null) _islandTitleText.Text = TitleText.Text;
        if (_islandArtistText is not null) _islandArtistText.Text = ArtistText.Text;
        if (_islandArtImage is not null) _islandArtImage.Source = ArtImage.Source;
        if (_islandPlayPauseGlyph is not null)
        {
            var isPlaying = _lastPlayGlyph == true;
            _islandPlayPauseGlyph.Content = FindResource(isPlaying ? "IconPauseFill" : "IconPlayFill");
        }
    }

    private void UpdateIslandVolumeControls()
    {
        if (_islandVolumeSlider is not null)
        {
            _updatingIslandVolume = true;
            _islandVolumeSlider.IsEnabled = _mediaVolumeSession is not null;
            _islandVolumeSlider.Value = _mediaVolumeSession is null || _mediaVolumeMuted
                ? 0
                : Math.Clamp(_mediaVolumeLevel * 100, 0, 100);
            _updatingIslandVolume = false;
        }

        if (_islandCompactVolumeSlider is not null)
        {
            _updatingIslandVolume = true;
            _islandCompactVolumeSlider.IsEnabled = _mediaVolumeSession is not null;
            _islandCompactVolumeSlider.Value = _mediaVolumeSession is null || _mediaVolumeMuted
                ? 0
                : Math.Clamp(_mediaVolumeLevel * 100, 0, 100);
            _updatingIslandVolume = false;
        }

        if (_islandMuteButton is not null)
            _islandMuteButton.IsEnabled = _mediaVolumeSession is not null;
        if (_islandVolumeGlyph is not null && _lastAudioIconKey is not null)
            _islandVolumeGlyph.Content = FindResource(_lastAudioIconKey);
    }

    public override UIElement? BuildPreviewFlyoutContent()
    {
        var root = new StackPanel { HorizontalAlignment = HorizontalAlignment.Center };
        var percent = new TextBlock
        {
            Text = "67%",
            FontSize = 11.5,
            FontWeight = FontWeights.SemiBold,
            HorizontalAlignment = HorizontalAlignment.Center,
            Margin = new Thickness(0, 0, 0, 8)
        };
        percent.SetResourceReference(TextBlock.ForegroundProperty, "TextPrimary");
        var slider = new Slider
        {
            Height = 104,
            Value = 67,
            IsHitTestVisible = false,
            HorizontalAlignment = HorizontalAlignment.Center
        };
        slider.SetResourceReference(StyleProperty, "WidgetVerticalSlider");
        root.Children.Add(percent);
        root.Children.Add(slider);
        return root;
    }

    protected override void OnActivated() => _ = ActivateAsync();

    private async Task ActivateAsync()
    {
        var activationVersion = ++_activationVersion;

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
        catch (Exception ex)
        {
            // async void cannot be caught by callers; log and degrade instead of crashing the app.
            AppLog.Warning($"Media widget activation failed: {ex.Message}");
        }
    }

    protected override void OnDeactivated()
    {
        _activationVersion++;
        CloseVolumePopover();
        BindSession(null);
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
        if (!IsSameSession(candidate, _session))
        {
            BindSession(candidate);
            return;
        }

        if (_session is null) return;
        RefreshPlayback(_session);
        RefreshMediaVolumeState();
    }

    private void RefreshMediaVolumeState()
    {
        var state = _mediaVolumeSession?.State ?? AppAudioSessionState.Unavailable;
        if (state != AppAudioSessionState.Active
            && Environment.TickCount64 - _lastVolumeSessionRetryMs >= VolumeSessionRetryIntervalMs)
        {
            BindMediaVolumeSession();
            RenderVolume();
            return;
        }
        if (_mediaVolumeSession is not { } session) return;
        if (state == AppAudioSessionState.Unavailable)
        {
            ClearMediaVolumeSession();
            RenderVolume();
            return;
        }
        var level = Math.Clamp(session.CurrentVolume, 0f, 1f);
        var muted = session.CurrentMute;
        if (Math.Abs(level - _mediaVolumeLevel) < 0.002f && muted == _mediaVolumeMuted) return;
        _mediaVolumeLevel = level;
        _mediaVolumeMuted = muted;
        RenderVolume();
    }

    protected override void OnLanguageChanged() => RenderVolume();

    private void InvalidateAudioCache()
    {
        _lastAudioHasDevice = null;
        _lastAudioLevel = -1;
        _lastAudioIconKey = null;
    }

    /// <summary>
    /// Resolves the WASAPI app audio session that belongs to the currently bound media session.
    /// Only per-app sessions are used - the master endpoint volume is never changed here.
    /// </summary>
    private void BindMediaVolumeSession()
    {
        _lastVolumeSessionRetryMs = Environment.TickCount64;
        ClearMediaVolumeSession();
        try
        {
            if (_session is not null && _audio is not null)
                _mediaVolumeSession = _audio.FindAppSession(_session.SourceAppUserModelId);
        }
        catch (Exception ex) when (ex is COMException or InvalidOperationException)
        {
            AppLog.Warning($"Media app volume session lookup failed: {ex.Message}");
        }

        _mediaVolumeLevel = _mediaVolumeSession?.Volume ?? -1;
        _mediaVolumeMuted = _mediaVolumeSession?.IsMuted ?? false;
    }

    private void ClearMediaVolumeSession()
    {
        _mediaVolumeSession?.Dispose();
        _mediaVolumeSession = null;
        InvalidateAudioCache();
    }

    private void ClearAlbumArt()
    {
        ArtImage.Source = null;
        if (_islandArtImage is not null) _islandArtImage.Source = null;
        ArtPlaceholder.Visibility = Visibility.Visible;
        AnimateArtTint(DefaultArtTint);
    }

    private void AnimateArtTint(Color targetColor)
    {
        if (!Settings.EffectiveAnimationsEnabled || !SystemParameters.ClientAreaAnimation)
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
        var gap = DockMetrics.FlyoutGap;
        return DockFlyout.Edge switch
        {
            DockEdge.Left =>
            [
                new CustomPopupPlacement(new System.Windows.Point(targetSize.Width + gap, (targetSize.Height - popupSize.Height) / 2), PopupPrimaryAxis.Vertical)
            ],
            DockEdge.Right =>
            [
                new CustomPopupPlacement(new System.Windows.Point(-popupSize.Width - gap, (targetSize.Height - popupSize.Height) / 2), PopupPrimaryAxis.Vertical)
            ],
            _ =>
            [
                new CustomPopupPlacement(new System.Windows.Point((targetSize.Width - popupSize.Width) / 2, -popupSize.Height - gap), PopupPrimaryAxis.Horizontal),
                new CustomPopupPlacement(new System.Windows.Point((targetSize.Width - popupSize.Width) / 2, targetSize.Height + gap), PopupPrimaryAxis.Horizontal)
            ]
        };
    }

    private void VolumePopup_Opened(object? sender, EventArgs e)
    {
        if (!Settings.EffectiveAnimationsEnabled || !SystemParameters.ClientAreaAnimation)
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
        if (IsDynamicIsland) return;
        BindMediaVolumeSession();
        RenderVolume();
        if (_mediaVolumeSession is null) return;
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
            current = (current is Visual or System.Windows.Media.Media3D.Visual3D ? VisualTreeHelper.GetParent(current) : null)
                ?? LogicalTreeHelper.GetParent(current);
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
        if (!IsActive) return;
        // Master endpoint changes do not represent the playing app's volume. Only refresh
        // when no app session is bound yet - it may have just appeared.
        if (_mediaVolumeSession is null)
        {
            BindMediaVolumeSession();
            RenderVolume();
        }
    });

    private void Audio_DefaultDeviceChanged() => Dispatcher.BeginInvoke(DispatcherPriority.Background, () =>
    {
        if (!IsActive || _audio is null) return;
        _audio.BindDefaultDevice();
        BindMediaVolumeSession();
        RenderVolume();
    });

    private void RenderVolume()
    {
        var lang = UiText.Current.Language;
        var session = _mediaVolumeSession;

        // Without a per-app audio session the slider stays disabled: system/master volume
        // must never be changed from the media widget.
        if (session is null)
        {
            CloseVolumePopover();
            if (_lastAudioHasDevice == false && _lastAudioLanguage == lang)
            {
                UpdateIslandVolumeControls();
                return;
            }
            _lastAudioHasDevice = false;
            _lastAudioLanguage = lang;
            _lastAudioLevel = -1;
            MuteButton.IsEnabled = false;
            VolumeSlider.IsEnabled = false;
            VolumePercentText.Text = "—";
            SetVolumeIcon("IconVolumeMute");
            MuteButton.ToolTip = UiText.Get("Volume");
            UpdateIslandVolumeControls();
            return;
        }

        var level = (int)Math.Round(Math.Clamp(_mediaVolumeLevel, 0f, 1f) * 100);
        var muted = _mediaVolumeMuted;
        if (_lastAudioHasDevice == true && _lastAudioLevel == level && _lastAudioMuted == muted
            && _lastAudioLanguage == lang)
        {
            UpdateIslandVolumeControls();
            return;
        }

        _lastAudioHasDevice = true;
        _lastAudioLevel = level;
        _lastAudioMuted = muted;
        _lastAudioLanguage = lang;

        MuteButton.IsEnabled = true;
        VolumeSlider.IsEnabled = true;
        SetVolumeIcon(muted || level == 0 ? "IconVolumeMute" : level < 50 ? "IconVolumeLow" : "IconVolume");
        var displayed = muted ? 0 : level;
        VolumePercentText.Text = $"{displayed}%";
        MuteButton.ToolTip = muted ? UiText.Get("MuteToggle")
            : $"{session.Name}: {UiText.Format("Percent", level)}";
        _updatingVolume = true;
        VolumeSlider.Value = displayed;
        _updatingVolume = false;
        UpdateIslandVolumeControls();
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
            if (!IsSameSession(candidate, _session)) BindSession(candidate);
        });

    private void Manager_SessionsChanged(MediaSessionManager sender, SessionsChangedEventArgs args) =>
        Dispatcher.BeginInvoke(DispatcherPriority.Background, () =>
        {
            if (!IsActive || _manager is null) return;
            var candidate = GetActiveSession();
            if (!IsSameSession(candidate, _session)) BindSession(candidate);
        });

    private static bool IsSameSession(MediaSession? a, MediaSession? b)
    {
        if (ReferenceEquals(a, b)) return true;
        if (a is null || b is null) return false;
        try
        {
            return a.Equals(b);
        }
        catch
        {
            return false;
        }
    }

    private MediaSession? GetActiveSession()
    {
        if (_manager is null) return null;

        MediaSession? current = null;
        try { current = _manager.GetCurrentSession(); } catch (Exception ex) { AppLog.Warning($"Media widget operation failed: {ex.Message}"); }

        if (current is not null)
        {
            try
            {
                var pb = current.GetPlaybackInfo();
                if (pb?.PlaybackStatus == GlobalSystemMediaTransportControlsSessionPlaybackStatus.Playing)
                    return current;
            }
            catch (Exception ex) { AppLog.Warning($"Media widget operation failed: {ex.Message}"); }
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
                    catch (Exception ex) { AppLog.Warning($"Media widget operation failed: {ex.Message}"); }
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
                    catch (Exception ex) { AppLog.Warning($"Media widget operation failed: {ex.Message}"); }
                }

                return sessions[0];
            }
        }
        catch (Exception ex) { AppLog.Warning($"Media widget operation failed: {ex.Message}"); }

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
        CloseVolumePopover();
        _timeline.Reset();
        _mediaTitle = null;
        _hasWindowsTimeline = false;
        _usingBrowserTimeline = false;
        _lastBrowserTimelineRead = 0;
        _draggingProgress = false;
        _seekingProgress = false;
        _canSeek = false;
        UpdateProgressUI();
        if (_session is not null)
        {
            _session.MediaPropertiesChanged += Session_MediaPropertiesChanged;
            _session.PlaybackInfoChanged += Session_PlaybackInfoChanged;
            _session.TimelinePropertiesChanged += Session_TimelinePropertiesChanged;
        }

        BindMediaVolumeSession();
        RenderVolume();
        _ = RefreshMediaAsync();
    }

    private void Session_MediaPropertiesChanged(MediaSession sender, MediaPropertiesChangedEventArgs args) =>
        Dispatcher.BeginInvoke(DispatcherPriority.Background, () =>
        {
            if (IsActive && IsSameSession(sender, _session)) _ = RefreshMediaAsync();
        });

    private void Session_PlaybackInfoChanged(MediaSession sender, PlaybackInfoChangedEventArgs args) =>
        Dispatcher.BeginInvoke(DispatcherPriority.Background, () =>
        {
            if (IsActive && IsSameSession(sender, _session)) RefreshPlayback(sender);
        });

    private void Session_TimelinePropertiesChanged(MediaSession sender, TimelinePropertiesChangedEventArgs args) =>
        Dispatcher.BeginInvoke(DispatcherPriority.Background, () =>
        {
            if (IsActive && IsSameSession(sender, _session)) RefreshPlayback(sender);
        });

    private void RefreshPlayback(MediaSession session)
    {
        try
        {
            var playback = session.GetPlaybackInfo();
            var controls = playback.Controls;
            var playing = playback.PlaybackStatus == GlobalSystemMediaTransportControlsSessionPlaybackStatus.Playing;
            if (_lastPlayGlyph != playing)
            {
                _lastPlayGlyph = playing;
                PlayPauseGlyph.Content = FindResource(playing ? "IconPauseFill" : "IconPlayFill");
            }
            if (_islandPlayPauseGlyph is not null)
                _islandPlayPauseGlyph.Content = FindResource(playing ? "IconPauseFill" : "IconPlayFill");
            PlayPauseButton.IsEnabled = controls.IsPlayPauseToggleEnabled || controls.IsPlayEnabled || controls.IsPauseEnabled;
            PreviousButton.IsEnabled = controls.IsPreviousEnabled;
            NextButton.IsEnabled = controls.IsNextEnabled;
            _canSeek = controls.IsPlaybackPositionEnabled;

            var timeline = session.GetTimelineProperties();
            var start = timeline.StartTime;
            var end = timeline.EndTime > start ? timeline.EndTime : timeline.MaxSeekTime;
            var reportedRate = playback.PlaybackRate.GetValueOrDefault();
            var rate = reportedRate > 0 && double.IsFinite(reportedRate) ? reportedRate : 1;
            _hasWindowsTimeline = end > start;
            if (_hasWindowsTimeline)
            {
                _usingBrowserTimeline = false;
                _timeline.Update(new(start, end, timeline.Position, timeline.LastUpdatedTime),
                    playing ? rate : 0, DateTimeOffset.UtcNow, Stopwatch.GetTimestamp());
            }
            else
            {
                _canSeek = false;
                _draggingProgress = false;
                if (!_usingBrowserTimeline) _timeline.Reset();
                var tick = Environment.TickCount64;
                if (!_readingBrowserTimeline && _mediaTitle is { } mediaTitle
                    && tick - _lastBrowserTimelineRead >= BrowserTimelineRefreshIntervalMs)
                {
                    _lastBrowserTimelineRead = tick;
                    _ = RefreshBrowserTimelineAsync(session.SourceAppUserModelId, mediaTitle, _refreshVersion);
                }
            }
        }
        catch (Exception ex) when (ex is COMException or InvalidOperationException)
        {
            AppLog.Warning($"Media playback refresh failed: {ex.Message}");
        }

        if (!_draggingProgress && !_seekingProgress) UpdateProgressUI();
    }

    private async Task RefreshBrowserTimelineAsync(string sourceAppId, string title, int version)
    {
        _readingBrowserTimeline = true;
        try
        {
            var sample = await Task.Run(() => LibreWolfMediaTimeline.Read(sourceAppId, title));
            if (!IsActive || _hasWindowsTimeline || version != _refreshVersion) return;

            _usingBrowserTimeline = sample.HasValue;
            if (sample is { } timeline)
            {
                // Accessibility reports the actual position on every read and does not report a playback rate.
                _timeline.Update(new(TimeSpan.Zero, timeline.Duration, timeline.Position, default),
                    0, DateTimeOffset.UtcNow, Stopwatch.GetTimestamp());
            }
            else
            {
                _timeline.Reset();
            }
            if (!_seekingProgress) UpdateProgressUI();
        }
        finally
        {
            _readingBrowserTimeline = false;
        }
    }

    private void UpdateProgressUI()
    {
        var duration = _timeline.Duration;
        var available = duration > 0;
        var elapsed = _timeline.PositionAt(Stopwatch.GetTimestamp());
        var elapsedText = available ? FormatTime(TimeSpan.FromSeconds(elapsed)) : string.Empty;
        var totalText = available ? FormatTime(TimeSpan.FromSeconds(duration)) : string.Empty;

        _updatingProgress = true;
        TimelineControls.Visibility = available || IsDynamicIsland ? Visibility.Visible : Visibility.Collapsed;
        ProgressSlider.Maximum = available ? duration : 100;
        ProgressSlider.Value = elapsed;
        ProgressSlider.IsEnabled = available;
        ProgressSlider.IsHitTestVisible = available && _canSeek;
        ProgressSlider.Focusable = available && _canSeek;
        ElapsedText.Text = elapsedText;
        TotalText.Text = totalText;
        ProgressSlider.ToolTip = available ? $"{elapsedText} / {totalText}" : null;
        _updatingProgress = false;

        if (_islandProgressSlider is { } islandProgress
            && _islandElapsedText is { } islandElapsed
            && _islandTotalText is { } islandTotal
            && !_islandDraggingProgress && !_seekingProgress)
        {
            _updatingIslandProgress = true;
            islandProgress.Maximum = available ? duration : 100;
            islandProgress.Value = elapsed;
            islandProgress.IsEnabled = available && _canSeek;
            islandProgress.IsHitTestVisible = available && _canSeek;
            islandProgress.Focusable = available && _canSeek;
            islandElapsed.Text = elapsedText;
            islandTotal.Text = totalText;
            _updatingIslandProgress = false;
        }
    }

    private static string FormatTime(TimeSpan span) =>
        span.TotalHours >= 1 ? span.ToString(@"h\:mm\:ss") : span.ToString(@"m\:ss");

    private async Task RefreshMediaAsync()
    {
        var version = ++_refreshVersion;
        var activationVersion = _activationVersion;
        var session = _session;
        _mediaTitle = null;
        if (_usingBrowserTimeline)
        {
            _usingBrowserTimeline = false;
            _timeline.Reset();
        }
        if (session is null)
        {
            HasData = false;
            ClearAlbumArt();
            return;
        }

        try
        {
            RefreshPlayback(session);
            var properties = await session.TryGetMediaPropertiesAsync();
            if (version != _refreshVersion || activationVersion != _activationVersion || !IsActive) return;
            var title = properties?.Title;
            if (string.IsNullOrWhiteSpace(title))
            {
                HasData = false;
                ClearAlbumArt();
                return;
            }

            _mediaTitle = title;
            TitleText.Text = title;
            ArtistText.Text = string.IsNullOrWhiteSpace(properties!.Artist) ? properties.AlbumTitle : properties.Artist;
            Card.ToolTip = string.IsNullOrWhiteSpace(ArtistText.Text) ? title : $"{title}\n{ArtistText.Text}";
            UpdateIslandMetadata();
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
        catch (Exception ex)
        {
            AppLog.Warning($"Media widget refresh failed: {ex.Message}");
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
            if (_islandArtImage is not null) _islandArtImage.Source = image;
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
        if (ProgressSlider.IsMouseOver && ProgressSlider.IsHitTestVisible)
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
        if (!_draggingProgress || ProgressSlider.IsMouseCaptureWithin) return;
        _draggingProgress = false;
        if (!_seekingProgress) UpdateProgressUI();
    }

    private async void ProgressSlider_ValueChanged(object sender, RoutedPropertyChangedEventArgs<double> e)
    {
        if (_updatingProgress) return;
        if (_draggingProgress)
        {
            ElapsedText.Text = FormatTime(TimeSpan.FromSeconds(Math.Max(0, e.NewValue)));
            return;
        }
        if (ProgressSlider.IsKeyboardFocusWithin) await SeekToSliderValueAsync();
    }

    private Task SeekToSliderValueAsync() => SeekToElapsedAsync(ProgressSlider.Value);

    private async Task SeekToElapsedAsync(double elapsed)
    {
        if (_session is not { } session || !_canSeek || _seekingProgress || _timeline.Sample is not { } sample) return;
        var target = sample.Start + TimeSpan.FromSeconds(Math.Clamp(elapsed, 0, _timeline.Duration));
        _seekingProgress = true;
        try
        {
            if (await RunAsync(s => s.TryChangePlaybackPositionAsync(target.Ticks))
                && ReferenceEquals(session, _session) && _timeline.Sample == sample)
                _timeline.ConfirmSeek(elapsed, Stopwatch.GetTimestamp());
        }
        finally
        {
            if (ReferenceEquals(session, _session))
            {
                _seekingProgress = false;
                if (!_draggingProgress) RefreshPlayback(session);
            }
        }
    }

    private void VolumeSlider_ValueChanged(object sender, RoutedPropertyChangedEventArgs<double> e)
    {
        if (_updatingVolume) return;
        SetMediaVolume((float)(e.NewValue / 100.0));
    }

    private void SetMediaVolume(float next)
    {
        if (_mediaVolumeSession is not { } session) return;
        if (!session.SetVolume(next))
        {
            ReportVolumeFailure();
            return;
        }
        _mediaVolumeLevel = next;
        if (next > 0 && _mediaVolumeMuted)
        {
            if (!session.SetMute(false))
            {
                ReportVolumeFailure();
                return;
            }
            _mediaVolumeMuted = false;
        }
        RenderVolume();
    }

    private void MuteButton_Click(object sender, RoutedEventArgs e)
    {
        var session = _mediaVolumeSession;
        if (session is null) return;
        var nextMuted = !_mediaVolumeMuted;
        if (session.SetMute(nextMuted))
        {
            _mediaVolumeMuted = nextMuted;
            RenderVolume();
        }
        else ReportVolumeFailure();
    }

    private void Volume_MouseWheel(object sender, MouseWheelEventArgs e)
    {
        if (_mediaVolumeSession is null) return;
        SetMediaVolume(Math.Clamp(_mediaVolumeLevel + Math.Sign(e.Delta) * 0.02f, 0f, 1f));
        e.Handled = true;
    }

    private void ReportVolumeFailure()
    {
        BindMediaVolumeSession();
        RenderVolume();
        Report(UiText.Get("ErrorVolumeChange"));
    }

    private async Task<bool> RunAsync(Func<MediaSession, IAsyncOperation<bool>> command)
    {
        if (_session is null) return false;
        try
        {
            if (await command(_session)) return true;
        }
        catch (Exception ex) when (ex is COMException or InvalidOperationException)
        {
            AppLog.Warning($"Media command failed: {ex.Message}");
        }
        Report(UiText.Get("ErrorMediaControl"));
        return false;
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
