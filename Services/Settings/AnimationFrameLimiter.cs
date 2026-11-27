using System.Windows;
using System.Windows.Media.Animation;

namespace OpenTaskbar.Services;

/// <summary>
/// Caps the frame rate of resource-defined Storyboards through WPF's DesiredFrameRate attached property.
/// Animations created directly in code are intentionally left alone: they are short state transitions,
/// not continuously running effects. 0 (or any unsupported value) restores the compositor default.
/// </summary>
internal static class AnimationFrameLimiter
{
    private static int _lastApplied = -1;

    public static void Apply(int fps)
    {
        if (Application.Current is null) return;
        var normalized = fps is 30 or 60 or 120 ? fps : 0;
        if (normalized == _lastApplied) return;
        var firstDefault = normalized == 0 && _lastApplied < 0;
        _lastApplied = normalized;
        // A fresh start with the default has nothing to reset, so avoid cloning frozen resources.
        if (!firstDefault) ApplyDictionary(Application.Current.Resources, normalized == 0 ? null : normalized);
    }

    private static void ApplyDictionary(ResourceDictionary dictionary, int? rate)
    {
        foreach (var key in dictionary.Keys.Cast<object>().ToArray())
        {
            if (dictionary[key] is not Timeline timeline) continue;
            if (timeline.IsFrozen)
            {
                // Resource dictionaries commonly freeze Freezables; a frozen timeline cannot accept an
                // attached property, so replace it with an unfrozen clone carrying the frame-rate cap.
                var clone = (Timeline)timeline.Clone();
                Timeline.SetDesiredFrameRate(clone, rate);
                dictionary[key] = clone;
            }
            else
            {
                Timeline.SetDesiredFrameRate(timeline, rate);
            }
        }
        foreach (var merged in dictionary.MergedDictionaries.ToArray()) ApplyDictionary(merged, rate);
    }
}
