using System.IO;
using System.Net;
using System.Net.Http;
using System.Text;
using OpenTaskbar.Models;
using OpenTaskbar.Services;
using OpenTaskbar.Widgets;

namespace OpenTaskbar.Tests;

internal static class AiUsageChecks
{
    public static int Run()
    {
        var checks = 0;

        void Check(bool condition, string name)
        {
            if (!condition) throw new InvalidOperationException(name);
            checks++;
        }

        Check(DockWidgetService.SupportedIds.Contains(DockWidgetService.AiLimitsId),
            "DockWidgetService must expose AiLimitsId.");
        Check(WidgetCoordinator.HandlesWidget(DockWidgetService.AiLimitsId),
            "WidgetCoordinator must handle AiLimits.");
        Check(WidgetCoordinator.GetLabelKey(DockWidgetService.AiLimitsId) == "WidgetAiLimits",
            "WidgetCoordinator must map AiLimits to WidgetAiLimits.");

        var tempDir = Path.Combine(Path.GetTempPath(), $"opentaskbar-ai-test-{Guid.NewGuid():N}");
        Directory.CreateDirectory(tempDir);
        try
        {
            var missingCredPath = Path.Combine(tempDir, "missing-credentials.json");
            var handler = new StubHandler(_ => throw new InvalidOperationException("HTTP must not be called without credentials."));
            var provider = new ClaudeUsageProvider(new HttpClient(handler), missingCredPath, TimeProvider.System);
            var missingReport = provider.FetchAsync(CancellationToken.None).GetAwaiter().GetResult();
            Check(missingReport.Status == AiProviderStatus.NotConfigured && missingReport.SetupCommand == "claude",
                "Missing credentials must return NotConfigured with claude setup command.");

            var credPath = Path.Combine(tempDir, ".credentials.json");
            File.WriteAllText(credPath, """
                {
                  "claudeAiOauth": {
                    "accessToken": "test-oauth-token",
                    "subscriptionType": "pro"
                  }
                }
                """);

            string? capturedAuth = null;
            string? capturedBeta = null;
            var okHandler = new StubHandler(req =>
            {
                capturedAuth = req.Headers.Authorization?.ToString();
                capturedBeta = req.Headers.TryGetValues("anthropic-beta", out var values) ? string.Join(",", values) : null;
                return new HttpResponseMessage(HttpStatusCode.OK)
                {
                    Content = new StringContent("""
                        {
                          "five_hour": { "utilization": 16.5, "resets_at": "2026-11-18T15:00:00+00:00" },
                          "seven_day": { "utilization": 24.0, "resets_at": "2026-11-22T00:00:00+00:00" }
                        }
                        """, Encoding.UTF8, "application/json")
                };
            });

            var okProvider = new ClaudeUsageProvider(new HttpClient(okHandler), credPath, TimeProvider.System);
            var okReport = okProvider.FetchAsync(CancellationToken.None).GetAwaiter().GetResult();
            Check(capturedAuth == "Bearer test-oauth-token", "Provider must send Bearer token.");
            Check(capturedBeta == "oauth-2025-04-20", "Provider must send anthropic-beta oauth header.");
            Check(okReport.Status == AiProviderStatus.Ready && okReport.PlanLabel == "Pro",
                "Valid response must produce Ready status and Pro plan label.");
            Check(okReport.Windows.Count == 2
                  && Math.Abs(okReport.Windows[0].UtilizationPercent - 16.5) < 0.01
                  && Math.Abs(okReport.Windows[1].UtilizationPercent - 24.0) < 0.01
                  && okReport.Windows[0].ResetsAt.HasValue,
                "Valid response must parse five_hour and seven_day windows.");

            var unauthHandler = new StubHandler(_ => new HttpResponseMessage(HttpStatusCode.Unauthorized));
            var unauthProvider = new ClaudeUsageProvider(new HttpClient(unauthHandler), credPath, TimeProvider.System);
            var unauthReport = unauthProvider.FetchAsync(CancellationToken.None).GetAwaiter().GetResult();
            Check(unauthReport.Status == AiProviderStatus.Unauthorized && unauthReport.SetupCommand == "claude",
                "401 response must return Unauthorized with claude setup command.");

            var badJsonHandler = new StubHandler(_ => new HttpResponseMessage(HttpStatusCode.OK)
            {
                Content = new StringContent("{not-json", Encoding.UTF8, "application/json")
            });
            var badJsonProvider = new ClaudeUsageProvider(new HttpClient(badJsonHandler), credPath, TimeProvider.System);
            var badJsonReport = badJsonProvider.FetchAsync(CancellationToken.None).GetAwaiter().GetResult();
            Check(badJsonReport.Status == AiProviderStatus.Error,
                "Malformed JSON response must return Error status.");

            // Codex / ChatGPT provider checks
            var codexCredPath = Path.Combine(tempDir, "codex-auth.json");
            File.WriteAllText(codexCredPath, """
                {
                  "tokens": {
                    "access_token": "codex-token",
                    "account_id": "acct-123"
                  }
                }
                """);
            string? codexAccountHeader = null;
            var codexHandler = new StubHandler(req =>
            {
                codexAccountHeader = req.Headers.TryGetValues("ChatGPT-Account-Id", out var vals) ? string.Join(",", vals) : null;
                return new HttpResponseMessage(HttpStatusCode.OK)
                {
                    Content = new StringContent("""
                        {
                          "plan_type": "plus",
                          "rate_limit": {
                            "primary_window": { "used_percent": 29, "reset_at": 1893456000 },
                            "secondary_window": { "used_percent": 90, "reset_at": 1894060800 }
                          }
                        }
                        """, Encoding.UTF8, "application/json")
                };
            });
            var codexProvider = new CodexUsageProvider(new HttpClient(codexHandler), codexCredPath, TimeProvider.System);
            var codexReport = codexProvider.FetchAsync(CancellationToken.None).GetAwaiter().GetResult();
            Check(codexAccountHeader == "acct-123", "Codex provider must forward ChatGPT-Account-Id header.");
            Check(codexReport.Status == AiProviderStatus.Ready && codexReport.PlanLabel == "Plus",
                "Codex provider must parse plan_type as Plus.");
            Check(codexReport.Windows.Count == 2
                  && Math.Abs(codexReport.Windows[0].UtilizationPercent - 29) < 0.01
                  && Math.Abs(codexReport.Windows[1].UtilizationPercent - 90) < 0.01,
                "Codex provider must parse primary and secondary windows.");

            // Antigravity CLI + cache provider checks
            var agyCachePath = Path.Combine(tempDir, "antigravity-cache.json");
            var agyMissing = new AntigravityUsageProvider(_ => Task.FromResult<(int, string)?>(null), agyCachePath, TimeProvider.System);
            var agyMissingReport = agyMissing.FetchAsync(CancellationToken.None, interactiveRefresh: true).GetAwaiter().GetResult();
            Check(agyMissingReport.Status == AiProviderStatus.NotConfigured && agyMissingReport.SetupCommand == "agy",
                "Missing agy CLI without cache must return NotConfigured.");

            var agyNow = new DateTimeOffset(2026, 11, 18, 12, 0, 0, TimeSpan.Zero);
            var agyProvider = new AntigravityUsageProvider(_ => Task.FromResult<(int, string)?>((0, """
                {
                  "status": "SUCCESS",
                  "command": {
                    "name": "usage",
                    "data": {
                      "groups": [
                        {
                          "name": "Gemini Models",
                          "buckets": [
                            { "id": "gemini-weekly", "window": "weekly", "remaining_fraction": 0.96, "reset_time": "2026-11-25T08:31:45Z" },
                            { "id": "gemini-5h", "window": "5h", "remaining_fraction": 0.89, "reset_time": "2026-11-18T15:31:45Z" }
                          ]
                        }
                      ]
                    }
                  }
                }
                """)), agyCachePath, new FixedTimeProvider(agyNow));
            var agyReport = agyProvider.FetchAsync(CancellationToken.None, interactiveRefresh: true).GetAwaiter().GetResult();
            Check(agyReport.Status == AiProviderStatus.Ready && agyReport.Windows.Count == 2,
                "Antigravity provider must parse command.data.groups 5h and weekly buckets.");
            Check(agyReport.Windows[0].LabelKey == "AiLimitsSession"
                  && Math.Abs(agyReport.Windows[0].UtilizationPercent - 11) < 0.01
                  && agyReport.Windows[1].LabelKey == "AiLimitsWeekly"
                  && Math.Abs(agyReport.Windows[1].UtilizationPercent - 4) < 0.01,
                "Antigravity provider must order 5h session first and weekly second.");
            Check(File.Exists(agyCachePath), "Antigravity provider must persist cache after successful CLI fetch.");

            // GitHub Copilot provider checks
            var copilotPath = Path.Combine(tempDir, "copilot-apps.json");
            File.WriteAllText(copilotPath, """{ "github.com:user": { "oauth_token": "ghu_test" } }""");
            var copilotHandler = new StubHandler(_ => new HttpResponseMessage(HttpStatusCode.OK)
            {
                Content = new StringContent("""
                    {
                      "copilot_plan": "individual",
                      "quota_reset_date_utc": "2026-12-01T00:00:00Z",
                      "quota_snapshots": {
                        "premium_interactions": { "percent_remaining": 72.5 }
                      }
                    }
                    """, Encoding.UTF8, "application/json")
            });
            var copilotProvider = new CopilotUsageProvider(new HttpClient(copilotHandler), [copilotPath], TimeProvider.System);
            var copilotReport = copilotProvider.FetchAsync(CancellationToken.None).GetAwaiter().GetResult();
            Check(copilotReport.Status == AiProviderStatus.Ready && copilotReport.PlanLabel == "Individual"
                  && Math.Abs(copilotReport.Windows[0].UtilizationPercent - 27.5) < 0.01,
                "Copilot provider must parse plan and remaining premium interactions.");

            // DeepSeek provider checks
            var deepSeekHandler = new StubHandler(_ => new HttpResponseMessage(HttpStatusCode.OK)
            {
                Content = new StringContent("""
                    {
                      "is_available": true,
                      "balance_infos": [{ "currency": "USD", "total_balance": "4.50" }]
                    }
                    """, Encoding.UTF8, "application/json")
            });
            var deepSeekProvider = new DeepSeekUsageProvider(new HttpClient(deepSeekHandler), () => "ds-test-key", TimeProvider.System);
            var deepSeekReport = deepSeekProvider.FetchAsync(CancellationToken.None).GetAwaiter().GetResult();
            Check(deepSeekReport.Status == AiProviderStatus.Ready
                  && deepSeekReport.Windows.Count == 1
                  && deepSeekReport.Windows[0].DisplayValue == "4.5 USD",
                "DeepSeek provider must parse balance_infos as remaining balance.");

            // OpenRouter provider checks
            var openRouterHandler = new StubHandler(_ => new HttpResponseMessage(HttpStatusCode.OK)
            {
                Content = new StringContent("""
                    {
                      "data": { "total_credits": 20.0, "total_usage": 5.0 }
                    }
                    """, Encoding.UTF8, "application/json")
            });
            var openRouterProvider = new OpenRouterUsageProvider(new HttpClient(openRouterHandler), () => "or-test-key", TimeProvider.System);
            var openRouterReport = openRouterProvider.FetchAsync(CancellationToken.None).GetAwaiter().GetResult();
            Check(openRouterReport.Status == AiProviderStatus.Ready && openRouterReport.PlanLabel == "$15"
                  && Math.Abs(openRouterReport.Windows[0].UtilizationPercent - 25.0) < 0.01,
                "OpenRouter provider must calculate remaining credits and usage percent.");
        }
        finally
        {
            if (Directory.Exists(tempDir))
                Directory.Delete(tempDir, recursive: true);
        }

        return checks;
    }

    private sealed class FixedTimeProvider(DateTimeOffset utcNow) : TimeProvider
    {
        public override DateTimeOffset GetUtcNow() => utcNow;
    }

    private sealed class StubHandler(Func<HttpRequestMessage, HttpResponseMessage> responder) : HttpMessageHandler
    {
        protected override Task<HttpResponseMessage> SendAsync(HttpRequestMessage request, CancellationToken cancellationToken) =>
            Task.FromResult(responder(request));
    }
}
