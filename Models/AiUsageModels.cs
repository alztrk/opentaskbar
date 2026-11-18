namespace OpenTaskbar.Models;

public enum AiProviderStatus
{
    Ready,
    NotConfigured,
    Unauthorized,
    Error
}

public sealed record AiUsageWindow(
    string LabelKey,
    double UtilizationPercent,
    DateTimeOffset? ResetsAt,
    string? DisplayValue = null);

public sealed record AiProviderReport(
    string ProviderId,
    string DisplayName,
    string? PlanLabel,
    AiProviderStatus Status,
    IReadOnlyList<AiUsageWindow> Windows,
    DateTimeOffset FetchedAt,
    string? ErrorKey = null,
    string? SetupCommand = null);
