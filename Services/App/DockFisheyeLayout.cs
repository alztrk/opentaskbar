namespace OpenTaskbar.Services;

/// <summary>
/// Pure, deterministic macOS-style dock fisheye mathematics: C1-smooth raised-cosine falloff,
/// critically-damped spring integration, overlap-free scale/slot coupling, and headroom/preview clearance.
/// </summary>
public static class DockFisheyeLayout
{
    /// <summary>
    /// Ratio of cell growth to visual scale delta when the dock has room to expand.
    /// 0.95 gives full-bleed circular and square icons generous breathing room with zero neighbour overlap.
    /// </summary>
    public const double SlotExpansionFactor = 0.95;

    /// <summary>
    /// Subtle extra lift (DIP) away from the screen edge at peak magnification. Kept minimal because
    /// bottom-anchored scaling (RenderTransformOrigin 0.5, 1.0) already grows upward cleanly without
    /// detaching icons from their running-indicator dots.
    /// </summary>
    public const double MaxLiftDips = 1.0;

    /// <summary>Minimum visual gap (DIP) preserved between adjacent icons when slots cannot expand.</summary>
    public const double MinimumNoOverlapGapDips = 4.0;

    /// <summary>Spring stiffness (1/s^2) for magnification bloom and settle.</summary>
    public const double SpringStiffness = 280.0;

    /// <summary>Spring damping (1/s) giving a damping ratio ~0.90: fast, fluid tracking without oscillation.</summary>
    public const double SpringDamping = 30.0;

    /// <summary>Approximate sum of raised-cosine falloff weights across a 5-icon bell arch.</summary>
    private const double FullArchWeightSum = 2.8;

    /// <summary>Influence radius scaled to the active cell size; slightly tighter when slots cannot expand.</summary>
    public static double InfluenceRadiusForCell(double cellSize, bool constrained = false)
    {
        if (!double.IsFinite(cellSize) || cellSize <= 0) return DockMetrics.InfluenceRadius;
        return constrained
            ? Math.Max(95.0, cellSize * 2.1)
            : Math.Max(110.0, cellSize * 2.8);
    }

    /// <summary>
    /// Raised-cosine bell curve: 1 at distance 0, 0 at distance >= radius, zero derivative at both ends
    /// so icons never kink or jump as the pointer enters or crosses item boundaries.
    /// </summary>
    public static double ComputeFalloff(double distance, double radius)
    {
        if (!double.IsFinite(distance) || !double.IsFinite(radius) || radius <= 0) return 0.0;
        var absDistance = Math.Abs(distance);
        if (absDistance >= radius) return 0.0;
        if (absDistance <= 0.0) return 1.0;
        return 0.5 * (1.0 + Math.Cos(Math.PI * absDistance / radius));
    }

    /// <summary>
    /// Maximum peak scale that keeps the hovered icon and its immediate neighbour strictly non-overlapping
    /// when button slots cannot expand (full-width, scrolling, or manually width-locked docks).
    /// </summary>
    public static double ComputeNoOverlapMaxScale(double cellSize, double visualIconSize, double configuredMaxScale)
    {
        if (!double.IsFinite(cellSize) || !double.IsFinite(visualIconSize) || visualIconSize <= 0 || configuredMaxScale <= 1.0)
            return 1.0;

        var constrainedRadius = InfluenceRadiusForCell(cellSize, constrained: true);
        var neighbourFalloff = ComputeFalloff(cellSize, constrainedRadius);
        // Combined half-widths of peak icon (s0) + immediate neighbour (1 + f*(s0-1)) must stay <= cellSize - gap.
        var combinedSlope = 0.5 * (1.0 + neighbourFalloff);
        var margin = Math.Max(0.0, cellSize - MinimumNoOverlapGapDips - visualIconSize);
        var safeDelta = margin / (visualIconSize * combinedSlope);
        return Math.Clamp(1.0 + safeDelta, 1.08, configuredMaxScale);
    }

    /// <summary>
    /// Smoothly couples the peak magnification scale to the available slot expansion budget so icons enjoy
    /// full macOS magnification when the dock can breathe outward, yet never collide when the dock is full.
    /// </summary>
    public static double ComputeEffectiveMaxScale(
        double cellSize, double visualIconSize, double configuredMaxScale, double availableExpansion)
    {
        if (!double.IsFinite(configuredMaxScale) || configuredMaxScale <= 1.0) return 1.0;
        var noOverlapMax = ComputeNoOverlapMaxScale(cellSize, visualIconSize, configuredMaxScale);
        var fraction = ComputeExpansionFraction(cellSize, configuredMaxScale, availableExpansion);
        return noOverlapMax + (configuredMaxScale - noOverlapMax) * fraction;
    }

    /// <summary>How much of the configured magnification the current expansion budget can afford, from 0 to 1.</summary>
    public static double ComputeExpansionFraction(double cellSize, double configuredMaxScale, double availableExpansion)
    {
        if (!double.IsFinite(configuredMaxScale) || configuredMaxScale <= 1.0
            || !double.IsFinite(availableExpansion) || availableExpansion <= 0.0)
        {
            return 0.0;
        }
        var fullExpansionNeed = FullArchWeightSum * (configuredMaxScale - 1.0) * Math.Max(1.0, cellSize) * SlotExpansionFactor;
        return Math.Clamp(availableExpansion / Math.Max(1.0, fullExpansionNeed), 0.0, 1.0);
    }

    /// <summary>
    /// Influence radius derived from the same expansion fraction as the peak scale. The old boolean switch
    /// changed the radius abruptly whenever the budget crossed 0.5 DIP, making an identical hover look wider.
    /// </summary>
    public static double InfluenceRadiusForExpansion(double cellSize, double expansionFraction)
    {
        if (!double.IsFinite(cellSize) || cellSize <= 0) return DockMetrics.InfluenceRadius;
        var constrained = Math.Max(95.0, cellSize * 2.1);
        var free = Math.Max(110.0, cellSize * 2.8);
        return constrained + (free - constrained) * Math.Clamp(expansionFraction, 0.0, 1.0);
    }

    /// <summary>Computes the target magnification scale for an item at a given resting distance from the pointer.</summary>
    public static double ComputeTargetScale(double distance, double radius, double maxScale)
    {
        if (!double.IsFinite(maxScale) || maxScale <= 1.0) return 1.0;
        var falloff = ComputeFalloff(distance, radius);
        return 1.0 + (maxScale - 1.0) * falloff;
    }

    /// <summary>Advances a single scalar spring step with frame-rate independent clamping.</summary>
    public static (double Value, double Velocity, bool Settled) StepSpring(
        double current, double target, double velocity, double dtSeconds, double maxScale)
    {
        // Closed-form damped spring: exact for any dt, so a dropped frame never changes the animation's duration.
        var dt = double.IsFinite(dtSeconds) ? Math.Clamp(dtSeconds, 0.001, 0.05) : 0.016;
        var omega = Math.Sqrt(SpringStiffness);
        var eta = SpringDamping / 2.0;
        var omegaD = Math.Sqrt(Math.Max(1.0, omega * omega - eta * eta));
        var decay = Math.Exp(-eta * dt);
        var cos = Math.Cos(omegaD * dt);
        var sin = Math.Sin(omegaD * dt);
        var offset = current - target;
        var temp = (velocity + eta * offset) / omegaD;
        var next = target + decay * (offset * cos + temp * sin);
        var nextVelocity = decay * (velocity * cos - (omegaD * offset + eta * temp) * sin);
        next = Math.Clamp(next, 1.0, Math.Max(1.0, maxScale) + 0.02);

        if (Math.Abs(target - next) < 0.004 && Math.Abs(nextVelocity) < 0.02)
        {
            return (target, 0.0, true);
        }

        return (next, nextVelocity, false);
    }

    /// <summary>
    /// Distributes desired slot width/height expansions across magnified buttons while strictly respecting
    /// <paramref name="availableExpansion"/> so the dock never overflows the monitor or triggers scrollbars on hover.
    /// </summary>
    public static double ComputeSlotExtras(
        ReadOnlySpan<double> scales,
        ReadOnlySpan<double> baseSizes,
        double availableExpansion,
        Span<double> extrasOut)
    {
        var count = Math.Min(scales.Length, Math.Min(baseSizes.Length, extrasOut.Length));
        var totalDesired = 0.0;
        for (var i = 0; i < count; i++)
        {
            var desired = Math.Max(0.0, scales[i] - 1.0) * Math.Max(0.0, baseSizes[i]) * SlotExpansionFactor;
            extrasOut[i] = desired;
            totalDesired += desired;
        }

        if (totalDesired <= 0.001 || !double.IsFinite(availableExpansion) || availableExpansion <= 0.0)
        {
            extrasOut[..count].Clear();
            return 0.0;
        }

        if (totalDesired <= availableExpansion)
        {
            return totalDesired;
        }

        var ratio = availableExpansion / totalDesired;
        var totalActual = 0.0;
        for (var i = 0; i < count; i++)
        {
            extrasOut[i] *= ratio;
            totalActual += extrasOut[i];
        }

        return totalActual;
    }

    /// <summary>
    /// Computes subtle upward/outward lift (DIP) clamped to the available headroom so magnified icons never clip
    /// against the top/outer boundary of the dock window at any dock size or scale.
    /// </summary>
    public static double ComputeSafeLift(double scale, double maxScale, double iconSize, double availableHeadroom)
    {
        if (scale <= 1.001 || maxScale <= 1.0) return 0.0;
        var progress = Math.Clamp((scale - 1.0) / (maxScale - 1.0), 0.0, 1.0);
        var desiredLift = MaxLiftDips * progress;
        var growthOutward = Math.Max(0.0, iconSize) * (scale - 1.0);
        var remainingHeadroom = Math.Max(0.0, availableHeadroom - growthOutward - 1.0);
        return Math.Min(desiredLift, remainingHeadroom);
    }

    /// <summary>
    /// Extra vertical/side gap (DIP) added above <see cref="DockMetrics.PreviewGap"/> when magnification is
    /// enabled so window thumbnail previews maintain clean air space above the magnified icon peak.
    /// </summary>
    public static double ComputePreviewClearance(double iconSize, double effectiveMaxScale)
    {
        if (!double.IsFinite(iconSize) || iconSize <= 0 || !double.IsFinite(effectiveMaxScale) || effectiveMaxScale <= 1.0)
            return 0.0;

        var peakGrowth = iconSize * (effectiveMaxScale - 1.0) + MaxLiftDips;
        var desiredGapFromButtonTop = peakGrowth + 10.0;
        return Math.Max(0.0, Math.Ceiling(desiredGapFromButtonTop - DockMetrics.PreviewGap));
    }
}
