using System.Windows;
using System.Windows.Controls;
using System.Windows.Data;
using System.Windows.Media;
using System.Windows.Media.Animation;
using System.Windows.Media.Imaging;
using OpenTaskbar.Services;

namespace OpenTaskbar;

public partial class MainWindow
{
    private readonly HashSet<FrameworkElement> _currentMagnifiedBuffer = [];
    private readonly List<(FrameworkElement Icon, Point Center)> _magnificationTargets = [];
    private bool _magnificationTargetsDirty = true;
    private readonly Dictionary<FrameworkElement, double> _magnificationScaleTargets = [];
    private bool _magnificationRendering;
    private TimeSpan? _magnificationLastFrame;
    private readonly System.Runtime.CompilerServices.ConditionalWeakTable<Image, BitmapSource> _restingIcons = new();
    private double _targetGlintRelX = 0.5;
    private double _currentGlintRelX = 0.5;
    private bool _glintRendering;
    private bool _glintActive;
    private double _glintCurrentOpacity;

    internal void InvalidateMagnificationTargets() => _magnificationTargetsDirty = true;

    private void RebuildMagnificationTargets()
    {
        _magnificationTargets.Clear();
        foreach (var button in GetAllDockButtons())
        {
            if (GetButtonIcon(button) is not { } icon) continue;
            var center = button.TranslatePoint(new Point(button.ActualWidth / 2, button.ActualHeight / 2), DockContents);
            _magnificationTargets.Add((icon, center));
        }

        _magnificationTargetsDirty = false;
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
        icon.RenderTransformOrigin = new Point(0.5, 0.95);
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

        var mousePos = e.GetPosition(DockContents);
        var mouseX = IsVertical ? mousePos.Y : mousePos.X;
        const double influenceRadius = DockMetrics.InfluenceRadius;
        const double maxScale = DockMetrics.HoverScale;

        if (_magnificationTargetsDirty) RebuildMagnificationTargets();

        _currentMagnifiedBuffer.Clear();
        _magnificationScaleTargets.Clear();

        foreach (var (icon, center) in _magnificationTargets)
        {
            var distance = Math.Abs(mouseX - (IsVertical ? center.Y : center.X));
            var targetScale = 1.0;
            if (distance < influenceRadius)
            {
                var factor = Math.Cos(distance / influenceRadius * (Math.PI / 2));
                targetScale = 1.0 + (maxScale - 1.0) * factor * factor;
            }

            _magnificationScaleTargets[icon] = targetScale;
            if (targetScale > 1.005) _currentMagnifiedBuffer.Add(icon);
        }

        foreach (var icon in _magnifiedIcons)
        {
            if (!_magnificationScaleTargets.ContainsKey(icon))
                _magnificationScaleTargets[icon] = 1.0;
        }

        _magnifiedIcons.Clear();
        foreach (var icon in _currentMagnifiedBuffer)
            _magnifiedIcons.Add(icon);

        if (!_magnificationRendering)
        {
            // Cancel a still-running mouse-leave animation without snapping to its base value.
            foreach (var icon in _magnificationScaleTargets.Keys)
            {
                var (scale, _) = GetIconTransforms(icon);
                var currentX = scale.ScaleX;
                var currentY = scale.ScaleY;
                scale.BeginAnimation(ScaleTransform.ScaleXProperty, null);
                scale.BeginAnimation(ScaleTransform.ScaleYProperty, null);
                scale.ScaleX = currentX;
                scale.ScaleY = currentY;
            }

            _magnificationLastFrame = null;
            CompositionTarget.Rendering += RenderMagnification;
            _magnificationRendering = true;
        }
    }

    private void RenderMagnification(object? sender, EventArgs e)
    {
        var frameTime = e is RenderingEventArgs frame ? frame.RenderingTime : TimeSpan.Zero;
        var elapsedMs = _magnificationLastFrame is { } last
            ? Math.Clamp((frameTime - last).TotalMilliseconds, 1, 50) : 16.7;
        _magnificationLastFrame = frameTime;
        // Frame-rate-independent easing with roughly a 55 ms response time.
        var blend = 1 - Math.Exp(-elapsedMs / 55.0);
        var settled = true;

        foreach (var (icon, targetScale) in _magnificationScaleTargets)
        {
            var (scale, _) = GetIconTransforms(icon);
            icon.RenderTransformOrigin = IconScaleOrigin;
            var current = scale.ScaleX;
            var next = Math.Abs(targetScale - current) < 0.003
                ? targetScale : current + (targetScale - current) * blend;
            if (Math.Abs(targetScale - next) >= 0.003) settled = false;
            if (Math.Abs(current - next) < 0.0001) continue;
            UseHoverIcon(icon, next > HoverIconThreshold);
            scale.ScaleX = next;
            scale.ScaleY = next;
        }

        if (settled) StopMagnificationRendering();
    }

    private void StopMagnificationRendering()
    {
        if (!_magnificationRendering) return;
        CompositionTarget.Rendering -= RenderMagnification;
        _magnificationRendering = false;
        _magnificationLastFrame = null;
    }

    private void StartGlintRendering()
    {
        if (_glintRendering) return;
        _glintRendering = true;
        CompositionTarget.Rendering += RenderGlintPhysics;
    }

    private void StopGlintRendering()
    {
        if (!_glintRendering) return;
        CompositionTarget.Rendering -= RenderGlintPhysics;
        _glintRendering = false;
    }

    private void RenderGlintPhysics(object? sender, EventArgs e)
    {
        var targetOpacity = _glintActive ? 1.0 : 0.0;
        var opacityDiff = targetOpacity - _glintCurrentOpacity;
        if (Math.Abs(opacityDiff) > 0.005)
        {
            _glintCurrentOpacity += opacityDiff * 0.18;
        }
        else
        {
            _glintCurrentOpacity = targetOpacity;
        }

        var posDiff = _targetGlintRelX - _currentGlintRelX;
        if (Math.Abs(posDiff) > 0.0002)
        {
            _currentGlintRelX += posDiff * 0.18;
        }
        else
        {
            _currentGlintRelX = _targetGlintRelX;
        }

        ApplyGlintMetrics(_currentGlintRelX, _glintCurrentOpacity);

        if (!_glintActive && _glintCurrentOpacity <= 0.005)
        {
            _glintCurrentOpacity = 0.0;
            DockDynamicGlintBorder.Opacity = 0.0;
            DockDynamicGlowBorder.Opacity = 0.0;
            StopGlintRendering();
        }
    }

    private void ApplyGlintMetrics(double relX, double opacity)
    {
        var cornerFactor = Math.Clamp(Math.Sin(relX * Math.PI), 0.35, 1.0);
        var effectiveOpacity = opacity * cornerFactor;

        DockDynamicGlintBorder.Opacity = effectiveOpacity;
        DockDynamicGlowBorder.Opacity = effectiveOpacity * 0.85;

        const double halfSpread = 0.12;
        const double innerSpread = 0.05;

        var x0 = Math.Max(0.0, relX - halfSpread);
        var x1 = Math.Max(0.0, relX - innerSpread);
        var x2 = relX;
        var x3 = Math.Min(1.0, relX + innerSpread);
        var x4 = Math.Min(1.0, relX + halfSpread);

        DockSpecularGlintStop0.Offset = x0;
        DockSpecularGlintStop1.Offset = x1;
        DockSpecularGlintStop2.Offset = x2;
        DockSpecularGlintStop3.Offset = x3;
        DockSpecularGlintStop4.Offset = x4;

        var pt = new Point(relX, 0.0);
        DockSpecularGlow.Center = pt;
        DockSpecularGlow.GradientOrigin = pt;
    }

    private void DockSurface_PreviewMouseMove(object sender, System.Windows.Input.MouseEventArgs e)
    {
        var width = DockFrame.ActualWidth;
        if (width <= 0) return;
        var pos = e.GetPosition(DockFrame);
        _targetGlintRelX = Math.Clamp(IsVertical ? (pos.Y / DockFrame.ActualHeight) : (pos.X / width), 0.0, 1.0);
        StartGlintRendering();
    }

    private void DockSurface_MouseEnter(object sender, System.Windows.Input.MouseEventArgs e)
    {
        _glintActive = true;
        var width = DockFrame.ActualWidth;
        if (width > 0)
        {
            var pos = e.GetPosition(DockFrame);
            _targetGlintRelX = Math.Clamp(IsVertical ? (pos.Y / DockFrame.ActualHeight) : (pos.X / width), 0.0, 1.0);
        }
        StartGlintRendering();
        NativeMemoryMethods.CancelIdleTrim();
        _autoHideController?.OnMouseEnterDock();
    }

    private void DockSurface_MouseLeave(object sender, System.Windows.Input.MouseEventArgs e)
    {
        _glintActive = false;
        StartGlintRendering();
        ResetMagnification();
        _autoHideController?.OnMouseLeaveDock();
        NativeMemoryMethods.ScheduleIdleTrim(TimeSpan.FromSeconds(2));
    }

    private void DockEdgeTrigger_MouseEnter(object sender, System.Windows.Input.MouseEventArgs e) => _autoHideController?.OnMouseEnterEdge();
    private void DockEdgeTrigger_MouseLeave(object sender, System.Windows.Input.MouseEventArgs e) => _autoHideController?.OnMouseLeaveEdge();

    private void ResetMagnification()
    {
        StopMagnificationRendering();
        _magnificationScaleTargets.Clear();
        if (_magnifiedIcons.Count == 0) return;
        var duration = _preferences.Current.AnimationsEnabled && SystemParameters.ClientAreaAnimation
            ? TimeSpan.FromMilliseconds(120) : TimeSpan.Zero;
        var easing = new CubicEase { EasingMode = EasingMode.EaseOut };

        foreach (var icon in _magnifiedIcons)
        {
            var (scale, _) = GetIconTransforms(icon);
            var shrink = new DoubleAnimation(1.0, duration) { EasingFunction = easing };
            // Keep the sharp variant while shrinking; the resting bitmap is exact only at scale 1.
            shrink.Completed += (_, _) =>
            {
                if (scale.ScaleX <= HoverIconThreshold) UseHoverIcon(icon, false);
            };
            scale.BeginAnimation(ScaleTransform.ScaleXProperty, shrink);
            scale.BeginAnimation(ScaleTransform.ScaleYProperty, new DoubleAnimation(1.0, duration) { EasingFunction = easing });
        }

        _magnifiedIcons.Clear();
    }

    /// <summary>Scale above which an icon shows its larger variant instead of the 1:1 resting bitmap.</summary>
    private const double HoverIconThreshold = 1.005;

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
