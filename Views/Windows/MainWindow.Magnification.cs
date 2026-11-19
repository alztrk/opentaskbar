using System.Windows;
using System.Windows.Controls;
using System.Windows.Data;
using System.Windows.Media;
using System.Windows.Media.Animation;
using System.Windows.Media.Imaging;
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
    private readonly Dictionary<FrameworkElement, double> _magnificationScaleTargets = [];
    private readonly Dictionary<FrameworkElement, double> _magnificationVelocity = [];
    private bool _magnificationRendering;
    private TimeSpan? _magnificationLastFrame;
    private readonly System.Runtime.CompilerServices.ConditionalWeakTable<Image, BitmapSource> _restingIcons = new();

    internal void InvalidateMagnificationTargets()
    {
        if (_updatingMagnificationLayout || _magnificationLayoutActive) return;
        _magnificationTargetsDirty = true;
    }

    private void RebuildMagnificationTargets()
    {
        _magnificationTargets.Clear();
        if (DockHost is null || !IsLoaded) return;

        var fallbackCell = Profile.Cell;
        foreach (var button in GetAllDockButtons())
        {
            if (button.Visibility != Visibility.Visible) continue;
            if (button.ActualWidth <= 0 || button.ActualHeight <= 0) continue;
            if (!button.IsDescendantOf(DockHost)) continue;
            if (GetButtonIcon(button) is not { } icon) continue;

            var center = button.TranslatePoint(new Point(button.ActualWidth / 2, button.ActualHeight / 2), DockHost);
            var restCenter = IsVertical ? center.Y : center.X;
            var baseSize = IsVertical ? button.ActualHeight : button.ActualWidth;
            if (!double.IsFinite(baseSize) || baseSize <= 0) baseSize = fallbackCell;
            _magnificationTargets.Add(new MagnificationTarget(button, icon, restCenter, baseSize));
        }

        CaptureMagnificationExpansionBudget();
        _magnificationTargetsDirty = false;
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

    private void CaptureMagnificationExpansionBudget()
    {
        var margin = DockSurface.Margin;
        if (IsVertical)
        {
            _canExpandSurface = false;
            _restSurfaceWidth = 0;
            _restScrollMaxWidth = double.PositiveInfinity;
            var availableHeight = _dockArea.Height > 0 ? _dockArea.Height : ActualHeight;
            var maximumSurfaceHeight = Math.Max(1.0, availableHeight - margin.Top - margin.Bottom);
            _availableExpansion = DockScrollViewer.ScrollableHeight <= 0.5 && DockSurface.ActualHeight > 0
                ? Math.Max(0.0, maximumSurfaceHeight - DockSurface.ActualHeight)
                : 0.0;
            UpdateEffectiveMaxScale();
            return;
        }

        var availableWidth = _dockArea.Width > 0 ? _dockArea.Width : ActualWidth;
        var maximumSurfaceWidth = Math.Max(1.0, availableWidth - margin.Left - margin.Right);
        _restSurfaceWidth = !double.IsNaN(DockSurface.Width) && DockSurface.Width > 0
            ? DockSurface.Width
            : DockSurface.ActualWidth;
        _restScrollMaxWidth = DockScrollViewer.MaxWidth;

        if (!IsFullWidth
            && _dockWidthOverride is null
            && DockScrollViewer.ScrollableWidth <= 0.5
            && _restSurfaceWidth > 0
            && _dockArea.Width > 0)
        {
            _canExpandSurface = true;
            _availableExpansion = Math.Max(0.0, maximumSurfaceWidth - _restSurfaceWidth);
        }
        else if (IsFullWidth
            && DockScrollViewer.ScrollableWidth <= 0.5
            && double.IsFinite(_restScrollMaxWidth)
            && DockContents.ActualWidth > 0)
        {
            _canExpandSurface = false;
            _availableExpansion = Math.Max(0.0, _restScrollMaxWidth - DockContents.ActualWidth);
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
        if (icon.RenderTransform is TransformGroup { IsFrozen: false, IsSealed: false } group)
        {
            var s = group.Children.OfType<ScaleTransform>().FirstOrDefault();
            var t = group.Children.OfType<TranslateTransform>().FirstOrDefault();
            if (s is { IsFrozen: false, IsSealed: false } && t is { IsFrozen: false, IsSealed: false })
            {
                return (s, t);
            }
        }

        var newScale = new ScaleTransform();
        var newTranslate = new TranslateTransform();
        var newGroup = new TransformGroup();
        newGroup.Children.Add(newScale);
        newGroup.Children.Add(newTranslate);
        icon.RenderTransform = newGroup;
        icon.RenderTransformOrigin = new Point(0.5, 0.5);
        return (newScale, newTranslate);
    }

    private static FrameworkElement? GetButtonIcon(Button button) =>
        button.Content as FrameworkElement ?? (FrameworkElement?)FindVisualChild<Image>(button) ?? FindVisualChild<System.Windows.Shapes.Path>(button);

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
        foreach (var app in _pinnedApps)
        {
            if (PinnedApps.ItemContainerGenerator.ContainerFromItem(app) is DependencyObject container
                && FindVisualChild<Button>(container) is { } button)
            {
                yield return button;
            }
        }

        foreach (var app in _runningApps)
        {
            if (RunningApps.ItemContainerGenerator.ContainerFromItem(app) is DependencyObject container
                && FindVisualChild<Button>(container) is { } button)
            {
                yield return button;
            }
        }

        foreach (var button in GetExtraDockButtons()) yield return button;
    }

    private bool IsPointerOverNonMagnifiedZone(DependencyObject? source)
    {
        var current = source;
        while (current is not null && !ReferenceEquals(current, DockSurface))
        {
            if (ReferenceEquals(current, WidgetIsland)
                || ReferenceEquals(current, LeftResizeThumb)
                || ReferenceEquals(current, RightResizeThumb)
                || ReferenceEquals(current, ScrollPrevOverlay)
                || ReferenceEquals(current, ScrollNextOverlay))
            {
                return true;
            }

            if (current is FrameworkElement fe)
            {
                foreach (var (token, element) in _dockItemElements)
                {
                    if (token.StartsWith("widget:", StringComparison.OrdinalIgnoreCase) && ReferenceEquals(element, fe))
                    {
                        return true;
                    }
                }
            }

            current = current is Visual or System.Windows.Media.Media3D.Visual3D
                ? VisualTreeHelper.GetParent(current)
                : LogicalTreeHelper.GetParent(current);
        }

        return false;
    }

    private void AnimateActivationFeedback(Button button)
    {
        if (!_preferences.Current.AnimationsEnabled || !SystemParameters.ClientAreaAnimation)
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
        if (!_preferences.Current.MagnificationEnabled || !_preferences.Current.AnimationsEnabled || !SystemParameters.ClientAreaAnimation)
            return;

        if (LeftResizeThumb.IsDragging || RightResizeThumb.IsDragging || DockScrollViewer.IsPanning)
            return;

        if (LeftResizeThumb.IsMouseOver || RightResizeThumb.IsMouseOver
            || ScrollPrevOverlay.IsMouseOver || ScrollNextOverlay.IsMouseOver
            || IsPointerOverNonMagnifiedZone(e.OriginalSource as DependencyObject))
        {
            ResetMagnification();
            return;
        }

        if (_magnificationTargetsDirty && !_magnificationLayoutActive)
        {
            RebuildMagnificationTargets();
        }

        if (_magnificationTargets.Count == 0) return;

        var mousePos = e.GetPosition(DockHost);
        var mouseAxis = IsVertical ? mousePos.Y : mousePos.X;
        var constrained = _availableExpansion <= 0.5;
        var influenceRadius = DockFisheyeLayout.InfluenceRadiusForCell(Profile.Cell, constrained);
        var maxScale = _effectiveMaxScale;

        _currentMagnifiedBuffer.Clear();
        _magnificationScaleTargets.Clear();

        foreach (var target in _magnificationTargets)
        {
            var targetScale = DockFisheyeLayout.ComputeTargetScale(mouseAxis - target.RestCenter, influenceRadius, maxScale);
            _magnificationScaleTargets[target.Icon] = targetScale;
            if (targetScale > HoverIconThreshold) _currentMagnifiedBuffer.Add(target.Icon);
        }

        foreach (var icon in _magnifiedIcons)
        {
            if (!_magnificationScaleTargets.ContainsKey(icon))
                _magnificationScaleTargets[icon] = 1.0;
        }

        _magnifiedIcons.Clear();
        foreach (var icon in _currentMagnifiedBuffer)
            _magnifiedIcons.Add(icon);

        StartMagnificationRendering();
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
        if (_magnificationTargetsDirty && !_magnificationLayoutActive)
        {
            RebuildMagnificationTargets();
        }

        var frameTime = e is RenderingEventArgs frame ? frame.RenderingTime : TimeSpan.Zero;
        var elapsedMs = _magnificationLastFrame is { } last
            ? Math.Clamp((frameTime - last).TotalMilliseconds, 1, 50) : 16.7;
        _magnificationLastFrame = frameTime;

        var dtSeconds = Math.Min(elapsedMs, 32.0) / 1000.0;
        var settled = true;
        var maxScale = _effectiveMaxScale;
        var profile = Profile;
        var availableHeadroom = DockMetrics.Headroom + profile.VerticalPadding;
        var vertical = IsVertical;
        var liftSign = vertical ? (Edge == DockEdge.Left ? 1.0 : -1.0) : -1.0;

        // 1) Advance spring scales and apply icon transform + safe outward lift.
        foreach (var (icon, targetScale) in _magnificationScaleTargets)
        {
            var (scale, translate) = GetIconTransforms(icon);
            icon.RenderTransformOrigin = IconScaleOrigin;
            var current = scale.ScaleX;
            var velocity = _magnificationVelocity.GetValueOrDefault(icon);
            var (next, nextVelocity, iconSettled) = DockFisheyeLayout.StepSpring(current, targetScale, velocity, dtSeconds, maxScale);
            if (!iconSettled) settled = false;

            if (Math.Abs(current - next) >= 0.0001 || iconSettled)
            {
                UseHoverIcon(icon, next > HoverIconThreshold);
                scale.ScaleX = next;
                scale.ScaleY = next;
            }

            if (!translate.HasAnimatedProperties)
            {
                var lift = DockFisheyeLayout.ComputeSafeLift(next, maxScale, profile.Icon, availableHeadroom);
                if (vertical)
                {
                    translate.X = lift * liftSign;
                    translate.Y = 0;
                }
                else
                {
                    translate.X = 0;
                    translate.Y = lift * liftSign;
                }
            }

            _magnificationVelocity[icon] = nextVelocity;
        }

        // 2) Expand button slots (and floating dock frame) proportionally so icons, active indicator dots,
        // click hitboxes, and neighbouring widgets breathe outward smoothly with zero overlap or clipping.
        var count = _magnificationTargets.Count;
        if (count > 0)
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
                if (zIndex > 0) Panel.SetZIndex(target.Button, zIndex);
                else target.Button.ClearValue(Panel.ZIndexProperty);
            }

            var totalActualExtra = DockFisheyeLayout.ComputeSlotExtras(scales, baseSizes, _availableExpansion, extras);
            ApplyMagnificationSlotLayout(extras, totalActualExtra, vertical);
        }

        if (settled)
        {
            var allAtRest = true;
            foreach (var targetScale in _magnificationScaleTargets.Values)
            {
                if (targetScale > 1.001)
                {
                    allAtRest = false;
                    break;
                }
            }

            if (allAtRest)
            {
                ClearMagnificationLayoutOverrides();
                ClearMagnificationZIndexes();
                _magnificationScaleTargets.Clear();
                _magnifiedIcons.Clear();
            }

            StopMagnificationRendering();
        }
    }

    private void ApplyMagnificationSlotLayout(ReadOnlySpan<double> extras, double totalActualExtra, bool vertical)
    {
        if (totalActualExtra > 0.05)
        {
            _updatingMagnificationLayout = true;
            try
            {
                _magnificationLayoutActive = true;
                for (var i = 0; i < _magnificationTargets.Count; i++)
                {
                    var target = _magnificationTargets[i];
                    var slotSize = target.BaseSize + extras[i];
                    if (vertical) target.Button.Height = slotSize;
                    else target.Button.Width = slotSize;
                }

                if (_canExpandSurface && _restSurfaceWidth > 0)
                {
                    DockSurface.BeginAnimation(WidthProperty, null);
                    DockSurface.Width = _restSurfaceWidth + totalActualExtra;
                    if (double.IsFinite(_restScrollMaxWidth))
                    {
                        DockScrollViewer.MaxWidth = _restScrollMaxWidth + totalActualExtra;
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
        if (!_magnificationLayoutActive) return;
        _updatingMagnificationLayout = true;
        try
        {
            foreach (var target in _magnificationTargets)
            {
                target.Button.ClearValue(WidthProperty);
                target.Button.ClearValue(HeightProperty);
            }

            if (_canExpandSurface && _restSurfaceWidth > 0)
            {
                DockSurface.BeginAnimation(WidthProperty, null);
                DockSurface.Width = _restSurfaceWidth;
                if (double.IsFinite(_restScrollMaxWidth))
                {
                    DockScrollViewer.MaxWidth = _restScrollMaxWidth;
                }
            }

            _magnificationLayoutActive = false;
        }
        finally
        {
            _updatingMagnificationLayout = false;
        }
    }

    private void ClearMagnificationZIndexes()
    {
        foreach (var target in _magnificationTargets)
        {
            target.Button.ClearValue(Panel.ZIndexProperty);
        }
    }

    private void StopMagnificationRendering()
    {
        if (!_magnificationRendering) return;
        CompositionTarget.Rendering -= RenderMagnification;
        _magnificationRendering = false;
        _magnificationLastFrame = null;
        _magnificationVelocity.Clear();
    }

    private void DockSurface_PreviewMouseMove(object sender, System.Windows.Input.MouseEventArgs e)
    {
    }

    private void DockSurface_MouseEnter(object sender, System.Windows.Input.MouseEventArgs e)
    {
        NativeMemoryMethods.CancelIdleTrim();
        _autoHideController?.OnMouseEnterDock();
    }

    private void DockSurface_MouseLeave(object sender, System.Windows.Input.MouseEventArgs e)
    {
        ResetMagnification();
        _autoHideController?.OnMouseLeaveDock();
        NativeMemoryMethods.ScheduleIdleTrim(TimeSpan.FromSeconds(2));
    }

    private void DockEdgeTrigger_MouseEnter(object sender, System.Windows.Input.MouseEventArgs e) => _autoHideController?.OnMouseEnterEdge();
    private void DockEdgeTrigger_MouseLeave(object sender, System.Windows.Input.MouseEventArgs e) => _autoHideController?.OnMouseLeaveEdge();

    private void ResetMagnification(bool immediate = false)
    {
        var iconsToReset = _magnificationScaleTargets.Keys
            .Concat(_magnifiedIcons)
            .Concat(_magnificationTargets.Select(t => t.Icon))
            .Distinct()
            .ToList();

        if (immediate)
        {
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
            StopMagnificationRendering();
            ClearMagnificationLayoutOverrides();
            ClearMagnificationZIndexes();
            _magnificationScaleTargets.Clear();
            _magnifiedIcons.Clear();
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
        if ((icon as Image ?? FindVisualChild<Image>(icon)) is not { } image) return;
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
