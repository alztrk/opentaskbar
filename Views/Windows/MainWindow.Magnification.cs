using System.Windows;
using System.Windows.Controls;
using System.Windows.Data;
using System.Windows.Media;
using System.Windows.Media.Animation;
using System.Windows.Media.Imaging;
using System.Windows.Threading;
using OpenTaskbar.Models;
using OpenTaskbar.Services;

namespace OpenTaskbar;

public partial class MainWindow
{
    private readonly record struct MagnificationTarget(Button Button, FrameworkElement Icon, double RestCenter, double BaseSize);

    private readonly HashSet<FrameworkElement> _currentMagnifiedBuffer = [];
    private readonly List<MagnificationTarget> _magnificationTargets = [];
    private bool _magnificationTargetsDirty = true;
    private bool _magnificationLayoutActive;
    private bool _updatingMagnificationLayout;
    private double _restSurfaceWidth;
    private double _restScrollMaxWidth;
    private bool _canExpandSurface;
    private double _availableExpansion;
    private double _effectiveMaxScale = DockMetrics.HoverScale;
    // Frozen for the duration of one hover session so a mid-hover budget recapture cannot make the same hover
    // bloom small first and oversized later (or push icons past the dock edge).
    private double _sessionMaxScale = DockMetrics.HoverScale;
    private readonly Dictionary<FrameworkElement, double> _magnificationScaleTargets = [];
    private readonly Dictionary<FrameworkElement, double> _magnificationVelocity = [];
    private readonly Dictionary<FrameworkElement, bool> _hoverIconStates = [];
    private bool _magnificationRendering;
    private TimeSpan? _magnificationLastFrame;
    private double _lastMagnificationPointerAxis;
    private bool _hasMagnificationPointer;
    private Point _lastDockMousePoint;
    private bool _hasLastDockMousePoint;
    private DispatcherTimer? _magnificationScrollResetTimer;
    private double _magnifiedSpanMin;
    private double _magnifiedSpanMax;
    private readonly List<(double Min, double Max)> _nonMagnifiedRestSpans = [];
    private readonly System.Runtime.CompilerServices.ConditionalWeakTable<Image, BitmapSource> _restingIcons = new();

    // Layout phases. The bool fields below are kept as the public surface other partials read, but all
    // writes go through the guards so there is one explicit "measure -> decide -> apply -> verify" chain.
    private enum MagnificationLayoutPhase
    {
        Rest,               // no overrides; rest geometry is safe to sample
        ApplyingOverrides,  // overrides installed/being written; re-entrant measurement must stay out
        ClearingOverrides   // overrides being restored to their dynamic styles
    }
    private MagnificationLayoutPhase _layoutPhase = MagnificationLayoutPhase.Rest;
    private bool _scrollGestureScopeActive;
    private bool _deferredResizeScheduled;

    private bool MagnificationLayoutBusy => _magnificationLayoutActive || _updatingMagnificationLayout;
    private bool IsMagnificationRest => !_magnificationLayoutActive && !_magnificationRendering;
    private bool CanRecaptureRestGeometry => IsMagnificationRest && !_updatingMagnificationLayout;
    private bool CanInvalidateTargets(bool allowWhileRendering) =>
        !MagnificationLayoutBusy && (allowWhileRendering || !_magnificationRendering);
    private bool CanRebuildTargets => !_magnificationLayoutActive;

    private void EnterLayoutPhase(MagnificationLayoutPhase phase, bool active, bool updating)
    {
        if (_layoutPhase != phase)
            ShellTelemetry.Event("MagnificationLayout", ("phase", phase), ("active", active), ("updating", updating));
        _layoutPhase = phase;
        _magnificationLayoutActive = active;
        _updatingMagnificationLayout = updating;
    }

    /// <summary>
    /// Holds a scroll gesture scope from ScrollChanged until the deferred resize triggered by the
    /// magnification reset completes. FitDockToContent's content-overflow signal cannot reset the strip
    /// inside this scope, which breaks the ScrollChanged -> reset -> resize -> reset feedback loop.
    /// </summary>
    internal void BeginScrollGestureScope()
    {
        if (_scrollGestureScopeActive) return;
        _scrollGestureScopeActive = true;
        DockScrollViewer.BeginGesture();
    }

    private bool CanEndScrollGestureScope => _scrollGestureScopeActive && !_deferredResizeScheduled;

    internal void EndScrollGestureScope(string reason)
    {
        if (!CanEndScrollGestureScope) return;
        _scrollGestureScopeActive = false;
        DockScrollViewer.EndGesture();
        ShellTelemetry.Event("ScrollLayout", ("phase", "gesture-end"), ("reason", reason),
            ("offset", DockScrollViewer.HorizontalOffset), ("target", DockScrollViewer.DesiredHorizontalOffset));
    }

    /// <summary>Runs at most one deferred resize per gesture, then closes the gesture scope.</summary>
    private void FlushDeferredDockResize(string reason)
    {
        if (!_deferredDockResize || _deferredResizeScheduled) return;
        // Never start another resize from inside one: that is the feedback loop being broken here.
        if (_resizingDock || MagnificationLayoutBusy || _magnificationRendering) return;
        _deferredDockResize = false;
        _deferredResizeScheduled = true;
        var before = DockScrollViewer.HorizontalOffset;
        ShellTelemetry.Event("ScrollLayout", ("phase", "resize-flush"), ("reason", reason), ("before", before));
        Dispatcher.BeginInvoke(DispatcherPriority.Loaded, new Action(() =>
        {
            _deferredResizeScheduled = false;
            ResizeDockToContent();
            ShellTelemetry.Event("ScrollLayout", ("phase", "resize-done"), ("reason", reason),
                ("before", before), ("after", DockScrollViewer.HorizontalOffset),
                ("target", DockScrollViewer.DesiredHorizontalOffset));
            EndScrollGestureScope("deferred-resize-complete");
        }));
    }

    // Cache the transform pair per icon so the per-frame render loop never runs an
    // OfType/FirstOrDefault LINQ walk after the first frame.
    private sealed class IconTransforms(ScaleTransform scale, TranslateTransform translate, TransformGroup group)
    {
        public ScaleTransform Scale { get; } = scale;
        public TranslateTransform Translate { get; } = translate;
        public TransformGroup Group { get; } = group;
    }
    private static readonly System.Runtime.CompilerServices.ConditionalWeakTable<FrameworkElement, IconTransforms> s_iconTransforms = new();
    private readonly Dictionary<Button, int> _zIndexCache = [];
    private readonly Dictionary<Button, double> _appliedSlotSizes = [];

    /// <summary>Minimum DIP change required before writing layout-affecting magnification properties (#128 Finding #5).</summary>
    internal const double MagnificationLayoutDeltaThreshold = 0.25;

    /// <summary>Skips sub-0.0001 scale writes mid-spring, but always writes the exact target once settled.</summary>
    internal static bool ShouldWriteSpringScale(double current, double next, bool settled) =>
        settled ? current != next : Math.Abs(current - next) >= 0.0001;

    private double _decayStartExtra = double.NaN;
    private double _decayStartDelta;

    /// <summary>
    /// While the pointer is on the dock the frame keeps the whole arch reserve, so a centred dock does not wobble
    /// as icons re-centre under the pointer (#127). Once the pointer has left, the springs only decay; the frame
    /// then follows the content back to rest instead of holding the reserve and snapping by its full width on the
    /// last frame.
    /// </summary>
    private double DecayingSurfaceDelta(double reservedDelta, double totalActualExtra)
    {
        if (_hasMagnificationPointer)
        {
            _decayStartExtra = double.NaN;
            return reservedDelta;
        }
        if (double.IsNaN(_decayStartExtra))
        {
            _decayStartExtra = totalActualExtra;
            _decayStartDelta = reservedDelta;
        }
        return ScaleDecayingSurfaceDelta(_decayStartDelta, _decayStartExtra, totalActualExtra);
    }

    /// <summary>
    /// Shrinks the reserved frame delta in proportion to the remaining slot expansion, never below the expansion
    /// the slots still need, so magnified icons are not clipped while they settle.
    /// </summary>
    internal static double ScaleDecayingSurfaceDelta(double startDelta, double startExtra, double currentExtra)
    {
        if (!double.IsFinite(startDelta) || startDelta <= 0.0) return 0.0;
        if (!double.IsFinite(startExtra) || startExtra <= 0.0 || !double.IsFinite(currentExtra)) return startDelta;
        var proportional = startDelta * Math.Clamp(currentExtra / startExtra, 0.0, 1.0);
        return Math.Max(proportional, Math.Clamp(currentExtra, 0.0, startDelta));
    }

    internal static bool ShouldApplyLayoutDelta(double current, double desired, double threshold = MagnificationLayoutDeltaThreshold) =>
        !double.IsFinite(current) || Math.Abs(current - desired) >= threshold;

    /// <summary>
    /// Decides whether a button slot needs a layout write on the current frame. Mid-spring frames skip deltas
    /// below 0.25 DIP, while zero-extra decay (returning to rest) and the final settled frame bypass the
    /// threshold so buttons never remain stuck up to 0.24 DIP off their exact target or rest width.
    /// </summary>
    internal static bool ShouldApplySlotLayoutWrite(
        double appliedOrCurrent,
        double desired,
        double extra,
        bool hasOverride,
        bool settled,
        double threshold = MagnificationLayoutDeltaThreshold)
    {
        if (extra <= 0.001) return hasOverride;
        if (settled)
        {
            return !hasOverride || !double.IsFinite(appliedOrCurrent) || Math.Abs(appliedOrCurrent - desired) >= 0.001;
        }
        return hasOverride
            ? ShouldApplyLayoutDelta(appliedOrCurrent, desired, threshold)
            : extra >= threshold;
    }
    // Threshold crossings call UseHoverIcon repeatedly for the same icon; avoid walking the visual
    // tree each time by remembering the Image element found for it.
    private static readonly System.Runtime.CompilerServices.ConditionalWeakTable<FrameworkElement, Image> s_hoverImageCache = new();

    internal void InvalidateMagnificationTargets() => InvalidateMagnificationTargets(allowWhileRendering: false);

    /// <summary>
    /// Scroll is a discrete layout change, so its late layout signal must still be able to correct the expansion
    /// budget even while the fisheye render loop is running. SizeChanged stays suppressed: slot writes fire it
    /// every frame and rebuilding from mid-spring geometry is wasted work and a visible hitch.
    /// </summary>
    internal void InvalidateMagnificationTargetsAfterScroll() => InvalidateMagnificationTargets(allowWhileRendering: true);

    private void InvalidateMagnificationTargets(bool allowWhileRendering)
    {
        if (!CanInvalidateTargets(allowWhileRendering)) return;
        _magnificationTargetsDirty = true;
    }

    private void RebuildMagnificationTargets()
    {
        // Only reuse previously captured rest geometry while slots/frame are actively expanded; once at rest,
        // always re-measure so dock resizes, scrolling, app changes, or edge switches never leave stale centers.
        var atRest = IsMagnificationRest;
        Dictionary<Button, MagnificationTarget>? previousRest = null;
        if (!atRest)
        {
            previousRest = new Dictionary<Button, MagnificationTarget>(_magnificationTargets.Count);
            foreach (var target in _magnificationTargets) previousRest[target.Button] = target;
        }

        _magnificationTargets.Clear();
        if (DockHost is null || !IsLoaded) return;

        var fallbackCell = Profile.Cell;
        // Cheap guards go into the sequence filter; the visual-tree lookup (GetButtonIcon) stays in the
        // body so it only runs for buttons that already passed every cheap check. Native shell buttons
        // are filtered here too, saving that lookup entirely for them.
        foreach (var button in GetAllDockButtons().Where(b =>
            b.Visibility == Visibility.Visible
            && b.ActualWidth > 0 && b.ActualHeight > 0
            && b.IsDescendantOf(DockHost)
            && !IsNativeShellButton(b)))
        {
            if (GetButtonIcon(button) is not { } icon) continue;

            if (previousRest is not null && previousRest.TryGetValue(button, out var known) && ReferenceEquals(known.Icon, icon))
            {
                _magnificationTargets.Add(known);
                continue;
            }

            var center = button.TranslatePoint(new Point(button.ActualWidth / 2, button.ActualHeight / 2), DockHost);
            var restCenter = IsVertical ? center.Y : center.X;
            var baseSize = IsVertical ? button.ActualHeight : button.ActualWidth;
            if (!double.IsFinite(baseSize) || baseSize <= 0) baseSize = fallbackCell;
            _magnificationTargets.Add(new MagnificationTarget(button, icon, restCenter, baseSize));
        }

        // Never sample the expansion budget from geometry that magnification itself has grown or an in-flight
        // width animation is still moving; that feedback is what made the same hover bloom to different sizes.
        if (atRest)
        {
            CaptureMagnifiedRestBounds();
            CaptureMagnificationExpansionBudget();
            if (_hasMagnificationPointer) UpdateMagnificationTargetsForPointer(_lastMagnificationPointerAxis);
        }
        _magnificationTargetsDirty = false;
    }

    /// <summary>
    /// Captures the resting axis span of magnified targets and every non-magnified control (native shell buttons
    /// and widgets) in <see cref="DockHost"/> coordinates. Deciding no-magnification zones from resting geometry
    /// instead of live hit-testing prevents layout expansion from sliding a shell button under a stationary
    /// pointer and causing a magnify/reset oscillation loop (#90).
    /// </summary>
    private void CaptureMagnifiedRestBounds()
    {
        _nonMagnifiedRestSpans.Clear();
        if (_magnificationTargets.Count == 0)
        {
            _magnifiedSpanMin = double.PositiveInfinity;
            _magnifiedSpanMax = double.NegativeInfinity;
            return;
        }

        var min = double.PositiveInfinity;
        var max = double.NegativeInfinity;
        foreach (var target in _magnificationTargets)
        {
            var half = target.BaseSize / 2.0;
            var left = target.RestCenter - half;
            var right = target.RestCenter + half;
            if (left < min) min = left;
            if (right > max) max = right;
        }
        _magnifiedSpanMin = min;
        _magnifiedSpanMax = max;

        void AddNonMagnifiedSpan(FrameworkElement? element)
        {
            if (element is null || element.Visibility != Visibility.Visible
                || element.ActualWidth <= 0 || element.ActualHeight <= 0
                || !element.IsDescendantOf(DockHost))
            {
                return;
            }

            var topLeft = element.TranslatePoint(new Point(0, 0), DockHost);
            var start = IsVertical ? topLeft.Y : topLeft.X;
            var length = IsVertical ? element.ActualHeight : element.ActualWidth;
            if (double.IsFinite(start) && double.IsFinite(length) && length > 0)
            {
                _nonMagnifiedRestSpans.Add((start, start + length));
            }
        }

        foreach (var button in GetAllDockButtons().Where(IsNativeShellButton))
        {
            AddNonMagnifiedSpan(button);
        }
        foreach (var (token, element) in _dockItemElements)
        {
            if (token.StartsWith("widget:", StringComparison.OrdinalIgnoreCase)) AddNonMagnifiedSpan(element);
        }
        AddNonMagnifiedSpan(PinnedTrayStrip);
        AddNonMagnifiedSpan(WidgetIsland);
    }

    private bool IsPointerOverMagnifiedTarget(DependencyObject? source)
    {
        var current = source;
        while (current is not null && !ReferenceEquals(current, DockSurface))
        {
            if (current is Button button)
            {
                for (var i = 0; i < _magnificationTargets.Count; i++)
                {
                    if (ReferenceEquals(_magnificationTargets[i].Button, button)) return true;
                }
                return false;
            }

            current = current is Visual or System.Windows.Media.Media3D.Visual3D
                ? VisualTreeHelper.GetParent(current)
                : LogicalTreeHelper.GetParent(current);
        }

        return false;
    }

    private bool IsAxisInNonMagnifiedRestSpan(double axis)
    {
        for (var i = 0; i < _nonMagnifiedRestSpans.Count; i++)
        {
            var (min, max) = _nonMagnifiedRestSpans[i];
            if (axis >= min && axis <= max) return true;
        }
        return false;
    }

    /// <summary>
    /// Rebuilds the spring goals from the last known pointer position. Called on mouse move and whenever the
    /// cap/radius changes, so a stationary pointer still gets the peak scale that is currently affordable.
    /// </summary>
    private void UpdateMagnificationTargetsForPointer(double pointerAxis)
    {
        if (_magnificationTargets.Count == 0) return;
        var fraction = DockFisheyeLayout.ComputeExpansionFraction(
            Profile.Cell, DockMetrics.HoverScale, _availableExpansion);
        var radius = DockFisheyeLayout.InfluenceRadiusForExpansion(Profile.Cell, fraction);

        _currentMagnifiedBuffer.Clear();
        _magnificationScaleTargets.Clear();
        foreach (var target in _magnificationTargets)
        {
            var scale = DockFisheyeLayout.ComputeTargetScale(pointerAxis - target.RestCenter, radius, _sessionMaxScale);
            _magnificationScaleTargets[target.Icon] = scale;
            if (scale > HoverIconThreshold) _currentMagnifiedBuffer.Add(target.Icon);
        }

        foreach (var icon in _magnifiedIcons)
        {
            _magnificationScaleTargets.TryAdd(icon, 1.0);
        }
        _magnifiedIcons.Clear();
        foreach (var icon in _currentMagnifiedBuffer) _magnifiedIcons.Add(icon);
    }

    /// <summary>Live peak magnification scale coupled to available slot expansion room.</summary>
    internal double CurrentMagnificationMaxScale => _effectiveMaxScale;

    private void UpdateEffectiveMaxScale()
    {
        var profile = Profile;
        var appIconSize = TryFindResource("DockApplicationIconSize") as double? ?? profile.Icon;
        var visualIconSize = Math.Max(profile.Icon, appIconSize);
        _effectiveMaxScale = DockFisheyeLayout.ComputeEffectiveMaxScale(
            profile.Cell, visualIconSize, DockMetrics.HoverScale, _availableExpansion);
    }

    /// <summary>Samples the un-magnified surface width only from a rest-phase layout pass.</summary>
    private double CaptureRestSurfaceWidth()
    {
        if (MagnificationLayoutBusy) return _restSurfaceWidth;
        return !double.IsNaN(DockSurface.Width) && DockSurface.Width > 0 ? DockSurface.Width : DockSurface.ActualWidth;
    }

    private void CaptureMagnificationExpansionBudget()
    {
        // Resting geometry must be sampled while the dock is at rest, not while slots are expanded.
        if (!CanRecaptureRestGeometry) return;
        var margin = DockSurface.Margin;
        if (IsVertical)
        {
            _canExpandSurface = false;
            _restSurfaceWidth = 0;
            _restScrollMaxWidth = double.PositiveInfinity;
            var availableHeight = _dockArea.Height > 0 ? _dockArea.Height : GetDockDisplay().Height;
            var maximumSurfaceHeight = Math.Max(1.0, availableHeight - margin.Top - margin.Bottom);
            _availableExpansion = DockScrollViewer.ScrollableHeight <= 0.5 && DockSurface.ActualHeight > 0
                ? Math.Max(0.0, maximumSurfaceHeight - DockSurface.ActualHeight)
                : 0.0;
            UpdateEffectiveMaxScale();
            return;
        }

        var availableWidth = _dockArea.Width > 0 ? _dockArea.Width : GetDockDisplay().Width;
        var maximumSurfaceWidth = Math.Max(1.0, availableWidth - margin.Left - margin.Right);
        // Read local Width only when no magnification override is active. A locally set Width is the expanded
        // value while the fisheye runs; capturing it as "rest" shrinks the budget on the next hover and makes the
        // peak scale drift between hovers. ActualWidth is the layout width and is safe only after overrides clear.
        _restSurfaceWidth = CaptureRestSurfaceWidth();
        _restScrollMaxWidth = MagnificationLayoutBusy ? _restScrollMaxWidth : DockScrollViewer.MaxWidth;

        // Subtract real overflow instead of treating any 1 px as "no room at all": the peak scale now degrades
        // gradually with free space rather than flipping between 1.1x and 1.5x (#127).
        var overflow = DockScrollViewer.ScrollableWidth;
        if (!_preferences.Current.MagnificationExpandsDock)
        {
            // Fixed frame: the dock keeps its rest width and icons grow in place up to the no-overlap scale.
            _canExpandSurface = false;
            _availableExpansion = 0.0;
        }
        else if (!IsFullWidth
            && _dockWidthOverride is null
            && _restSurfaceWidth > 0
            && _dockArea.Width > 0)
        {
            _canExpandSurface = true;
            _availableExpansion = DockFisheyeLayout.FreeExpansion(maximumSurfaceWidth, _restSurfaceWidth, overflow);
        }
        else if ((IsFullWidth || _dockWidthOverride is not null)
            && double.IsFinite(_restScrollMaxWidth)
            && DockContents.ActualWidth > 0)
        {
            _canExpandSurface = false;
            _availableExpansion = DockFisheyeLayout.FreeExpansion(_restScrollMaxWidth, DockContents.ActualWidth, overflow);
        }
        else
        {
            _canExpandSurface = false;
            _availableExpansion = 0.0;
        }

        UpdateEffectiveMaxScale();
    }

    internal static (ScaleTransform Scale, TranslateTransform Translate) GetIconTransforms(FrameworkElement icon)
    {
        if (s_iconTransforms.TryGetValue(icon, out var cached)
            && !cached.Scale.IsFrozen && !cached.Scale.IsSealed
            && !cached.Translate.IsFrozen && !cached.Translate.IsSealed
            && ReferenceEquals(icon.RenderTransform, cached.Group))
        {
            return (cached.Scale, cached.Translate);
        }

        ScaleTransform newScale;
        TranslateTransform newTranslate;
        TransformGroup group;
        if (icon.RenderTransform is TransformGroup { IsFrozen: false, IsSealed: false } existing
            && existing.Children.Count == 2
            && existing.Children[0] is ScaleTransform es
            && existing.Children[1] is TranslateTransform et
            && !es.IsFrozen && !es.IsSealed && !et.IsFrozen && !et.IsSealed)
        {
            newScale = es;
            newTranslate = et;
            group = existing;
        }
        else
        {
            newScale = new ScaleTransform();
            newTranslate = new TranslateTransform();
            group = new TransformGroup();
            group.Children.Add(newScale);
            group.Children.Add(newTranslate);
            icon.RenderTransform = group;
            icon.RenderTransformOrigin = new Point(0.5, 0.5);
        }

        s_iconTransforms.AddOrUpdate(icon, new IconTransforms(newScale, newTranslate, group));
        return (newScale, newTranslate);
    }

    private static FrameworkElement? GetButtonIcon(Button button) =>
        button.Content as FrameworkElement ?? (FrameworkElement?)FindVisualChild<Image>(button) ?? FindVisualChild<System.Windows.Shapes.Path>(button);

    /// <summary>
    /// Buttons that launch Windows-owned UI. Uses both direct references (fallback dock order) and the free-form
    /// order tokens, so a shell button keeps behaving the same after drag-and-drop reordering.
    /// </summary>
    private bool IsNativeShellButton(Button button)
    {
        if (ReferenceEquals(button, StartButton) || ReferenceEquals(button, SearchButton)
            || ReferenceEquals(button, TaskViewButton) || ReferenceEquals(button, TrayButton)
            || ReferenceEquals(button, ShowDesktopButton) || ReferenceEquals(button, RecycleBinButton))
        {
            return true;
        }

        foreach (var (token, element) in _dockItemElements)
        {
            if (!ShellDockTokens.Contains(token, StringComparer.OrdinalIgnoreCase)) continue;
            if (ReferenceEquals(element, button)
                || ReferenceEquals(FindVisualChild<Button>(element), button))
            {
                return true;
            }
        }
        return false;
    }

    private IEnumerable<Button> GetAllDockButtons()
    {
        if (_dockItemElements.Count > 0)
        {
            foreach (var (token, element) in _dockItemElements)
            {
                if (token.StartsWith("widget:", StringComparison.OrdinalIgnoreCase)) continue;
                if (element is Button btn)
                {
                    yield return btn;
                }
                else if (FindVisualChild<Button>(element) is { } button)
                {
                    yield return button;
                }
            }
            yield break;
        }

        yield return StartButton;

        // Profiles filter the pinned view, so look containers up by item rather than by index.
        foreach (var button in _pinnedApps
            .Select(app => PinnedApps.ItemContainerGenerator.ContainerFromItem(app) is DependencyObject container
                ? FindVisualChild<Button>(container)
                : null)
            .OfType<Button>())
        {
            yield return button;
        }

        foreach (var button in _runningApps
            .Select(app => RunningApps.ItemContainerGenerator.ContainerFromItem(app) is DependencyObject container
                ? FindVisualChild<Button>(container)
                : null)
            .OfType<Button>())
        {
            yield return button;
        }

        foreach (var button in GetExtraDockButtons()) yield return button;
    }

    private void AnimateActivationFeedback(Button button)
    {
        if (!_preferences.Current.EffectiveAnimationsEnabled || !SystemParameters.ClientAreaAnimation)
        {
            return;
        }

        var icon = GetButtonIcon(button);
        if (icon is null) return;
        var (_, translate) = GetIconTransforms(icon);

        var bounce = new DoubleAnimationUsingKeyFrames
        {
            Duration = TimeSpan.FromMilliseconds(DockMetrics.LaunchBounceMs),
            KeyFrames =
            {
                new EasingDoubleKeyFrame(0, KeyTime.FromTimeSpan(TimeSpan.Zero)),
                new EasingDoubleKeyFrame(-DockMetrics.LaunchBounceHeight * BounceDirection, KeyTime.FromTimeSpan(TimeSpan.FromMilliseconds(120)),
                    new CubicEase { EasingMode = EasingMode.EaseOut }),
                new EasingDoubleKeyFrame(0, KeyTime.FromTimeSpan(TimeSpan.FromMilliseconds(240)),
                    new CubicEase { EasingMode = EasingMode.EaseIn }),
                new EasingDoubleKeyFrame(-5 * BounceDirection, KeyTime.FromTimeSpan(TimeSpan.FromMilliseconds(330)),
                    new CubicEase { EasingMode = EasingMode.EaseOut }),
                new EasingDoubleKeyFrame(0, KeyTime.FromTimeSpan(TimeSpan.FromMilliseconds(DockMetrics.LaunchBounceMs)),
                    new CubicEase { EasingMode = EasingMode.EaseIn })
            }
        };
        translate.BeginAnimation(IsVertical ? TranslateTransform.XProperty : TranslateTransform.YProperty, bounce);
    }

    private void DockSurface_MouseMove(object sender, System.Windows.Input.MouseEventArgs e)
    {
        if (!_preferences.Current.MagnificationEnabled || !_preferences.Current.EffectiveAnimationsEnabled || !SystemParameters.ClientAreaAnimation)
            return;

        if (LeftResizeThumb.IsDragging || RightResizeThumb.IsDragging || DockScrollViewer.IsPanning)
            return;

        var mousePos = e.GetPosition(DockHost);
        // Layout expansion shifts non-magnified buttons (Recycle Bin, Start, Search, widgets) underneath a
        // stationary pointer, causing WPF to emit synthetic MouseMove events at the same DockHost coordinates.
        // Ignore those layout-driven moves so a sliding button cannot trigger a magnify/reset oscillation loop (#90).
        if (_hasLastDockMousePoint
            && Math.Abs(mousePos.X - _lastDockMousePoint.X) < 0.5
            && Math.Abs(mousePos.Y - _lastDockMousePoint.Y) < 0.5)
        {
            return;
        }
        _lastDockMousePoint = mousePos;
        _hasLastDockMousePoint = true;
        UpdateMagnificationForPoint(mousePos, e.OriginalSource as DependencyObject);
    }

    /// <summary>
    /// Re-arms magnification at a known pointer position. Shared by real MouseMove and by the post-scroll restart,
    /// which must bypass the same-coordinate guard that the scroll reset deliberately re-arms.
    /// </summary>
    private void UpdateMagnificationForPoint(Point mousePos, DependencyObject? source = null)
    {
        if (LeftResizeThumb.IsMouseOver || RightResizeThumb.IsMouseOver
            || ScrollPrevOverlay.IsMouseOver || ScrollNextOverlay.IsMouseOver)
        {
            ResetMagnification();
            return;
        }

        if (_magnificationTargetsDirty && CanRebuildTargets)
        {
            RebuildMagnificationTargets();
        }

        var mouseAxis = IsVertical ? mousePos.Y : mousePos.X;
        var overMagnifiedTarget = _magnificationLayoutActive && IsPointerOverMagnifiedTarget(source);
        // While expanded, the centred dock shifts content by ~delta/2 and edge icons grow past their rest slot.
        // Test against the *expanded* span with hysteresis so the pointer over the visibly magnified first/last
        // app is not treated as outside and reset. Non-magnified spans stay rest-based (#90).
        var spanMargin = 0.0;
        if (_magnificationLayoutActive)
        {
            var surfaceDelta = _restSurfaceWidth > 0 && double.IsFinite(DockSurface.ActualWidth)
                ? Math.Max(0.0, DockSurface.ActualWidth - _restSurfaceWidth)
                : 0.0;
            spanMargin = surfaceDelta / 2.0
                + Math.Max(0.0, Profile.Icon * (_sessionMaxScale - 1.0)) / 2.0
                + 4.0;
        }
        if (_magnificationTargets.Count == 0
            || (!overMagnifiedTarget && (mouseAxis < _magnifiedSpanMin - spanMargin
                || mouseAxis > _magnifiedSpanMax + spanMargin
                || IsAxisInNonMagnifiedRestSpan(mouseAxis))))
        {
            ResetMagnification();
            return;
        }
        if (!_hasMagnificationPointer) _sessionMaxScale = _effectiveMaxScale;
        _hasMagnificationPointer = true;
        _lastMagnificationPointerAxis = mouseAxis;
        UpdateMagnificationTargetsForPointer(mouseAxis);

        StartMagnificationRendering();
    }

    /// <summary>
    /// Smooth scrolling raises ScrollChanged once per rendering frame. Resetting on every frame re-arms the
    /// same-coordinate MouseMove guard and leaves the fisheye stalled after the final frame until the user clicks.
    /// Wait for the gesture to settle, then re-arm from the last known pointer position.
    /// </summary>
    internal void ResetMagnificationAfterScroll()
    {
        BeginScrollGestureScope();
        _magnificationScrollResetTimer ??= CreateScrollResetTimer();
        _magnificationScrollResetTimer.Stop();
        _magnificationScrollResetTimer.Start();
    }

    private DispatcherTimer CreateScrollResetTimer()
    {
        var timer = new DispatcherTimer(DispatcherPriority.Loaded) { Interval = TimeSpan.FromMilliseconds(90) };
        timer.Tick += ScrollResetTimer_Tick;
        return timer;
    }

    private void ScrollResetTimer_Tick(object? sender, EventArgs e)
    {
        _magnificationScrollResetTimer?.Stop();
        var point = _lastDockMousePoint;
        var hadPoint = _hasLastDockMousePoint;
        // A real mouse move since the last scroll frame already restarted the effect; don't interrupt it.
        if (!DockFisheyeLayout.ShouldResumeAfterScroll(_hasMagnificationPointer, _magnificationRendering, hadPoint))
        {
            EndScrollGestureScope("mouse-move-resumed");
            return;
        }

        ShellTelemetry.Event("ScrollLayout", ("phase", "magnification-reset"), ("gesture", DockScrollViewer.IsGestureActive));
        ResetMagnification(immediate: true);
        FlushDeferredDockResize("scroll-gesture-settled");
        EndScrollGestureScope("magnification-reset-settled");
        _lastDockMousePoint = point;
        _hasLastDockMousePoint = hadPoint;

        // Restoring the slot widths is only a DP write; let WPF arrange that rest geometry before re-sampling
        // the expansion budget. Sampling in the same frame can latch a still-overflowed (zero) budget and a
        // tiny _sessionMaxScale for the whole hover.
        Dispatcher.BeginInvoke(DispatcherPriority.Loaded, new Action(() =>
        {
            if (!DockFisheyeLayout.ShouldResumeAfterScroll(_hasMagnificationPointer, _magnificationRendering, true)) return;
            if (!_preferences.Current.MagnificationEnabled || !_preferences.Current.EffectiveAnimationsEnabled
                || !SystemParameters.ClientAreaAnimation) return;
            UpdateMagnificationForPoint(point);
        }));
    }

    private void StartMagnificationRendering()
    {
        if (_magnificationRendering) return;

        foreach (var icon in _magnificationScaleTargets.Keys)
        {
            var (scale, _) = GetIconTransforms(icon);
            icon.RenderTransformOrigin = IconScaleOrigin;
            if (scale.HasAnimatedProperties)
            {
                var currentX = scale.ScaleX;
                var currentY = scale.ScaleY;
                scale.BeginAnimation(ScaleTransform.ScaleXProperty, null);
                scale.BeginAnimation(ScaleTransform.ScaleYProperty, null);
                scale.ScaleX = currentX;
                scale.ScaleY = currentY;
            }
        }

        _magnificationLastFrame = null;
        _magnificationVelocity.Clear();
        CompositionTarget.Rendering += RenderMagnification;
        _magnificationRendering = true;
    }

    private void RenderMagnification(object? sender, EventArgs e)
    {
        if (_magnificationTargetsDirty && CanRebuildTargets)
        {
            RebuildMagnificationTargets();
            // The rebuilt targets already use the fresh cap; refresh the spring goals in the same frame.
            if (_hasMagnificationPointer) UpdateMagnificationTargetsForPointer(_lastMagnificationPointerAxis);
        }

        var frameTime = e is RenderingEventArgs frame ? frame.RenderingTime : TimeSpan.Zero;
        var elapsedMs = _magnificationLastFrame is { } last
            ? (frameTime - last).TotalMilliseconds : 16.7;
        _magnificationLastFrame = frameTime;

        var dtSeconds = Math.Clamp(elapsedMs, 1.0, 50.0) / 1000.0;
        var settled = true;
        var anyScaleChanged = false;
        var maxScale = _sessionMaxScale;
        var profile = Profile;
        var availableHeadroom = DockMetrics.Headroom + profile.VerticalPadding;
        var vertical = IsVertical;
        var liftSign = vertical ? (Edge == DockEdge.Left ? 1.0 : -1.0) : -1.0;
        var scaleOrigin = IconScaleOrigin;

        // 1) Advance spring scales and apply icon transform + safe outward lift.
        foreach (var (icon, targetScale) in _magnificationScaleTargets)
        {
            var (scale, translate) = GetIconTransforms(icon);
            // Origin only depends on edge, not the pointer. Writing the same Point every frame is
            // a needless dependency-property invalidation on each icon.
            if (icon.RenderTransformOrigin != scaleOrigin) icon.RenderTransformOrigin = scaleOrigin;
            var current = scale.ScaleX;
            var velocity = _magnificationVelocity.GetValueOrDefault(icon);
            var (next, nextVelocity, iconSettled) = DockFisheyeLayout.StepSpring(current, targetScale, velocity, dtSeconds, maxScale);
            if (!iconSettled) settled = false;

            // Swapping the bitmap is comparatively expensive (visual-tree walk, binding, CWT write); do it only
            // when the icon crosses the magnification threshold, with hysteresis to avoid boundary flicker.
            var wasHover = _hoverIconStates.TryGetValue(icon, out var state) && state;
            var wantHover = wasHover ? next > 1.06 : next > HoverIconThreshold;
            if (wantHover != wasHover)
            {
                _hoverIconStates[icon] = wantHover;
                UseHoverIcon(icon, wantHover);
            }
            // StepSpring returns the exact target with zero velocity once settled. Writing the same ScaleX/Y and
            // lift every frame invalidates ~4 transforms per icon for nothing on a transparent layered window.
            // A settled spring must land exactly on its target. Skipping its last sub-0.0001 step left icons at
            // e.g. 1.00009x, and across a dozen icons that residue kept the slot extras above the clear threshold,
            // so the expanded dock frame stayed wide after the pointer had left.
            if (ShouldWriteSpringScale(current, next, iconSettled))
            {
                scale.ScaleX = next;
                scale.ScaleY = next;
                anyScaleChanged = true;
            }

            if (!translate.HasAnimatedProperties)
            {
                var lift = DockFisheyeLayout.ComputeSafeLift(next, maxScale, profile.Icon, availableHeadroom);
                if (vertical)
                {
                    var targetX = lift * liftSign;
                    if (Math.Abs(translate.X - targetX) >= 0.01) translate.X = targetX;
                    if (translate.Y != 0) translate.Y = 0;
                }
                else
                {
                    if (translate.X != 0) translate.X = 0;
                    var targetY = lift * liftSign;
                    if (Math.Abs(translate.Y - targetY) >= 0.01) translate.Y = targetY;
                }
            }

            _magnificationVelocity[icon] = nextVelocity;
        }

        // 2) Expand button slots (and floating dock frame) proportionally so icons, active indicator dots,
        // click hitboxes, and neighbouring widgets breathe outward smoothly with zero overlap or clipping.
        var count = _magnificationTargets.Count;
        if (count > 0 && (anyScaleChanged || settled || !_magnificationLayoutActive))
        {
            var scales = count <= 64 ? stackalloc double[count] : new double[count];
            var baseSizes = count <= 64 ? stackalloc double[count] : new double[count];
            var extras = count <= 64 ? stackalloc double[count] : new double[count];
            for (var i = 0; i < count; i++)
            {
                var target = _magnificationTargets[i];
                var (scale, _) = GetIconTransforms(target.Icon);
                scales[i] = scale.ScaleX;
                baseSizes[i] = target.BaseSize;
                var zIndex = (int)Math.Round(Math.Max(0.0, scale.ScaleX - 1.0) * 100.0);
                // ZIndex is layout-affecting: rewriting it every frame forces an extra Arrange pass.
                if (_zIndexCache.TryGetValue(target.Button, out var previousZ) && previousZ == zIndex) continue;
                _zIndexCache[target.Button] = zIndex;
                if (zIndex > 0) Panel.SetZIndex(target.Button, zIndex);
                else target.Button.ClearValue(Panel.ZIndexProperty);
            }

            var totalActualExtra = DockFisheyeLayout.ComputeSlotExtras(scales, baseSizes, _availableExpansion, extras);
            ApplyMagnificationSlotLayout(extras, totalActualExtra, vertical, settled);
        }

        if (settled)
        {
            // Detach the per-frame loop once the springs have converged, but KEEP the expanded layout and targets:
            // clearing overrides here (the old allAtRest behaviour) would snap DockSurface.Width back by ~58 px
            // while the icon ScaleTransform stays at 1.5x, causing overlap and a re-bloom on the next MouseMove.
            // StepSpring zeroes velocity on Settled, so the next real MouseMove just re-arms the converged spring.
            // Layout overrides are restored by ResetMagnification when the pointer actually leaves the dock.
            if (!_hasMagnificationPointer)
            {
                _magnificationScaleTargets.Clear();
                _magnifiedIcons.Clear();
                _hoverIconStates.Clear();
            }
            StopMagnificationRendering();
        }
    }

    private void ApplyMagnificationSlotLayout(ReadOnlySpan<double> extras, double totalActualExtra, bool vertical, bool settled)
    {
        if (totalActualExtra > 0.01)
        {
            var canExpandHorizontal = !vertical && _canExpandSurface && _restSurfaceWidth > 0;
            var reservedDelta = canExpandHorizontal
                ? Math.Min(_availableExpansion, DockFisheyeLayout.PeakExpansionNeed(Profile.Cell, _sessionMaxScale))
                : 0.0;
            var surfaceDelta = canExpandHorizontal
                ? DecayingSurfaceDelta(reservedDelta, totalActualExtra)
                : 0.0;
            var surfaceWidth = canExpandHorizontal ? _restSurfaceWidth + surfaceDelta : 0.0;
            var maxScrollWidth = canExpandHorizontal && double.IsFinite(_restScrollMaxWidth)
                ? _restScrollMaxWidth + surfaceDelta
                : double.PositiveInfinity;

            var anyLayoutWriteNeeded = !_magnificationLayoutActive
                || (canExpandHorizontal && (ShouldApplyLayoutDelta(DockSurface.Width, surfaceWidth)
                    || (double.IsFinite(maxScrollWidth) && ShouldApplyLayoutDelta(DockScrollViewer.MaxWidth, maxScrollWidth))));

            if (!anyLayoutWriteNeeded)
            {
                for (var i = 0; i < _magnificationTargets.Count; i++)
                {
                    var target = _magnificationTargets[i];
                    var slotSize = target.BaseSize + extras[i];
                    var hasOverride = _appliedSlotSizes.TryGetValue(target.Button, out var applied);
                    if (ShouldApplySlotLayoutWrite(applied, slotSize, extras[i], hasOverride, settled))
                    {
                        anyLayoutWriteNeeded = true;
                        break;
                    }
                }
            }

            if (!anyLayoutWriteNeeded) return;

            EnterLayoutPhase(MagnificationLayoutPhase.ApplyingOverrides, active: _magnificationLayoutActive, updating: true);
            try
            {
                if (!_magnificationLayoutActive
                    && DockScrollViewer.ScrollableWidth <= 1.0
                    && DockScrollViewer.ScrollableHeight <= 1.0)
                {
                    DockScrollViewer.SuppressOverflowIndicators = true;
                }
                _magnificationLayoutActive = true;

                // Skipping sub-pixel writes (< 0.25 DIP) and untouched unmagnified buttons avoids full-dock
                // WPF relayouts on mid-spring frames, while zero-extra decay and the final settled frame
                // bypass the threshold so buttons always return cleanly to exact rest/target widths (#128 Finding #5).
                for (var i = 0; i < _magnificationTargets.Count; i++)
                {
                    var target = _magnificationTargets[i];
                    var slotSize = target.BaseSize + extras[i];
                    var hasOverride = _appliedSlotSizes.TryGetValue(target.Button, out var applied);
                    if (!ShouldApplySlotLayoutWrite(applied, slotSize, extras[i], hasOverride, settled))
                    {
                        continue;
                    }

                    if (extras[i] <= 0.001)
                    {
                        _appliedSlotSizes.Remove(target.Button);
                        target.Button.ClearValue(WidthProperty);
                        target.Button.ClearValue(HeightProperty);
                        continue;
                    }

                    if (vertical)
                    {
                        target.Button.Height = slotSize;
                    }
                    else
                    {
                        target.Button.Width = slotSize;
                    }
                    _appliedSlotSizes[target.Button] = slotSize;
                }

                if (canExpandHorizontal)
                {
                    // Reserve the whole arch expansion once per hover instead of tracking the live sum of slot
                    // extras: a centred dock re-measures icons under the pointer on every width change, which
                    // produced 18-19 px of wobble and 58 px end snaps (#127). The constant reserve is written
                    // once, so DockShadowLayer's BitmapCache stays warm across the entire hover (#128).
                    if (ShouldApplyLayoutDelta(DockSurface.Width, surfaceWidth))
                    {
                        DockSurface.BeginAnimation(WidthProperty, null);
                        DockSurface.Width = surfaceWidth;
                    }
                    if (double.IsFinite(maxScrollWidth) && ShouldApplyLayoutDelta(DockScrollViewer.MaxWidth, maxScrollWidth))
                    {
                        DockScrollViewer.MaxWidth = maxScrollWidth;
                    }
                }
            }
            finally
            {
                _updatingMagnificationLayout = false;
            }
        }
        else if (_magnificationLayoutActive)
        {
            ClearMagnificationLayoutOverrides();
        }
    }

    /// <summary>
    /// Restores every button's local Width/Height back to its dynamic style resource and returns
    /// DockSurface/DockScrollViewer to their resting widths so layout passes always measure resting geometry.
    /// </summary>
    internal void ClearMagnificationLayoutOverrides()
    {
        _decayStartExtra = double.NaN;
        if (!_magnificationLayoutActive) return;
        EnterLayoutPhase(MagnificationLayoutPhase.ClearingOverrides, active: true, updating: true);
        try
        {
            foreach (var button in _appliedSlotSizes.Keys.ToList())
            {
                button.ClearValue(WidthProperty);
                button.ClearValue(HeightProperty);
            }
            _appliedSlotSizes.Clear();
            foreach (var target in _magnificationTargets)
            {
                target.Button.ClearValue(WidthProperty);
                target.Button.ClearValue(HeightProperty);
            }

            if (_canExpandSurface && _restSurfaceWidth > 0)
            {
                if (ShouldApplyLayoutDelta(DockSurface.Width, _restSurfaceWidth))
                {
                    DockSurface.BeginAnimation(WidthProperty, null);
                    DockSurface.Width = _restSurfaceWidth;
                }
                if (double.IsFinite(_restScrollMaxWidth)
                    && ShouldApplyLayoutDelta(DockScrollViewer.MaxWidth, _restScrollMaxWidth))
                {
                    DockScrollViewer.MaxWidth = _restScrollMaxWidth;
                }
            }

            _magnificationLayoutActive = false;
            _layoutPhase = MagnificationLayoutPhase.Rest;
            // Release overflow suppression after WPF finishes arranging the restored rest widths so
            // ScrollableWidth does not reflect the previous expanded frame and flicker '>' for one frame (#82).
            Dispatcher.BeginInvoke(System.Windows.Threading.DispatcherPriority.Loaded, new Action(() =>
            {
                // Release after WPF arranges the restored rest widths so ScrollableWidth does not
                // reflect the previous expanded frame and flicker '>' for one frame (#82).
                if (!_magnificationLayoutActive) DockScrollViewer.SuppressOverflowIndicators = false;
            }));
        }
        finally
        {
            _updatingMagnificationLayout = false;
        }

        // Once rest geometry is back, a resize deferred by an earlier call during magnification can
        // safely run. Flush is guarded against re-entrancy, so FitDockToContent -> Clear cannot loop.
        FlushDeferredDockResize("layout-overrides-cleared");
    }

    private void ClearMagnificationZIndexes()
    {
        foreach (var button in _zIndexCache.Keys.ToList())
        {
            button.ClearValue(Panel.ZIndexProperty);
        }
        foreach (var target in _magnificationTargets)
        {
            target.Button.ClearValue(Panel.ZIndexProperty);
        }
        _zIndexCache.Clear();
    }

    private void StopMagnificationRendering()
    {
        if (!_magnificationRendering) return;
        CompositionTarget.Rendering -= RenderMagnification;
        _magnificationRendering = false;
        _magnificationLastFrame = null;
        _magnificationVelocity.Clear();
        FlushDeferredDockResize("magnification-stopped");
        EndScrollGestureScope("magnification-stopped");
        ScheduleFloatingLayeredWindowSync();
    }

    private void DockSurface_MouseEnter(object sender, System.Windows.Input.MouseEventArgs e)
    {
        NativeMemoryMethods.CancelIdleTrim();
        _autoHideController?.OnMouseEnterDock();
    }

    private void DockSurface_MouseLeave(object sender, System.Windows.Input.MouseEventArgs e)
    {
        _hasMagnificationPointer = false;
        _hasLastDockMousePoint = false;
        ResetMagnification();
        _autoHideController?.OnMouseLeaveDock();
        NativeMemoryMethods.ScheduleIdleTrim(TimeSpan.FromSeconds(2));
    }

    private void DockEdgeTrigger_MouseEnter(object sender, System.Windows.Input.MouseEventArgs e) => _autoHideController?.OnMouseEnterEdge();
    private void DockEdgeTrigger_MouseLeave(object sender, System.Windows.Input.MouseEventArgs e) => _autoHideController?.OnMouseLeaveEdge();

    private void ResetMagnification(bool immediate = false)
    {
        if (!immediate && !_hasMagnificationPointer && _magnificationRendering
            && _magnificationScaleTargets.Count > 0
            && _magnificationScaleTargets.Values.All(target => target == 1.0))
        {
            return;
        }
        _hasMagnificationPointer = false;
        // Scroll/layout signals call this many times per gesture. Once everything is already at rest there is
        // nothing to clear; avoid rebuilding the reset list and re-arming no-op work on every frame (#127).
        if (!_magnificationLayoutActive
            && _magnificationScaleTargets.Count == 0
            && _magnifiedIcons.Count == 0)
        {
            _hoverIconStates.Clear();
            return;
        }
        var iconsToReset = _magnificationScaleTargets.Keys
            .Concat(_magnifiedIcons)
            .Concat(_magnificationTargets.Select(t => t.Icon))
            .Distinct()
            .ToList();

        if (immediate)
        {
            _hasLastDockMousePoint = false;
            StopMagnificationRendering();
            foreach (var icon in iconsToReset)
            {
                var (scale, translate) = GetIconTransforms(icon);
                scale.BeginAnimation(ScaleTransform.ScaleXProperty, null);
                scale.BeginAnimation(ScaleTransform.ScaleYProperty, null);
                scale.ScaleX = 1.0;
                scale.ScaleY = 1.0;
                if (!translate.HasAnimatedProperties)
                {
                    translate.X = 0;
                    translate.Y = 0;
                }
                UseHoverIcon(icon, false);
            }

            ClearMagnificationLayoutOverrides();
            ClearMagnificationZIndexes();
            _magnificationScaleTargets.Clear();
            _magnifiedIcons.Clear();
            _hoverIconStates.Clear();
            _magnificationTargetsDirty = true;
            return;
        }

        _magnificationScaleTargets.Clear();
        var anyMagnified = _magnificationLayoutActive;
        foreach (var icon in iconsToReset)
        {
            var (scale, _) = GetIconTransforms(icon);
            if (scale.ScaleX > 1.001) anyMagnified = true;
            _magnificationScaleTargets[icon] = 1.0;
        }

        if (!anyMagnified)
        {
            foreach (var icon in iconsToReset) UseHoverIcon(icon, false);
            StopMagnificationRendering();
            ClearMagnificationLayoutOverrides();
            ClearMagnificationZIndexes();
            _magnificationScaleTargets.Clear();
            _magnifiedIcons.Clear();
            _hoverIconStates.Clear();
            return;
        }

        StartMagnificationRendering();
    }

    /// <summary>Scale above which an icon shows its larger variant instead of the 1:1 resting bitmap.</summary>
    private const double HoverIconThreshold = 1.08;

    /// <summary>
    /// Swaps a dock icon between its resting bitmap (drawn 1:1, sharp at scale 1) and a larger native variant
    /// (scaled down while magnified, so it stays sharp instead of blurring). The binding to the resting icon is kept:
    /// the variant is set with SetCurrentValue and the binding is re-read to restore.
    /// </summary>
    private void UseHoverIcon(FrameworkElement icon, bool magnified)
    {
        Image? image;
        if (s_hoverImageCache.TryGetValue(icon, out var cached))
        {
            image = cached;
        }
        else if ((icon as Image ?? FindVisualChild<Image>(icon)) is { } found)
        {
            s_hoverImageCache.AddOrUpdate(icon, found);
            image = found;
        }
        else return;
        if (magnified)
        {
            if (image.Source is not BitmapSource current
                || ApplicationIconProvider.GetHoverVariant(current) is not { } hover) return;
            _restingIcons.AddOrUpdate(image, current);
            image.SetCurrentValue(Image.SourceProperty, hover);
        }
        else if (_restingIcons.TryGetValue(image, out var resting))
        {
            _restingIcons.Remove(image);
            if (BindingOperations.GetBindingExpression(image, Image.SourceProperty) is { } binding) binding.UpdateTarget();
            else image.SetCurrentValue(Image.SourceProperty, resting);
        }
    }
}
