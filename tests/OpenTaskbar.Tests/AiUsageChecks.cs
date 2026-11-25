using System.Diagnostics;
using System.IO;
using System.Net;
using System.Net.Http;
using System.Net.Http.Headers;
using System.Security.Cryptography;
using System.Text;
using System.Text.Json;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Media;
using System.Windows.Threading;
using OpenTaskbar;
using OpenTaskbar.Models;
using OpenTaskbar.Services;
using OpenTaskbar.Widgets;

internal static class AiUsageChecks
{
    public static int Run()
    {
        var checks = 0;
        void Check(bool condition, string message)
        {
            if (!condition) throw new InvalidOperationException(message);
            checks++;
        }

        foreach (var command in new[] { "opencode", "agy" })
        {
            var startInfo = AiUsageService.HiddenCliStartInfo(command, "--test");
            Check(Path.GetFileNameWithoutExtension(startInfo.FileName).Equals(command, StringComparison.OrdinalIgnoreCase)
                && startInfo.ArgumentList.SequenceEqual(["--test"])
                && !startInfo.UseShellExecute && startInfo.CreateNoWindow
                && startInfo.WindowStyle == ProcessWindowStyle.Hidden
                && startInfo.RedirectStandardInput && startInfo.RedirectStandardOutput
                && startInfo.RedirectStandardError,
                "Quota CLI processes must not create visible or interactive console windows.");
        }

        var claude = AiUsageService.ParseClaude("""
            {"five_hour":{"utilization":62,"resets_at":"2026-11-19T12:00:00Z"},"seven_day":{"utilization":31}}
            """, "max");
        Check(claude.Error is null && claude.Report is { Plan: "Max", Windows.Count: 2 }
            && claude.Report.Windows[0].Used == 0.62 && claude.Report.Windows[1].Used == 0.31
            && claude.Report.Windows[0].ResetsAt == DateTimeOffset.Parse("2026-11-19T12:00:00Z"),
            "Claude fixtures must preserve percentages, plan and ISO reset dates.");
        var models = AiUsageService.ParseClaude("""
            {"seven_day_xyz":{"utilization":120},"seven_day_other":{"utilization":-5},"seven_day_nan":{"utilization":"NaN"},"extra_usage":{"utilization":40}}
            """, null).Report!;
        Check(models.Windows.Count == 3 && models.Windows[0].LabelKey == "AiUsageWeeklyModel:xyz",
            "Unknown weekly windows must survive; extra usage is not a quota window.");
        Check(models.Windows.Select(window => window.Used).SequenceEqual([1.0, 0.0, 0.0]),
            "Usage must clamp out-of-range and non-finite percentages.");
        var before = DateTimeOffset.UtcNow;
        var codex = AiUsageService.ParseCodex("""
            {"plan_type":"plus","rate_limit":{"primary_window":{"used_percent":18,"reset_at":1800000000,"limit_window_seconds":18000},"secondary_window":{"used_percent":7,"reset_after_seconds":60,"limit_window_seconds":604800}}}
            """).Report!;
        Check(codex.Plan == "Plus" && codex.Windows[0].Used == 0.18 && codex.Windows[0].LabelKey == "AiUsageSession"
            && codex.Windows[0].WindowSeconds == 18000 && codex.Windows[0].ResetsAt == DateTimeOffset.FromUnixTimeSeconds(1800000000),
            "Codex must parse primary usage, duration and Unix reset time.");
        Check(codex.Windows[1].LabelKey == "AiUsageWeekly" && codex.Windows[1].ResetsAt >= before.AddSeconds(60)
            && codex.Windows[1].ResetsAt <= DateTimeOffset.UtcNow.AddSeconds(60), "Codex must support relative reset times.");
        foreach (var invalid in new[] { "null", "true", "[]", "42", "{}", "{\"utilization\":null}", "{\"utilization\":true}",
            "{\"utilization\":\"invalid\"}", "{\"utilization\":{}}", "{\"utilization\":[]}" })
        {
            var partial = AiUsageService.ParseClaude("{\"five_hour\":" + invalid + ",\"seven_day\":{\"utilization\":31}}", null);
            Check(partial.Error is null && partial.Report is { Windows.Count: 1 } && partial.Report.Windows[0].Used == 0.31,
                "A malformed Claude window must leave valid windows available.");
            var partialCodex = AiUsageService.ParseCodex("{\"rate_limit\":{\"primary_window\":"
                + invalid.Replace("utilization", "used_percent") + ",\"secondary_window\":{\"used_percent\":31}}}");
            Check(partialCodex.Error is null && partialCodex.Report is { Windows.Count: 1 },
                "A malformed Codex window must leave valid windows available.");
        }
        var unzoned = AiUsageService.ParseClaude("""{"five_hour":{"utilization":1,"resets_at":"2026-11-19T12:00:00"}}""", null);
        Check(unzoned.Report!.Windows[0].ResetsAt == new DateTimeOffset(2026, 11, 19, 12, 0, 0, TimeSpan.Zero),
            "ISO reset dates without an offset must assume UTC.");
        foreach (var reset in new[] { "1800000000.5", "\"1800000000.5\"", "1800000000500", "\"1800000000500\"" })
        {
            var absolute = AiUsageService.ParseCodex("{\"rate_limit\":{\"primary_window\":{\"used_percent\":1,\"reset_at\":" + reset + "}}}");
            Check(absolute.Report!.Windows[0].ResetsAt == DateTimeOffset.UnixEpoch.AddSeconds(1800000000.5),
                "Codex reset times must accept fractional seconds and milliseconds as numbers or numeric strings.");
        }
        Check(AiUsageService.PlanLabel("default_claude_max_5x") == "Max"
            && AiUsageService.PlanLabel("plus") == "Plus" && AiUsageService.PlanLabel(new string('x', 40))!.Length == 24,
            "Plan headings must use short, capitalized labels rather than raw API tiers.");
        var full = AiUsageService.ParseClaude("""
            {"five_hour":{"utilization":42.0,"resets_at":"2026-11-18T22:00:00Z","limit_dollars":null},
             "seven_day":{"utilization":29.0,"resets_at":"2026-11-24T07:59:00Z"},"seven_day_opus":null,"seven_day_sonnet":null,
             "extra_usage":{"is_enabled":false,"monthly_limit":null,"used_credits":null,"utilization":null},
             "limits":[{"kind":"session","group":"session","percent":42,"severity":"warning"},
                       {"kind":"weekly_all","group":"weekly","percent":29,"severity":"normal"},{"group":null},null,42],
             "spend":{"used":{"amount_minor":0,"currency":"USD","exponent":2},"limit":null,"percent":0,"enabled":false},
             "seven_day_breakdown":{"rows":[{"key":"claude_code","display_name":"Claude Code","percent":98,"percent_of_limit":29},
                {"key":"chat","display_name":"Chats","percent":1,"percent_of_limit":0},{"display_name":null,"percent_of_limit":5},
                {"display_name":"Cowork","percent_of_limit":null},{"display_name":"Other","percent_of_limit":"bad"},
                {"display_name":"Tiny","percent_of_limit":0.4},{"display_name":"Small","percent_of_limit":0.6}]},"codename_x":null}
            """, null).Report!;
        Check(full.Windows.Count == 2 && full.Windows[0].Severity == "warning" && full.Windows[1].Severity == "normal",
            "Claude limit severities must attach to the session and weekly windows.");
        Check(full.Breakdown.Count == 2 && full.Breakdown[0] == new AiUsageShare("Claude Code", 0.29) && full.Breakdown[1].Name == "Small",
            "The weekly breakdown must keep only named rows that round to at least 1% of the limit.");
        Check(full.Spend is null && !full.LimitReached, "Disabled extra usage must not be shown.");
        var spend = AiUsageService.ParseClaude("""
            {"five_hour":{"utilization":10},"spend":{"used":{"amount_minor":420,"currency":"EUR","exponent":2},
             "limit":{"amount_minor":5000,"currency":"EUR","exponent":2},"percent":8.4,"enabled":true}}
            """, null).Report!.Spend;
        Check(spend is { Used: 4.2m, Limit: 50m, Currency: "EUR" } && Math.Abs(spend.Used01!.Value - 0.084) < 1e-9,
            "Enabled extra usage must convert minor units and keep the currency.");
        Check(AiUsageService.ParseClaude("""{"five_hour":{"utilization":1},"extra_usage":{"is_enabled":true},"spend":{"used":null}}""", null)
            .Report!.Spend is null, "Extra usage without an amount must stay hidden.");
        var nulls = AiUsageService.ParseClaude("""
            {"five_hour":{"utilization":5},"limits":null,"spend":null,"extra_usage":null,"seven_day_breakdown":{"rows":null}}
            """, null).Report!;
        Check(nulls.Breakdown.Count == 0 && nulls.Spend is null && nulls.Windows[0].Severity is null,
            "Null Claude extras must leave the quota windows usable.");

        var richCodex = AiUsageService.ParseCodex("""
            {"plan_type":"plus","rate_limit":{"allowed":true,"limit_reached":true,
              "primary_window":{"used_percent":31,"limit_window_seconds":18000,"reset_after_seconds":14000,"reset_at":null},
              "secondary_window":{"used_percent":90,"limit_window_seconds":null,"reset_at":1795100000}},
             "code_review_rate_limit":null,"additional_rate_limits":null,
             "credits":{"has_credits":true,"unlimited":false,"balance":"12.5"},
             "rate_limit_reset_credits":{"available_count":3,"applicable_available_count":0},
             "user_id":"fixture-user","account_id":"fixture-account","email":"fixture@example.invalid"}
            """).Report!;
        Check(richCodex.LimitReached && richCodex.Credits == 12.5 && !richCodex.UnlimitedCredits && richCodex.ResetCredits == 0
            && richCodex.Windows.Count == 2 && richCodex.Windows[1] is { WindowSeconds: null, LabelKey: "AiUsageWeekly" },
            "Codex must parse limit state and credits, count only applicable resets, and tolerate a missing window length.");
        Check(AiUsageService.ParseCodex("""
            {"rate_limit":{"primary_window":{"used_percent":1}},"rate_limit_reset_credits":{"available_count":3,"applicable_available_count":2}}
            """).Report!.ResetCredits == 2, "Codex must show the number of resets that apply now.");
        var badResets = AiUsageService.ParseCodex("""
            {"rate_limit":{"primary_window":{"used_percent":10,"reset_at":1e15,"reset_after_seconds":1e20},
              "secondary_window":{"used_percent":20,"reset_at":"soon","reset_after_seconds":60},
              "extra_window":{"used_percent":1}}}
            """);
        Check(badResets.Error is null && badResets.Report!.Windows[0].ResetsAt is null
            && badResets.Report.Windows[1].ResetsAt is { } fallback && fallback > DateTimeOffset.UtcNow.AddSeconds(30)
            && fallback < DateTimeOffset.UtcNow.AddSeconds(90),
            "Out-of-range resets must drop only the reset, and an unusable absolute reset must fall back to the relative one.");
        Check(AiUsageService.ParseCodex("""{"rate_limit":{"primary_window":{"used_percent":1,"reset_at":-5,"reset_after_seconds":-5}}}""")
            .Report!.Windows[0].ResetsAt is null, "Negative resets must be ignored.");
        Check(AiUsageService.ParseCodex("""{"rate_limit":{"allowed":false,"primary_window":{"used_percent":1}}}""").Report!.LimitReached,
            "A disallowed Codex request must count as a reached limit.");
        var quietCodex = AiUsageService.ParseCodex("""
            {"rate_limit":{"primary_window":{"used_percent":1}},"credits":{"has_credits":false,"unlimited":false,"balance":"0"},
             "rate_limit_reset_credits":{"available_count":0}}
            """).Report!;
        Check(!quietCodex.LimitReached && quietCodex.Credits is null && quietCodex.ResetCredits == 0,
            "Codex extras must stay hidden when the account has no credits or resets.");
        var unlimited = AiUsageService.ParseCodex("""
            {"rate_limit":{"primary_window":{"used_percent":1}},"credits":{"has_credits":true,"unlimited":true,"balance":null},
             "rate_limit_reset_credits":null}
            """).Report!;
        Check(unlimited.UnlimitedCredits && unlimited.Credits is null && unlimited.ResetCredits == 0,
            "Unlimited Codex credits must not show a balance.");
        foreach (var odd in new[] { "null", "true", "[]", "{\"balance\":\"x\",\"has_credits\":true}" })
        {
            var oddCodex = AiUsageService.ParseCodex("{\"rate_limit\":{\"primary_window\":{\"used_percent\":1}},\"credits\":" + odd
                + ",\"rate_limit_reset_credits\":" + odd + "}");
            Check(oddCodex.Error is null && oddCodex.Report is { Credits: null, ResetCredits: 0, LimitReached: false },
                "Malformed Codex extras must not break quota parsing.");
        }

        var now = new DateTimeOffset(2026, 11, 18, 12, 0, 0, TimeSpan.Zero);
        AiUsageWindow Slot(double used, double remainingHours, long? length = 18000) =>
            new("AiUsageSession", used, now.AddHours(remainingHours), length);
        Check(AiUsageWidget.Elapsed(Slot(0.5, 2.5), now) == 0.5, "Half of a five-hour window must have elapsed after 2.5 hours.");
        Check(AiUsageWidget.Elapsed(Slot(0.5, 0), now) is null && AiUsageWidget.Elapsed(Slot(0.5, 6), now) is null
            && AiUsageWidget.Elapsed(Slot(0.5, 1, null), now) is null && AiUsageWidget.Elapsed(Slot(0.5, 1, 0), now) is null
            && AiUsageWidget.Elapsed(new("AiUsageSession", 0.5, null, 18000), now) is null,
            "Pace needs a known window length and a reset inside that window.");
        Check(AiUsageWidget.LimitIn(Slot(0.8, 2.5), now) is { } soon && Math.Abs(soon.TotalMinutes - 37.5) < 0.01,
            "80% used halfway through must reach the limit in 37.5 minutes at the same rate.");
        Check(AiUsageWidget.LimitIn(Slot(0.5, 2.5), now) is null && AiUsageWidget.LimitIn(Slot(0.3, 2.5), now) is null,
            "On or under pace must not warn.");
        Check(AiUsageWidget.LimitIn(Slot(0.2, 4.9), now) is null, "The first minutes of a window must not project a limit.");
        Check(AiUsageWidget.LimitIn(Slot(1, 2.5), now) is null && AiUsageWidget.LimitIn(Slot(0, 2.5), now) is null,
            "Full or unused windows must not project a limit.");
        Check(AiUsageWidget.LimitIn(Slot(0.999, 2.5), now) == TimeSpan.FromMinutes(1),
            "Projected limits must not show less than a minute.");
        Check(AiUsageWidget.LimitIn(new("AiUsageWeekly", 0.6, now.AddDays(3.5), 604800), now) is { } weekly
            && weekly > TimeSpan.FromDays(1) && weekly < TimeSpan.FromDays(3.5), "Weekly windows must project past a day.");

        var ringReport = new AiUsageReport(AiProviders.Claude, null,
            [new("AiUsageSession", 0.2, null, 18000), new("AiUsageWeekly", 0.4, null, 604800), new("AiUsageWeeklyModel:opus", 0.7, null, 604800)],
            now);
        Check(AiUsageWidget.RingWindow(ringReport, AiUsageRing.Highest)!.Used == 0.4
            && AiUsageWidget.RingWindow(ringReport, AiUsageRing.Session)!.Used == 0.2
            && AiUsageWidget.RingWindow(ringReport, AiUsageRing.Weekly)!.Used == 0.4,
            "The ring must follow the selected window; Highest ignores per-model weekly windows.");
        Check(AiUsageWidget.RingWindow(ringReport with { Windows = [new("AiUsageWeeklyModel:opus", 0.7, null, 604800)] },
            AiUsageRing.Highest)!.Used == 0.7, "Per-model windows must still show when nothing else is reported.");
        var sessionOnly = ringReport with { Windows = [new("AiUsageSession", 0.2, null, 18000)] };
        Check(AiUsageWidget.RingWindow(sessionOnly, AiUsageRing.Weekly)!.Used == 0.2, "A missing selected window must fall back to the highest.");
        Check(AiUsageWidget.Accent(0.5) == "WidgetAccent" && AiUsageWidget.Accent(0.75) == "WidgetWarning" && AiUsageWidget.Accent(0.9) == "WidgetFocusAccent"
            && AiUsageWidget.Accent(0.2, "warning") == "WidgetWarning" && AiUsageWidget.Accent(0.2, "critical") == "WidgetFocusAccent"
            && AiUsageWidget.Accent(0.95, "normal") == "WidgetFocusAccent" && AiUsageWidget.Accent(0.2, "unknown") == "WidgetAccent",
            "Provider severity may raise the accent but never hide high usage.");
        AiUsageReport Report(params AiUsageWindow[] windows) => new(AiProviders.Codex, null, windows, now);
        Check(AiUsageWidget.Status(Report(new AiUsageWindow("AiUsageSession", 0.31, null, 18000), new("AiUsageWeekly", 0.9, null, 604800)))
                == ("AiUsageStatusNearLimit", "WidgetFocusAccent")
            && AiUsageWidget.Status(Report(new AiUsageWindow("AiUsageSession", 0.8, null, 18000))) == ("AiUsageStatusHigh", "WidgetWarning")
            && AiUsageWidget.Status(Report(new AiUsageWindow("AiUsageSession", 0.2, null, 18000, "warning"))) == ("AiUsageStatusHigh", "WidgetWarning")
            && AiUsageWidget.Status(Report(new AiUsageWindow("AiUsageSession", 0.2, null, 18000), new("AiUsageWeeklyModel:opus", 0.95, null, 604800)))
                == ("AiUsageStatusOk", "WidgetAccent")
            && AiUsageWidget.Status(Report(new AiUsageWindow("AiUsageSession", 0.5, null, 18000)) with { LimitReached = true })
                == ("AiUsageLimitReached", "WidgetFocusAccent")
            && AiUsageWidget.Status(Report(new AiUsageWindow("AiUsageWeekly", 1, null, 604800))) == ("AiUsageLimitReached", "WidgetFocusAccent"),
            "Status must follow thresholds or severity of session and weekly windows, with a reached limit first.");
        var session = new AiUsageWindow("AiUsageSession", 0.8, now.AddHours(2.5), 18000);
        var week = new AiUsageWindow("AiUsageWeekly", 0.9, now.AddDays(3.5), 604800);
        Check(AiUsageWidget.PaceWindow(Report(week, session), now) == session
            && AiUsageWidget.PaceWindow(Report(Slot(0.3, 2.5)), now) is null && AiUsageWidget.PaceWindow(Report(), now) is null,
            "Only the window that runs out soonest may carry a pace warning.");

        foreach (var json in new[] { "", "null", "{", "[]", "{}", "{\"five_hour\":true}", "{\"rate_limit\":null}" })
        {
            Check(AiUsageService.ParseClaude(json, null).Error == "ErrorAiUsageLoad", "Invalid Claude responses must return a localization key.");
            Check(AiUsageService.ParseCodex(json).Error == "ErrorAiUsageLoad", "Invalid Codex responses must return a localization key.");
            Check(AiUsageService.ParseAntigravity(json).Error == "ErrorAiUsageLoad", "Invalid Antigravity responses must return a localization key.");
            Check(AiUsageService.ParseOpenCode(json).Error == "ErrorAiUsageLoad", "Invalid OpenCode responses must return a localization key.");
            Check(AiUsageService.ParseCursor(json).Error == "ErrorAiUsageLoad", "Invalid Cursor responses must return a localization key.");
        }

        // Cursor parser checks
        const string cursorLiveUsage = """
            {"billingCycleStart":"1794571743688","billingCycleEnd":"1795176543688",
             "planUsage":{"remainingBonus":false,"autoPercentUsed":39.61538461538461,"totalPercentUsed":39.61538461538461},
             "enabled":true}
            """;
        const string cursorLivePlan = """
            {"planInfo":{"planName":"Plus","billingCycleEnd":"1796082954937","includedUsagePeriod":"INCLUDED_USAGE_PERIOD_WEEKLY","includedUsageResetsAt":"1795176543688"}}
            """;
        var (cursorReport, cursorErr) = AiUsageService.ParseCursor(cursorLiveUsage, cursorLivePlan);
        Check(cursorErr is null && cursorReport is { Provider: AiProviders.Cursor, Plan: "Plus", LimitReached: false }
            && cursorReport.Windows is [{ LabelKey: "AiUsageWeekly", WindowSeconds: 7 * 86400L }]
            && Math.Abs(cursorReport.Windows[0].Used - 0.396153846) < 1e-4
            && cursorReport.Windows[0].ResetsAt?.ToUnixTimeMilliseconds() == 1795176543688L
            && cursorReport.Breakdown.Count == 0,
            "Cursor must parse weekly usage, plan name, reset timestamp, and omit breakdown when API is absent and Auto matches total.");

        var (cursorWithApi, cursorWithApiErr) = AiUsageService.ParseCursor("""
            {"billingCycleStart":1794571743688,"billingCycleEnd":1795176543688,
             "planUsage":{"autoPercentUsed":25.0,"apiPercentUsed":14.5,"totalPercentUsed":39.5}}
            """);
        Check(cursorWithApiErr is null && cursorWithApi is not null
            && cursorWithApi.Windows[0] is { LabelKey: "AiUsageWeekly", WindowSeconds: 7 * 86400L }
            && cursorWithApi.Windows[0].ResetsAt?.ToUnixTimeMilliseconds() == 1795176543688L
            && cursorWithApi.Breakdown.Count == 2
            && cursorWithApi.Breakdown[0] == new AiUsageShare("Auto", 0.25)
            && cursorWithApi.Breakdown[1] == new AiUsageShare("API", 0.145),
            "Cursor must accept numeric timestamps and expose Auto/API breakdown when apiPercentUsed > 0.05.");

        var (cursorMonthly, cursorMonthlyErr) = AiUsageService.ParseCursor("""
            {"billingCycleStart":1794571743688,"billingCycleEnd":1797163743688,"planUsage":{"totalPercentUsed":50}}
            """);
        Check(cursorMonthlyErr is null && cursorMonthly is not null
            && cursorMonthly.Windows is [{ LabelKey: "AiUsageMonthly", Used: 0.5, WindowSeconds: 30 * 86400L }],
            "Cursor 30-day cycle must classify as AiUsageMonthly.");

        var (cursorForcedMonthly, _) = AiUsageService.ParseCursor(cursorLiveUsage, """
            {"planInfo":{"planName":"Pro","includedUsagePeriod":"INCLUDED_USAGE_PERIOD_MONTHLY"}}
            """);
        Check(cursorForcedMonthly is { Plan: "Pro" } && cursorForcedMonthly.Windows[0].LabelKey == "AiUsageMonthly",
            "Cursor INCLUDED_USAGE_PERIOD_MONTHLY hint must override window length heuristic.");

        var (cursorBadPlan, cursorBadPlanErr) = AiUsageService.ParseCursor(cursorLiveUsage, "{not valid json");
        Check(cursorBadPlanErr is null && cursorBadPlan is { Plan: null }
            && cursorBadPlan.Windows is [{ LabelKey: "AiUsageWeekly", WindowSeconds: 7 * 86400L }],
            "Cursor must ignore malformed secondary GetPlanInfo JSON and keep parsed usage.");

        var (cursorPlanResetFallback, _) = AiUsageService.ParseCursor("""
            {"planUsage":{"totalPercentUsed":100}}
            """, cursorLivePlan);
        Check(cursorPlanResetFallback is { LimitReached: true }
            && cursorPlanResetFallback.Windows is [{ LabelKey: "AiUsageWeekly", Used: 1.0, WindowSeconds: 7 * 86400L }]
            && cursorPlanResetFallback.Windows[0].ResetsAt?.ToUnixTimeMilliseconds() == 1795176543688L,
            "Cursor must fall back to GetPlanInfo includedUsageResetsAt and 7-day weekly window length when usage timestamps are omitted.");

        foreach (var badCursor in new[] { """{"planUsage":{}}""", """{"planUsage":{"autoPercentUsed":10}}""", """{"planUsage":{"totalPercentUsed":"NaN"}}""" })
        {
            var (badRep, badErr) = AiUsageService.ParseCursor(badCursor);
            Check(badRep is null && badErr == "ErrorAiUsageLoad",
                "Cursor responses missing a finite planUsage.totalPercentUsed must fail with ErrorAiUsageLoad.");
        }

        // Cursor PKCE RFC 7636 vector and StartLogin checks
        const string pkceVerifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        const string pkceExpected = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";
        Check(CursorAuthService.PkceChallenge(pkceVerifier) == pkceExpected,
            "Cursor PKCE challenge must match RFC 7636 test vector.");
        var (v, c, uuid, loginUrl) = CursorAuthService.StartLogin();
        Check(v.Length == 43 && !v.Contains('=') && CursorAuthService.PkceChallenge(v) == c
            && Guid.TryParseExact(uuid, "D", out _) && uuid == uuid.ToLowerInvariant()
            && loginUrl == $"https://cursor.com/loginDeepControl?challenge={c}&uuid={uuid}&mode=login",
            "Cursor StartLogin must generate a valid Base64Url verifier, matching challenge, lowercase UUID, and loginDeepControl URL.");

        // Cursor JWT expiry check
        static string FakeJwt(long expUnix) =>
            $"{CursorAuthService.Base64Url("{\"alg\":\"HS256\"}"u8)}.{CursorAuthService.Base64Url(System.Text.Encoding.UTF8.GetBytes($"{{\"exp\":{expUnix}}}"))}.sig";
        Check(CursorAuthService.IsExpiringSoon(FakeJwt(DateTimeOffset.UtcNow.AddMinutes(2).ToUnixTimeSeconds()))
            && !CursorAuthService.IsExpiringSoon(FakeJwt(DateTimeOffset.UtcNow.AddMinutes(20).ToUnixTimeSeconds()))
            && !CursorAuthService.IsExpiringSoon("not-a-jwt"),
            "Cursor IsExpiringSoon must detect JWTs expiring within 5 minutes and safely ignore non-JWT tokens.");

        // Cursor DPAPI save / load / sign-out roundtrip using isolated temp TokenFilePath
        var previousCursorPath = CursorAuthService.TokenFilePath;
        var tempCursorPath = Path.Combine(Path.GetTempPath(), $"ot-cursor-dpapi-{Guid.NewGuid():N}", "cursor-auth.bin");
        CursorAuthService.TokenFilePath = tempCursorPath;
        var sessionEvents = 0;
        Action onSessionChanged = () => sessionEvents++;
        CursorAuthService.SessionChanged += onSessionChanged;
        try
        {
            Check(!CursorAuthService.HasSession && CursorAuthService.LoadTokens() == (null, null),
                "Missing Cursor token file must report no session.");
            CursorAuthService.SaveTokens("cursor-access-123", "cursor-refresh-456");
            Check(File.Exists(tempCursorPath)
                && !System.Text.Encoding.UTF8.GetString(File.ReadAllBytes(tempCursorPath)).Contains("cursor-access-123", StringComparison.Ordinal),
                "Cursor token file must be DPAPI-encrypted on disk, never plaintext.");
            Check(CursorAuthService.HasSession
                && CursorAuthService.LoadTokens() == ("cursor-access-123", "cursor-refresh-456")
                && AiUsageService.CredentialStamp(AiProviders.Cursor) > DateTime.MinValue
                && sessionEvents == 1,
                "Cursor DPAPI SaveTokens/LoadTokens must round-trip access and refresh tokens and notify SessionChanged.");
            CursorAuthService.SignOut();
            Check(!CursorAuthService.HasSession && !File.Exists(tempCursorPath)
                && CursorAuthService.LoadTokens() == (null, null) && sessionEvents == 2,
                "Cursor SignOut must delete the token file and notify SessionChanged.");

            // RefreshAsync: rotation, transient 5xx preservation, shouldLogout, and 401 sign-out
            CursorAuthService.SaveTokens("old-access", "old-refresh");
            using (var rotateClient = new HttpClient(new StubHttpHandler(_ => new HttpResponseMessage(HttpStatusCode.OK)
            {
                Content = new StringContent("""{"access_token":"new-access","refresh_token":"rotated-refresh"}""", Encoding.UTF8, "application/json")
            })))
            {
                var r1 = CursorAuthService.RefreshAsync("old-refresh", rotateClient, CancellationToken.None).GetAwaiter().GetResult();
                Check(!r1.SessionExpired && r1.AccessToken == "new-access" && r1.RefreshToken == "rotated-refresh"
                    && CursorAuthService.LoadTokens() == ("new-access", "rotated-refresh"),
                    "Cursor RefreshAsync must persist rotated refresh_token when returned.");
            }

            using (var transientClient = new HttpClient(new StubHttpHandler(_ => new HttpResponseMessage(HttpStatusCode.ServiceUnavailable))))
            {
                var r2 = CursorAuthService.RefreshAsync("rotated-refresh", transientClient, CancellationToken.None).GetAwaiter().GetResult();
                Check(!r2.SessionExpired && r2.AccessToken is null && CursorAuthService.HasSession,
                    "Cursor RefreshAsync must preserve the existing session on transient 5xx errors.");
            }

            using (var logoutClient = new HttpClient(new StubHttpHandler(_ => new HttpResponseMessage(HttpStatusCode.OK)
            {
                Content = new StringContent("""{"shouldLogout":true}""", Encoding.UTF8, "application/json")
            })))
            {
                var r3 = CursorAuthService.RefreshAsync("rotated-refresh", logoutClient, CancellationToken.None).GetAwaiter().GetResult();
                Check(r3.SessionExpired && !CursorAuthService.HasSession,
                    "Cursor RefreshAsync must sign out cleanly when shouldLogout is true.");
            }

            CursorAuthService.SaveTokens("temp-access", "temp-refresh");
            using (var unauthorizedClient = new HttpClient(new StubHttpHandler(_ => new HttpResponseMessage(HttpStatusCode.Unauthorized))))
            {
                var r4 = CursorAuthService.RefreshAsync("temp-refresh", unauthorizedClient, CancellationToken.None).GetAwaiter().GetResult();
                Check(r4.SessionExpired && !CursorAuthService.HasSession,
                    "Cursor RefreshAsync must sign out cleanly on HTTP 401.");
            }

            // Concurrent stale-token RefreshAsync must reuse the rotated session without clearing it.
            CursorAuthService.SaveTokens("rotated-access", "rotated-refresh");
            using (var unexpectedCallClient = new HttpClient(new StubHttpHandler(_ => throw new InvalidOperationException("Refresh lock should skip stale token HTTP exchange."))))
            {
                var rConcurrent = CursorAuthService.RefreshAsync("stale-refresh", unexpectedCallClient, CancellationToken.None).GetAwaiter().GetResult();
                Check(!rConcurrent.SessionExpired && rConcurrent.AccessToken == "rotated-access"
                    && rConcurrent.RefreshToken == "rotated-refresh" && CursorAuthService.HasSession,
                    "Cursor RefreshAsync must short-circuit when another caller already rotated the session.");
            }

            // Oversized (>64 KB) encrypted token files must fail closed without reading unbounded bytes.
            Directory.CreateDirectory(Path.GetDirectoryName(tempCursorPath)!);
            File.WriteAllBytes(tempCursorPath, new byte[(1 << 16) + 16]);
            Check(CursorAuthService.LoadTokens() == (null, null),
                "Cursor LoadTokens must ignore oversized token files safely.");
        }
        finally
        {
            CursorAuthService.SessionChanged -= onSessionChanged;
            CursorAuthService.TokenFilePath = previousCursorPath;
            var dir = Path.GetDirectoryName(tempCursorPath);
            if (dir is not null && Directory.Exists(dir)) Directory.Delete(dir, recursive: true);
        }

        Check(DockWidgetService.AiUsageWidgets.Any(w => w.Id == "AiUsageCursor" && w.Provider == AiProviders.Cursor && w.LabelKey == "WidgetAiUsageCursor"),
            "AiUsageCursor must be registered in DockWidgetService.AiUsageWidgets.");

        var openCode = AiUsageService.ParseOpenCode("""
            {"rolling5h":{"usageDollars":2.34,"limitDollars":12,"usagePercent":19.5,"resetInSec":7200},
             "weekly":{"usageDollars":8.91,"limitDollars":30,"usagePercent":29.7,"resetInSec":345600},
             "monthly":{"usageDollars":15,"limitDollars":60,"resetInSec":1414800}}
            """).Report!;
        Check(openCode.Provider == AiProviders.OpenCode && openCode.Plan == "Go" && !openCode.LimitReached && openCode.Windows.Count == 3
            && openCode.Windows[0] is { LabelKey: "AiUsageSession", WindowSeconds: 18000 } && Math.Abs(openCode.Windows[0].Used - 0.195) < 1e-6
            && openCode.Windows[1] is { LabelKey: "AiUsageWeekly", WindowSeconds: 604800 } && Math.Abs(openCode.Windows[1].Used - 0.297) < 1e-6
            && openCode.Windows[2] is { LabelKey: "AiUsageMonthly", WindowSeconds: 2592000 } && Math.Abs(openCode.Windows[2].Used - 0.25) < 1e-6
            && openCode.Windows.All(window => window.ResetsAt is not null),
            "OpenCode Go must parse 5-hour, weekly and monthly rolling quota windows and derive percentage from dollars when usagePercent is omitted.");

        var agy = AiUsageService.ParseAntigravity("""
            {"buckets":[
                {"modelId":"chat_20706","remainingFraction":1},
                {"modelId":"gemini-2.5-pro","remainingFraction":0.6,"resetTime":"2026-11-18T20:30:00Z"},
                {"modelId":"gemini-4-hydrogen-sonar","remainingFraction":0.55,"resetTime":"2026-11-18T20:30:00Z"},
                {"modelId":"gemini-3-flash","remainingFraction":0.8,"resetTime":"2026-11-18T20:30:00Z"},
                {"modelId":"claude-sonnet-5-5-medium","remainingFraction":0.05,"resetTime":"2026-11-18T21:20:00Z"},
                {"modelId":"gpt-oss-120b-medium","remainingFraction":0.05,"resetTime":"2026-11-18T21:20:00Z"}
            ]}
            """, "Google AI Pro").Report!;
        Check(agy.Provider == AiProviders.Antigravity && agy.Plan == "Google AI Pro" && agy.Windows.Count == 3
            && agy.Windows[0] is { LabelKey: "AiUsagePool:Gemini Pro" } && Math.Abs(agy.Windows[0].Used - 0.45) < 1e-6
            && agy.Windows[1] is { LabelKey: "AiUsagePool:Gemini Flash" } && Math.Abs(agy.Windows[1].Used - 0.2) < 1e-6
            && agy.Windows[2] is { LabelKey: "AiUsagePool:Claude" } && Math.Abs(agy.Windows[2].Used - 0.95) < 1e-6,
            "Antigravity must group model buckets into Gemini Pro, Gemini Flash and Claude pools and deduplicate shared GPT-OSS quota.");
        var agyExhausted = AiUsageService.ParseAntigravity("""
            {"buckets":[{"modelId":"claude-opus-5-5-high","resetTime":"2026-11-18T21:20:00Z"}]}
            """).Report!;
        Check(agyExhausted.LimitReached && agyExhausted.Windows is [{ LabelKey: "AiUsagePool:Claude", Used: 1.0 }],
            "Antigravity buckets that omit remainingFraction when resetTime is set must register as 100% used.");
        var agyGrouped = AiUsageService.ParseAntigravity("""
            {"command":{"name":"usage","data":{"groups":[
                {"name":"Gemini Models","description":"Models within this group: Gemini 4 Hydrogen, Gemini 3.8 Flash",
                 "buckets":[{"id":"gemini-weekly","window":"weekly","remaining_fraction":0.5878,"reset_time":"2026-11-24T23:28:59Z"},
                            {"id":"gemini-5h","window":"5h","remaining_fraction":0.4554,"reset_time":"2026-11-18T20:26:15Z"}]},
                {"name":"Claude and GPT models","description":"Models within this group: Claude Opus 5.5, Claude Sonnet 5.5, GPT-OSS 120B",
                 "buckets":[{"id":"3p-weekly","window":"weekly","remaining_fraction":0.4907,"reset_time":"2026-11-25T16:21:48Z"},
                            {"id":"3p-5h","window":"5h","remaining_fraction":0.0,"reset_time":"2026-11-18T21:21:48Z"}]},
                {"name":"Gemini 4 Argon","description":"For paid Pro plans, Argon quota is separate.",
                 "buckets":[{"id":"gemini-argon-wtu-weekly","window":"weekly","remaining_fraction":0.6967,"reset_time":"2026-11-25T16:56:21Z"}]}
            ]}}}
            """).Report!;
        Check(!agyGrouped.LimitReached && agyGrouped.Windows.Count == 5
            && agyGrouped.Windows[0] is { Group: "AiUsageGroupGemini", GroupModels: "Gemini 4 Hydrogen, Gemini 3.8 Flash", LabelKey: "AiUsageWeekly", WindowSeconds: 604800 }
            && agyGrouped.Windows[1] is { Group: "AiUsageGroupGemini", LabelKey: "AiUsageSession", WindowSeconds: 18000 }
            && agyGrouped.Windows[3] is { Group: "AiUsageGroupClaudeGpt", LabelKey: "AiUsageSession", Used: 1.0 }
            && agyGrouped.Windows[4] is { Group: "Gemini 4 Argon", GroupModels: null, LabelKey: "AiUsageWeekly" }
            && AiUsageWidget.RingWindow(agyGrouped, AiUsageRing.Session)!.Used == 1.0
            && Math.Abs(AiUsageWidget.RingWindow(agyGrouped, AiUsageRing.Weekly)!.Used - (1 - 0.4907)) < 1e-6,
            "Antigravity grouped usage must preserve weekly and 5-hour windows per model group.");

        var codexFreeMonthly = AiUsageService.ParseCodex("""
            {"plan_type":"free","rate_limit":{"allowed":true,"limit_reached":false,
             "primary_window":{"used_percent":45,"limit_window_seconds":2592000,"reset_after_seconds":864000},
             "secondary_window":null}}
            """).Report!;
        Check(codexFreeMonthly.Plan == "Free"
            && codexFreeMonthly.Windows.Count == 1
            && codexFreeMonthly.Windows[0] is { LabelKey: "AiUsageMonthly", Used: 0.45, WindowSeconds: 2592000 },
            "Codex free-plan 30-day windows must classify as AiUsageMonthly instead of AiUsageWeekly.");

        var path = Path.Combine(Path.GetTempPath(), $"ot-ai-fixture-{Guid.NewGuid():N}.json");
        try
        {
            Check(AiUsageService.ReadClaudeToken(path) is null && AiUsageService.ReadCodexToken(path) is null,
                "Missing fixture credentials must require CLI sign-in.");
            foreach (var expiry in new[] { 1L, DateTimeOffset.UtcNow.AddHours(1).ToUnixTimeMilliseconds() })
            {
                File.WriteAllText(path, JsonSerializer.Serialize(new { claudeAiOauth = new { accessToken = "fixture-only", expiresAt = expiry } }));
                var hash = SHA256.HashData(File.ReadAllBytes(path));
                Check(AiUsageService.ReadClaudeToken(path) == (expiry == 1 ? null : "fixture-only"),
                    "Expired Claude credentials must not be used.");
                Check(hash.SequenceEqual(SHA256.HashData(File.ReadAllBytes(path))), "Credential reads must leave fixture bytes unchanged.");
            }
            File.WriteAllText(path, """{"claudeAiOauth":{"accessToken":"","refreshToken":"fixture-refresh-desktop","scopes":["user:profile"],"subscriptionType":"pro","rateLimitTier":"default_claude_ai"}}""");
            Check(AiUsageService.ReadClaudeToken(path) is null, "A desktop credential without an accessToken must trigger refresh on load.");
            var refreshedToken = AiUsageService.TryRefreshClaudeTokenAsync(path, (refresh, _) =>
                Task.FromResult<string?>(refresh == "fixture-refresh-desktop"
                    ? """{"access_token":"fixture-refreshed-access","refresh_token":"fixture-rotated-refresh","expires_in":28800}"""
                    : null)).GetAwaiter().GetResult();
            Check(refreshedToken == "fixture-refreshed-access" && AiUsageService.ReadClaudeToken(path) == "fixture-refreshed-access",
                "Claude refresh must exchange refreshToken and persist the new accessToken and expiresAt back to .credentials.json.");
            using (var refreshedDoc = JsonDocument.Parse(File.ReadAllText(path)))
            {
                var oauth = refreshedDoc.RootElement.GetProperty("claudeAiOauth");
                Check(oauth.GetProperty("refreshToken").GetString() == "fixture-rotated-refresh"
                    && oauth.GetProperty("subscriptionType").GetString() == "pro"
                    && oauth.GetProperty("scopes").GetArrayLength() == 1,
                    "Claude refresh must persist the rotated refreshToken and preserve existing OAuth metadata.");
            }
            File.WriteAllText(path, """{"tokens":{"access_token":"fixture-codex","account_id":"fixture-account"}}""");
            Check(AiUsageService.ReadCodexToken(path) == "fixture-codex", "Codex must read its nested CLI token.");
            File.WriteAllText(path, """{"token":{"access_token":"fixture-agy"}}""");
            Check(AiUsageService.ReadAntigravityToken(path) == "fixture-agy", "Antigravity must read its nested keyring token.");
            File.WriteAllText(path, """{"access_token":"fixture-agy-flat","expiry_date":1}""");
            Check(AiUsageService.ReadAntigravityToken(path) is null, "Expired Antigravity oauth_creds must not be used.");
            File.WriteAllText(path, """{"opencode-go":{"type":"api","key":"fixture-oc-v1"}}""");
            Check(AiUsageService.ReadOpenCodeToken(path) == "fixture-oc-v1", "OpenCode must read v1 auth.json provider keys.");
            File.WriteAllText(path, """[{"integrationID":"ollama-cloud","active":true,"value":{"key":"other"}},{"integrationID":"opencode-go","active":true,"value":{"type":"key","key":"fixture-oc-v2"}}]""");
            Check(AiUsageService.ReadOpenCodeToken(path) == "fixture-oc-v2", "OpenCode must read v2 auth export array entries.");
            File.WriteAllText(path, """{"github.com:fixture":{"oauth_token":"fixture-copilot"}}""");
            Check(AiUsageService.ReadCopilotToken(path) == "fixture-copilot", "Copilot must read its oauth_token from apps.json.");
            var (copilotReport, copilotError) = AiUsageService.ParseCopilot("""
                {"copilot_plan":"individual","quota_reset_date_utc":"2026-12-01T00:00:00Z","quota_snapshots":{"premium_interactions":{"percent_remaining":72.5}}}
                """);
            Check(copilotError is null && copilotReport is { Provider: AiProviders.Copilot, Plan: "Individual", LimitReached: false }
                  && copilotReport.Windows is [{ LabelKey: "AiUsageMonthly", Used: >= 0.274 and <= 0.276, WindowSeconds: 2592000L }],
                "Copilot must parse plan, reset date, and premium interactions remaining percentage.");
            File.WriteAllText(path, """{"api_key":"fixture-deepseek"}""");
            Check(AiUsageService.ReadDeepSeekToken(path) == "fixture-deepseek", "DeepSeek must read its api_key from auth.json.");
            var (dsReport, dsError) = AiUsageService.ParseDeepSeek("""
                {"is_available":true,"balance_infos":[{"currency":"USD","total_balance":"4.81"}]}
                """);
            Check(dsError is null && dsReport is { Provider: AiProviders.DeepSeek, Plan: "4.81 USD", Credits: 4.81, LimitReached: false }
                  && dsReport.Windows is [{ LabelKey: "AiUsageBalance", Used: 0.0 }],
                "DeepSeek must parse balance_infos and remaining balance.");
            File.WriteAllText(path, """{"api_key":"fixture-openrouter"}""");
            Check(AiUsageService.ReadOpenRouterToken(path) == "fixture-openrouter", "OpenRouter must read its api_key from auth.json.");
            var (orReport, orError) = AiUsageService.ParseOpenRouter("""
                {"data":{"total_credits":20.0,"total_usage":5.0}}
                """);
            Check(orError is null && orReport is { Provider: AiProviders.OpenRouter, Plan: "$15", Credits: 15.0, LimitReached: false }
                  && orReport.Windows is [{ LabelKey: "AiUsageCreditsLabel", Used: >= 0.249 and <= 0.251 }],
                "OpenRouter must parse total_credits and total_usage.");
            File.WriteAllText(path, "null");
            Check(AiUsageService.ReadClaudeToken(path) is null && AiUsageService.ReadCodexToken(path) is null,
                "Malformed credential files must fail closed.");
            var settings = new DockSettings(AiUsageProviders: AiProviders.All);
            var store = new DockSettingsStore(path);
            Check(!DockSettingsStore.Validate(settings with { AiUsageProviders = (AiProviders)256 }), "Unknown AI provider flags must be rejected.");
            Check(store.Save(settings) is null && store.Load().Settings.AiUsageProviders == AiProviders.None,
                "Legacy provider flags must be consumed by the per-provider widget migration.");
            File.WriteAllText(path, "{}");
            Check(store.Load().Settings is { AiUsageProviders: AiProviders.None, AiUsageRing: AiUsageRing.Highest, AntigravityGroups: AntigravityGroups.All },
                "Legacy settings must leave providers disabled, the ring on the highest window, and Antigravity groups on All.");
            Check(!DockSettingsStore.Validate(settings with { AiUsageRing = (AiUsageRing)7 }), "Unknown ring windows must be rejected.");
            Check(!DockSettingsStore.Validate(settings with { AntigravityGroups = AntigravityGroups.None })
                && !DockSettingsStore.Validate(settings with { AntigravityGroups = (AntigravityGroups)99 })
                && DockSettingsStore.Validate(settings with { AntigravityGroups = AntigravityGroups.Gemini | AntigravityGroups.Argon }),
                "AntigravityGroups must require at least one valid group flag and reject unknown bits.");
            Check(store.Save(settings with { AiUsageRing = AiUsageRing.Weekly, AntigravityGroups = AntigravityGroups.Gemini | AntigravityGroups.Argon }) is null
                && store.Load().Settings is { AiUsageRing: AiUsageRing.Weekly, AntigravityGroups: AntigravityGroups.Gemini | AntigravityGroups.Argon }
                && File.ReadAllText(path).Contains("\"AiUsageRing\": \"Weekly\"", StringComparison.Ordinal),
                "The ring window and Antigravity groups must round-trip through settings.");
            Check(SettingsBackup.Export(path, settings) is null, "AI preferences must support settings export.");
            using var backup = JsonDocument.Parse(File.ReadAllText(path));
            var exported = backup.RootElement.GetProperty("Settings");
            Check(!exported.EnumerateObject().Any(property => property.Name.Contains("token", StringComparison.OrdinalIgnoreCase)
                || property.Name.Contains("credential", StringComparison.OrdinalIgnoreCase)), "Settings export must contain no credential fields.");
        }
        finally { File.Delete(path); }

        Check(AiUsageService.HttpFailure(HttpStatusCode.Unauthorized, null).Error == "ErrorAiUsageSignIn",
            "Only unauthorized responses must require CLI sign-in.");
        foreach (var status in new[] { HttpStatusCode.Forbidden, HttpStatusCode.Gone, HttpStatusCode.NotFound })
        {
            var failure = AiUsageService.HttpFailure(status, null);
            Check(failure.Error == "ErrorAiUsageLoad" && failure.RetryAfter is null
                && AiUsageWidget.RetryDelay(failure.Error, 1, failure.RetryAfter) == TimeSpan.FromMinutes(2),
                "Forbidden and removed endpoints must use normal error backoff.");
        }
        var retry = AiUsageService.HttpFailure(HttpStatusCode.TooManyRequests, new RetryConditionHeaderValue(TimeSpan.FromMinutes(42)));
        Check(retry.Error == "ErrorAiUsageRateLimited" && AiUsageWidget.RetryDelay(retry.Error, 1, retry.RetryAfter) == TimeSpan.FromMinutes(42),
            "Retry-After must override exponential backoff, even beyond the default cap.");
        Check(AiUsageWidget.RetryDelay("ErrorAiUsageRateLimited", 1, null) == TimeSpan.FromMinutes(5)
            && AiUsageWidget.RetryDelay("ErrorAiUsageRateLimited", 2, null) == TimeSpan.FromMinutes(10)
            && AiUsageWidget.RetryDelay("ErrorAiUsageRateLimited", 3, null) == TimeSpan.FromMinutes(20)
            && AiUsageWidget.RetryDelay("ErrorAiUsageRateLimited", 4, null) == TimeSpan.FromMinutes(30), "Rate limiting must back off to a thirty-minute cap.");
        Check(AiUsageWidget.RetryDelay("ErrorAiUsageLoad", 1, null) == TimeSpan.FromMinutes(2)
            && AiUsageWidget.RetryDelay("ErrorAiUsageLoad", 2, null) == TimeSpan.FromMinutes(4), "Network failures must use exponential retries.");
        Check(AiUsageWidget.RetryDelay(null, 0, null).TotalSeconds is >= 300 and <= 330, "Successful loads must refresh after five minutes plus jitter.");

        string[] keys = ["WidgetAiUsage", "WidgetAiUsageDescription", "AiUsageSettingsHeading", "AiUsageProviderHint",
            "AiUsageUnofficialNote", "AiUsageSession", "AiUsageWeekly", "AiUsageSessionShort", "AiUsageWeeklyShort", "AiUsageMonthly", "AiUsageWeeklyModel", "AiUsageGroupGemini", "AiUsageGroupClaudeGpt", "AiUsageAntigravityGroupsLabel", "AiUsageResetsIn", "AiUsageResetsAt",
            "AiUsageUpdated", "AiUsageRefresh", "AiUsageOpenDashboard", "AiUsageNoProviders", "AiUsageRingLabel",
            "AiUsageRingHighest", "AiUsageRingSession", "AiUsageLimitReached", "AiUsageStatusOk", "AiUsageStatusSignIn",
            "AiUsageStatusStale", "AiUsageStatusNearLimit", "AiUsageStatusHigh", "AiUsageStatusUnavailable", "AiUsageStatusLoading", "AiUsagePaceIn", "AiUsagePaceAt", "AiUsagePaceMarker", "HoursMinutesFormat",
            "AiUsageExtra", "AiUsageBreakdown", "AiUsageCredits", "AiUsageCreditsUnlimited", "AiUsageResetCredits",
            "ErrorAiUsageSignIn", "ErrorAiUsageLoad", "ErrorAiUsageRateLimited",
            "AiUsageAccountLabel", "AiUsageAccountConnected", "AiUsageAccountSignInRequired",
            "AiUsageCliSignInLabel", "AiUsageApiKeyLabel", "AiUsageApiKeyPlaceholder",
            "AiUsageApiKeySaved", "AiUsageApiKeySave", "AiUsageApiKeyClear",
            "WidgetAiUsageCursor", "WidgetAiUsageCursorDescription", "AiUsageCursorAccountLabel",
            "AiUsageCursorConnected", "AiUsageCursorSignInRequired", "AiUsageCursorSignIn",
            "AiUsageCursorSignOut", "AiUsageCursorWaiting", "AiUsageCursorSignInTimedOut", "ErrorAiUsageCursorSignIn"];
        var assembly = typeof(LanguageCatalog).Assembly;
        foreach (var resource in assembly.GetManifestResourceNames().Where(name => name.StartsWith("OpenTaskbar.Localization.Languages.", StringComparison.Ordinal)))
        {
            using var stream = assembly.GetManifestResourceStream(resource)!;
            using var document = JsonDocument.Parse(stream);
            Check(keys.All(key => document.RootElement.TryGetProperty(key, out var value) && !string.IsNullOrWhiteSpace(value.GetString())),
                $"Every AI usage key must be translated in {resource}.");
        }

        // DPAPI secret store and AiUsageService API key management checks in an isolated temp root.
        var previousRoot = AiSecretStore.Root;
        var tempSecretRoot = Path.Combine(Path.GetTempPath(), $"ot-ai-secrets-{Guid.NewGuid():N}");
        try
        {
            AiSecretStore.Root = tempSecretRoot;
            Check(!AiSecretStore.HasSecret("api-key:deepseek") && AiSecretStore.Get("api-key:deepseek") is null,
                "Missing secret files must return null and report no saved secret.");

            AiSecretStore.Set("api-key:deepseek", "  sk-deepseek-roundtrip-secret  ");
            var secretFile = AiSecretStore.SecretPath("api-key:deepseek");
            Check(File.Exists(secretFile) && AiSecretStore.HasSecret("api-key:deepseek"),
                "Saving a non-empty secret must create an encrypted .bin file.");
            var rawText = File.ReadAllText(secretFile);
            Check(!rawText.Contains("sk-deepseek-roundtrip-secret", StringComparison.Ordinal),
                "DPAPI secret blob on disk must not contain plaintext secret material.");
            Check(AiSecretStore.Get("api-key:deepseek") == "sk-deepseek-roundtrip-secret",
                "AiSecretStore must round-trip and trim UTF-8 secrets via Windows DPAPI.");

            AiSecretStore.Set("api-key:deepseek", "   ");
            Check(!File.Exists(secretFile) && !AiSecretStore.HasSecret("api-key:deepseek") && AiSecretStore.Get("api-key:deepseek") is null,
                "Setting a null or whitespace secret must delete the secret file.");

            var changedProviders = new List<AiProviders>();
            void OnChanged(AiProviders p) => changedProviders.Add(p);
            AiUsageService.CredentialsChanged += OnChanged;
            try
            {
                foreach (var (provider, sampleKey) in new[]
                         {
                             (AiProviders.DeepSeek, "ds-secret-key-1"),
                             (AiProviders.OpenRouter, "or-secret-key-2"),
                             (AiProviders.OpenCode, "oc-secret-key-3")
                         })
                {
                    Check(!AiUsageService.HasSavedSecret(provider), $"{provider} must start without a saved DPAPI secret.");
                    AiUsageService.SaveApiKey(provider, sampleKey);
                    Check(AiUsageService.HasSavedSecret(provider) && AiUsageService.HasCredential(provider),
                        $"{provider} must report a saved secret and valid credential after SaveApiKey.");
                    var readBack = provider switch
                    {
                        AiProviders.DeepSeek => AiUsageService.ReadDeepSeekToken(),
                        AiProviders.OpenRouter => AiUsageService.ReadOpenRouterToken(),
                        _ => AiUsageService.ReadOpenCodeToken()
                    };
                    Check(readBack == sampleKey, $"{provider} must read its DPAPI secret before environment/file fallbacks.");

                    AiUsageService.SaveApiKey(provider, null);
                    Check(!AiUsageService.HasSavedSecret(provider), $"{provider} must clear its DPAPI secret when SaveApiKey is passed null.");
                }
                Check(changedProviders.Count == 6
                    && changedProviders[0] == AiProviders.DeepSeek
                    && changedProviders[2] == AiProviders.OpenRouter
                    && changedProviders[4] == AiProviders.OpenCode,
                    "SaveApiKey must fire CredentialsChanged on save and clear.");
            }
            finally
            {
                AiUsageService.CredentialsChanged -= OnChanged;
            }

            foreach (var cliProvider in new[] { AiProviders.Claude, AiProviders.Codex, AiProviders.Copilot, AiProviders.OpenCode, AiProviders.Antigravity })
            {
                var signInInfo = AiUsageService.BuildSignInStartInfo(cliProvider);
                Check(signInInfo is not null && !signInInfo.CreateNoWindow && signInInfo.WindowStyle != ProcessWindowStyle.Hidden,
                    $"{cliProvider} interactive sign-in must use a visible window rather than HiddenCliStartInfo.");
            }

            // Windows Terminal ConPTY path and cmd.exe fallback for CLI Sign-In (so paste works in Claude TUI).
            var wtInfo = AiUsageService.BuildSignInStartInfo(AiProviders.Claude, @"C:\fake\wt.exe");
            Check(wtInfo is not null && wtInfo.FileName == @"C:\fake\wt.exe"
                && !wtInfo.CreateNoWindow && !wtInfo.UseShellExecute
                && wtInfo.ArgumentList.SequenceEqual(["-w", "new", "--", "cmd.exe", "/c", "claude", "auth", "login"]),
                "Claude CLI sign-in must launch inside Windows Terminal when wt.exe is available so bracketed paste works.");

            var cmdFallback = AiUsageService.BuildSignInStartInfo(AiProviders.Claude, "");
            Check(cmdFallback is not null && cmdFallback.FileName == "cmd.exe"
                && cmdFallback.UseShellExecute && cmdFallback.Arguments == "/c claude auth login",
                "CLI sign-in must fall back to UseShellExecute cmd.exe when Windows Terminal is absent.");

            // Secret store path hardening & oversized secret guard.
            var sanitizedPath = AiSecretStore.SecretPath("../../evil");
            var fullRoot = Path.GetFullPath(AiSecretStore.Root);
            if (!fullRoot.EndsWith(Path.DirectorySeparatorChar)) fullRoot += Path.DirectorySeparatorChar;
            Check(sanitizedPath.StartsWith(fullRoot, StringComparison.OrdinalIgnoreCase) && !Path.GetFileName(sanitizedPath).Contains("..", StringComparison.Ordinal),
                "AiSecretStore.SecretPath must neutralize traversal characters and stay within Root.");

            var oversizedPath = AiSecretStore.SecretPath("api-key:oversized");
            Directory.CreateDirectory(Path.GetDirectoryName(oversizedPath)!);
            File.WriteAllBytes(oversizedPath, new byte[(1 << 16) + 16]);
            Check(AiSecretStore.Get("api-key:oversized") is null,
                "AiSecretStore.Get must reject oversized (>64 KB) secret files cleanly.");
        }
        finally
        {
            AiSecretStore.Root = previousRoot;
            if (Directory.Exists(tempSecretRoot))
            {
                try { Directory.Delete(tempSecretRoot, recursive: true); } catch (IOException) { }
            }
        }

        // GitHub CLI (`gh auth login`) fallback for Copilot, isolated from the real hosts.yml and keyring.
        var hosts = AiUsageService.ParseGhHostsYaml("""
            enterprise.example.com:
                user: enterprise-user
                oauth_token: fixture-enterprise-token
            github.com:
                users:
                    fixture-user:
                        oauth_token: fixture-nested-token
                git_protocol: https
                user: fixture-user
            """);
        Check(hosts == ("fixture-user", "fixture-nested-token"),
            "hosts.yml parsing must read the github.com user and its token while ignoring other hosts.");
        var directHosts = AiUsageService.ParseGhHostsYaml("github.com:\r\n  oauth_token: \"fixture-direct\"  \r\n  user: 'quoted-user' # comment\r\nother.host:\r\n  user: nope\r\n");
        Check(directHosts == ("quoted-user", "fixture-direct"), "hosts.yml parsing must handle quotes, comments and CRLF.");
        Check(AiUsageService.ParseGhHostsYaml("other.host:\n  user: nope\n  oauth_token: nope\n") == (null, null)
            && AiUsageService.ParseGhHostsYaml("") == (null, null), "hosts.yml without github.com must yield nothing.");

        var fixtureGho = "gho_" + new string('x', 36);
        var encodedGho = "go-keyring-base64:" + Convert.ToBase64String(System.Text.Encoding.UTF8.GetBytes(fixtureGho));
        Check(AiUsageService.DecodeGhKeyringToken(fixtureGho) == fixtureGho
            && AiUsageService.DecodeGhKeyringToken(encodedGho) == fixtureGho
            && AiUsageService.DecodeGhKeyringToken("  " + encodedGho + "\0\0\n") == fixtureGho
            && AiUsageService.DecodeGhKeyringToken(fixtureGho + "\0 \r\n") == fixtureGho
            && AiUsageService.DecodeGhKeyringToken(string.Join("\0", fixtureGho.ToCharArray()) + "\0") == fixtureGho,
            "gh keyring tokens must decode plain, base64-prefixed, padded and UTF-16-as-UTF-8 blobs.");
        Check(AiUsageService.DecodeGhKeyringToken("go-keyring-base64:!!not-base64!!") is null
            && AiUsageService.DecodeGhKeyringToken("go-keyring-base64:") is null
            && AiUsageService.DecodeGhKeyringToken(null) is null && AiUsageService.DecodeGhKeyringToken(" \0 ") is null,
            "Invalid or empty gh keyring blobs must fail closed.");

        var previousHostsOverride = AiUsageService.GhHostsPathOverride;
        var previousKeyringOverride = AiUsageService.GhKeyringReaderOverride;
        var previousProbeOverride = AiUsageService.GhCliTokenProbeOverride;
        var ghHostsPath = Path.Combine(Path.GetTempPath(), $"opentaskbar-gh-hosts-{Guid.NewGuid():N}.yml");
        try
        {
            var keyringReads = new List<string>();
            var keyring = new Dictionary<string, (string? Value, DateTime LastWrittenUtc)>();
            var probeCalls = 0;
            AiUsageService.GhHostsPathOverride = ghHostsPath;
            AiUsageService.GhKeyringReaderOverride = target =>
            {
                keyringReads.Add(target);
                return keyring.TryGetValue(target, out var entry) ? entry : (null, DateTime.MinValue);
            };
            AiUsageService.GhCliTokenProbeOverride = () => { probeCalls++; return "fixture-cli-token"; };

            Check(AiUsageService.TryReadGitHubCliCredential(false, out var emptyStamp) is null && emptyStamp == DateTime.MinValue
                && keyringReads.SequenceEqual(["gh:github.com:"]) && probeCalls == 0,
                "Without hosts.yml only the default gh keyring target is read and no CLI probe runs.");

            File.WriteAllText(ghHostsPath, "github.com:\n    user: fixture-user\n    git_protocol: https\n");
            var userWritten = new DateTime(2030, 1, 2, 3, 4, 5, DateTimeKind.Utc);
            keyring["gh:github.com:fixture-user"] = (encodedGho, userWritten);
            keyringReads.Clear();
            Check(AiUsageService.TryReadGitHubCliCredential(false, out var userStamp) == fixtureGho && userStamp == userWritten
                && keyringReads.SequenceEqual(["gh:github.com:", "gh:github.com:fixture-user"]) && probeCalls == 0,
                "The per-user gh keyring target must be read after the default one and decoded.");

            keyring["gh:github.com:"] = (fixtureGho + "-default", DateTime.MinValue);
            Check(AiUsageService.TryReadGitHubCliCredential(false, out _) == fixtureGho + "-default",
                "The default gh keyring target must win over the per-user one.");

            keyring.Clear();
            File.WriteAllText(ghHostsPath, "github.com:\n    user: fixture-user\n    oauth_token: fixture-plain-token\n");
            Check(AiUsageService.TryReadGitHubCliCredential(true, out var plainStamp) == "fixture-plain-token"
                && plainStamp == File.GetLastWriteTimeUtc(ghHostsPath) && probeCalls == 0,
                "A plaintext hosts.yml token must be used when the keyring has none, before any CLI probe.");

            File.WriteAllText(ghHostsPath, "github.com:\n    user: fixture-user\n");
            Check(AiUsageService.TryReadGitHubCliCredential(false, out _) is null && probeCalls == 0,
                "allowCliProbe:false must never run `gh auth token`.");
            Check(AiUsageService.TryReadGitHubCliCredential(true, out _) == "fixture-cli-token" && probeCalls == 1,
                "allowCliProbe:true must fall back to `gh auth token`.");

            _ = AiUsageService.HasCredential(AiProviders.Copilot);
            _ = AiUsageService.CredentialStamp(AiProviders.Copilot);
            Check(probeCalls == 1, "HasCredential/CredentialStamp for Copilot must not invoke any CLI subprocess.");

            var stampBefore = AiUsageService.CredentialStamp(AiProviders.Copilot);
            keyring["gh:github.com:fixture-user"] = (encodedGho, DateTime.UtcNow.AddYears(5));
            Check(AiUsageService.CredentialStamp(AiProviders.Copilot) > stampBefore,
                "CredentialStamp(Copilot) must change when `gh auth login` writes the keyring.");

            var ghStartInfo = AiUsageService.HiddenCliStartInfo("gh", "auth", "token");
            Check(ghStartInfo.ArgumentList.SequenceEqual(["auth", "token"]) && ghStartInfo.CreateNoWindow && ghStartInfo.RedirectStandardInput,
                "`gh auth token` must run hidden with redirected stdin.");
        }
        finally
        {
            AiUsageService.GhHostsPathOverride = previousHostsOverride;
            AiUsageService.GhKeyringReaderOverride = previousKeyringOverride;
            AiUsageService.GhCliTokenProbeOverride = previousProbeOverride;
            File.Delete(ghHostsPath);
        }

        // Widgets need an STA thread; this also runs headless, without application resources.
        Exception? renderFailure = null;
        var thread = new Thread(() =>
        {
            try { checks += EmptyRenderChecks(); }
            catch (Exception ex) { renderFailure = ex; }
            finally { System.Windows.Threading.Dispatcher.CurrentDispatcher.InvokeShutdown(); }
        });
        thread.SetApartmentState(ApartmentState.STA);
        thread.Start();
        thread.Join();
        GC.Collect();
        GC.WaitForPendingFinalizers();
        if (renderFailure is not null) System.Runtime.ExceptionServices.ExceptionDispatchInfo.Capture(renderFailure).Throw();
        return checks;
    }

    private static int EmptyRenderChecks()
    {
        var checks = 0;
        var pending = new TaskCompletionSource<(AiUsageReport?, string?)>();
        Func<AiProviders, CancellationToken, Task<(AiUsageReport?, string?)>>[] loaders =
        [
            (_, _) => pending.Task,
            (provider, _) => Task.FromResult<(AiUsageReport?, string?)>((new(provider, null, [], DateTimeOffset.UtcNow), null))
        ];
        foreach (var provider in new[] { AiProviders.None, AiProviders.All })
        {
            try { _ = new AiUsageWidget(provider); }
            catch (ArgumentOutOfRangeException) { checks++; continue; }
            throw new InvalidOperationException("An AI usage widget must require exactly one provider.");
        }
        foreach (var loader in loaders)
            foreach (var provider in new[] { AiProviders.OpenCode, AiProviders.Copilot, AiProviders.DeepSeek, AiProviders.OpenRouter, AiProviders.Cursor })
                foreach (var compact in new[] { false, true })
                {
                    using var widget = new AiUsageWidget(provider, loader);
                    widget.Activate();
                    widget.SetCompactState(compact);
                    widget.Tick(DateTime.Now.AddMinutes(1));
                    if (widget.BuildHoverFlyoutContent() is null)
                        throw new InvalidOperationException($"{provider} must use the shared hover flyout.");
                    widget.SetCompactState(!compact);
                    checks++;
                }

        // Dual quota widget checks (Codex, Claude): dual progress bars, no ring, no flyout menu, localized 5h/7d labels
        foreach (var dualProvider in new[] { AiProviders.Codex, AiProviders.Claude })
        {
            using var dualWidget = new AiUsageWidget(dualProvider, (provider, _) =>
            {
                var report = new AiUsageReport(provider, "Pro",
                [
                    new("AiUsageSession", 0.50, DateTimeOffset.UtcNow.AddHours(2), 18000L),
                    new("AiUsageWeekly", 0.25, DateTimeOffset.UtcNow.AddDays(4), 604800L)
                ], DateTimeOffset.UtcNow);
                return Task.FromResult<(AiUsageReport?, string?)>((report, null));
            });
            dualWidget.Activate();
            if (dualWidget.BuildHoverFlyoutContent() is null)
                throw new InvalidOperationException($"{dualProvider} must use the shared hover flyout.");

            var cardField = typeof(AiUsageWidget).GetField("_card", System.Reflection.BindingFlags.NonPublic | System.Reflection.BindingFlags.Instance)!;
            var cardObj = cardField.GetValue(dualWidget)!;
            var ringProp = cardObj.GetType().GetProperty("Ring")!;
            if (ringProp.GetValue(cardObj) is not null)
                throw new InvalidOperationException($"{dualProvider} must not use a RingMeter.");

            var codexBarsProp = cardObj.GetType().GetProperty("Codex")!;
            var codexBars = codexBarsProp.GetValue(cardObj);
            if (codexBars is null)
                throw new InvalidOperationException($"{dualProvider} must have dual quota bars container.");

            var sessionLabelProp = codexBars.GetType().GetProperty("SessionLabel")!;
            var sessionLabel = (TextBlock)sessionLabelProp.GetValue(codexBars)!;
            var weeklyLabelProp = codexBars.GetType().GetProperty("WeeklyLabel")!;
            var weeklyLabel = (TextBlock)weeklyLabelProp.GetValue(codexBars)!;
            var sessionRowProp = codexBars.GetType().GetProperty("SessionRow")!;
            var dualSessionRow = (StackPanel)sessionRowProp.GetValue(codexBars)!;
            var sessionResetProp = codexBars.GetType().GetProperty("SessionReset")!;
            var sessionReset = (TextBlock)sessionResetProp.GetValue(codexBars)!;
            var weeklyResetProp = codexBars.GetType().GetProperty("WeeklyReset")!;
            var weeklyReset = (TextBlock)weeklyResetProp.GetValue(codexBars)!;
            var sessionFillProp = codexBars.GetType().GetProperty("SessionFill")!;
            var sessionFill = (Border)sessionFillProp.GetValue(codexBars)!;
            var weeklyFillProp = codexBars.GetType().GetProperty("WeeklyFill")!;
            var weeklyFill = (Border)weeklyFillProp.GetValue(codexBars)!;

            // Turkish test
            UiText.Current.SetLanguage(AppLanguage.Turkish);
            dualWidget.Tick(DateTime.Now.AddMinutes(1));
            if (sessionLabel.Text != "5s" || weeklyLabel.Text != "7g")
                throw new InvalidOperationException($"{dualProvider} labels must be localized in Turkish: expected 5s/7g, got {sessionLabel.Text}/{weeklyLabel.Text}");
            if (sessionFill.Width <= 0 || weeklyFill.Width <= 0)
                throw new InvalidOperationException($"{dualProvider} progress bars must reflect loaded quota.");

            var dualBarsPanel = (StackPanel)dualSessionRow.Parent;
            dualBarsPanel.Measure(new System.Windows.Size(double.PositiveInfinity, double.PositiveInfinity));
            if (dualBarsPanel.DesiredSize.Height <= 0 || dualBarsPanel.DesiredSize.Height > 33)
                throw new InvalidOperationException($"{dualProvider} DualQuotaBars StackPanel height must fit inside 34 DIP card (<= 33), got {dualBarsPanel.DesiredSize.Height:F2}.");

            // English test
            UiText.Current.SetLanguage(AppLanguage.English);
            dualWidget.Tick(DateTime.Now.AddMinutes(2));
            if (sessionLabel.Text != "5h" || weeklyLabel.Text != "7d")
                throw new InvalidOperationException($"{dualProvider} labels must be localized in English: expected 5h/7d, got {sessionLabel.Text}/{weeklyLabel.Text}");

            // Check name visibility is collapsed
            var nameProp = cardObj.GetType().GetProperty("Name")!;
            var nameBlock = (TextBlock)nameProp.GetValue(cardObj)!;
            if (nameBlock.Visibility != System.Windows.Visibility.Collapsed)
                throw new InvalidOperationException($"{dualProvider} card must hide its provider name text.");

            UiText.Current.SetLanguage(AppLanguage.Turkish);
            checks += 8;
        }

        // Cursor bar + theme layout checks: matches Codex/Claude horizontal bar theme (no ring, hides name, single weekly/monthly row, no hardcoded cyan logo fill)
        {
            using var cursorBarWidget = new AiUsageWidget(AiProviders.Cursor, (provider, _) =>
            {
                var report = new AiUsageReport(provider, "Plus",
                [
                    new("AiUsageWeekly", 0.35, DateTimeOffset.UtcNow.AddDays(3), 604800L)
                ], DateTimeOffset.UtcNow);
                return Task.FromResult<(AiUsageReport?, string?)>((report, null));
            });
            cursorBarWidget.Activate();

            var cardField = typeof(AiUsageWidget).GetField("_card", System.Reflection.BindingFlags.NonPublic | System.Reflection.BindingFlags.Instance)!;
            var cardObj = cardField.GetValue(cursorBarWidget)!;
            var ringProp = cardObj.GetType().GetProperty("Ring")!;
            if (ringProp.GetValue(cardObj) is not null)
                throw new InvalidOperationException("Cursor must use the themed dual-bar card layout instead of a RingMeter.");

            var logoProp = cardObj.GetType().GetProperty("Logo")!;
            var logoPath = (System.Windows.Shapes.Path)logoProp.GetValue(cardObj)!;
            if (logoPath.ReadLocalValue(System.Windows.Shapes.Shape.FillProperty) is System.Windows.Media.SolidColorBrush)
                throw new InvalidOperationException("Cursor logo fill must follow the active theme resource rather than a hardcoded brand brush.");

            var nameProp = cardObj.GetType().GetProperty("Name")!;
            var nameBlock = (TextBlock)nameProp.GetValue(cardObj)!;
            if (nameBlock.Visibility != System.Windows.Visibility.Collapsed)
                throw new InvalidOperationException("Cursor card must hide its provider name text to match Codex and Claude.");

            var barsObj = cardObj.GetType().GetProperty("Codex")!.GetValue(cardObj)!;
            var sessionRow = (StackPanel)barsObj.GetType().GetProperty("SessionRow")!.GetValue(barsObj)!;
            var weeklyRow = (StackPanel)barsObj.GetType().GetProperty("WeeklyRow")!.GetValue(barsObj)!;
            var weeklyLabel = (TextBlock)barsObj.GetType().GetProperty("WeeklyLabel")!.GetValue(barsObj)!;
            var weeklyFill = (Border)barsObj.GetType().GetProperty("WeeklyFill")!.GetValue(barsObj)!;

            UiText.Current.SetLanguage(AppLanguage.Turkish);
            cursorBarWidget.Tick(DateTime.Now.AddMinutes(1));
            if (sessionRow.Visibility != System.Windows.Visibility.Collapsed
                || weeklyRow.Visibility != System.Windows.Visibility.Visible
                || weeklyLabel.Text != "7g"
                || weeklyFill.Width <= 0)
            {
                throw new InvalidOperationException("Cursor card must show only its localized weekly/monthly progress bar row.");
            }

            UiText.Current.SetLanguage(AppLanguage.English);
            cursorBarWidget.Tick(DateTime.Now.AddMinutes(2));
            if (weeklyLabel.Text != "7d")
                throw new InvalidOperationException($"Cursor weekly label must localize in English, got {weeklyLabel.Text}.");

            var weeklyTrack = (Border)barsObj.GetType().GetProperty("WeeklyTrack")!.GetValue(barsObj)!;
            var weeklyReset = (TextBlock)barsObj.GetType().GetProperty("WeeklyReset")!.GetValue(barsObj)!;
            var fullResetText = weeklyReset.Text;
            // "35" can legitimately appear in the reset *time* depending on the wall clock, so assert on
            // the compact-percentage marker instead of bare digits (#173 CI triage).
            if (weeklyLabel.Visibility != System.Windows.Visibility.Visible || weeklyTrack.Width != 38
                || fullResetText.Contains('%') || !fullResetText.Contains(':'))
                throw new InvalidOperationException("Cursor full card must show the 7d label, 38px track, and reset time.");

            cursorBarWidget.SetCompactState(true);
            cursorBarWidget.Tick(DateTime.Now.AddMinutes(3));
            if (weeklyLabel.Visibility != System.Windows.Visibility.Collapsed || weeklyTrack.Width != 26
                || weeklyFill.Width > 26 || weeklyFill.Width <= 0
                || weeklyReset.Text != UiText.Format("Percent", "35"))
                throw new InvalidOperationException($"Cursor compact card must collapse the label, shrink the track to 26px and show the percentage, got '{weeklyReset.Text}'.");

            cursorBarWidget.SetCompactState(false);
            if (weeklyLabel.Visibility != System.Windows.Visibility.Visible || weeklyTrack.Width != 38
                || weeklyReset.Text != fullResetText || weeklyLabel.Text != "7d")
                throw new InvalidOperationException("Cursor full card must restore the 7d label, 38px track, and reset time after compact.");

            UiText.Current.SetLanguage(AppLanguage.Turkish);
            checks += 9;
        }

        // Antigravity widget checks: dynamic columns with vertical dividers, headers on top, no empty quotas, no name, no ring, no flyout menu
        using var agyWidget = new AiUsageWidget(AiProviders.Antigravity, (provider, _) =>
        {
            var agyReport = new AiUsageReport(provider, "Google AI Pro",
            [
                new("AiUsageSession", 0.40, DateTimeOffset.UtcNow.AddHours(3), 18000L, Group: "AiUsageGroupGemini"),
                new("AiUsageWeekly", 0.60, DateTimeOffset.UtcNow.AddDays(5), 604800L, Group: "AiUsageGroupGemini"),
                // Claude group only has weekly quota, no 5h session data
                new("AiUsageWeekly", 0.30, DateTimeOffset.UtcNow.AddDays(4), 604800L, Group: "AiUsageGroupClaudeGpt")
            ], DateTimeOffset.UtcNow);
            return Task.FromResult<(AiUsageReport?, string?)>((agyReport, null));
        });
        agyWidget.Activate();
        if (agyWidget.BuildHoverFlyoutContent() is null)
            throw new InvalidOperationException("Antigravity must use the shared hover flyout.");

        var agyCardField = typeof(AiUsageWidget).GetField("_card", System.Reflection.BindingFlags.NonPublic | System.Reflection.BindingFlags.Instance)!;
        var agyCardObj = agyCardField.GetValue(agyWidget)!;
        var agyRingProp = agyCardObj.GetType().GetProperty("Ring")!;
        if (agyRingProp.GetValue(agyCardObj) is not null)
            throw new InvalidOperationException("Antigravity must not use a RingMeter.");

        var agyProp = agyCardObj.GetType().GetProperty("Antigravity")!;
        var agyCard = agyProp.GetValue(agyCardObj);
        if (agyCard is null)
            throw new InvalidOperationException("Antigravity must have AntigravityCard container.");

        var agyNameProp = agyCardObj.GetType().GetProperty("Name")!;
        var agyNameBlock = (TextBlock)agyNameProp.GetValue(agyCardObj)!;
        if (agyNameBlock.Visibility != System.Windows.Visibility.Collapsed)
            throw new InvalidOperationException("Antigravity card must hide its provider name text.");

        var containerProp = agyCard.GetType().GetProperty("Container")!;
        var container = (StackPanel)containerProp.GetValue(agyCard)!;
        // 2 groups (Gemini, Claude) separated by 1 divider = 3 children in container
        if (container.Children.Count != 3)
            throw new InvalidOperationException($"Antigravity container must have 3 children (2 columns + 1 divider), got {container.Children.Count}");

        var columnsProp = agyCard.GetType().GetProperty("Columns")!;
        var columnsList = (System.Collections.IList)columnsProp.GetValue(agyCard)!;
        if (columnsList.Count != 2)
            throw new InvalidOperationException($"Antigravity must have 2 columns, got {columnsList.Count}");

        // Column 1 (Gemini) has 2 slots (5h + weekly) and header "Gemini" on top
        var col1 = columnsList[0]!;
        var col1SlotsProp = col1.GetType().GetProperty("Slots")!;
        var col1Slots = (System.Collections.IList)col1SlotsProp.GetValue(col1)!;
        if (col1Slots.Count != 2)
            throw new InvalidOperationException($"Gemini column must have 2 quota slots, got {col1Slots.Count}");
        var col1ElemProp = col1.GetType().GetProperty("Element")!;
        var col1Panel = (StackPanel)col1ElemProp.GetValue(col1)!;
        if (col1Panel.Orientation != Orientation.Vertical || ((TextBlock)col1Panel.Children[0]).Text != "Gemini")
            throw new InvalidOperationException("Gemini column must be vertical StackPanel with 'Gemini' header on top.");
        if (col1Panel.VerticalAlignment != System.Windows.VerticalAlignment.Center || container.VerticalAlignment != System.Windows.VerticalAlignment.Center)
            throw new InvalidOperationException("Antigravity column and container must be vertically centered.");
        var agyButton = (Button)agyCardObj.GetType().GetProperty("Button")!.GetValue(agyCardObj)!;
        var agyRow = (StackPanel)agyButton.Content;
        if (agyRow.VerticalAlignment != System.Windows.VerticalAlignment.Center)
            throw new InvalidOperationException("Antigravity card row must be vertically centered.");
        col1Panel.Measure(new System.Windows.Size(double.PositiveInfinity, double.PositiveInfinity));
        if (col1Panel.DesiredSize.Height <= 0 || col1Panel.DesiredSize.Height > 33)
            throw new InvalidOperationException($"Antigravity Full Gemini column (header + 5s + 7g) must measure <= 33 DIP to avoid vertical clipping in 34 DIP cards, got {col1Panel.DesiredSize.Height:F2}.");
        agyRow.Measure(new System.Windows.Size(double.PositiveInfinity, double.PositiveInfinity));
        if (agyRow.DesiredSize.Height <= 0 || agyRow.DesiredSize.Height > 33)
            throw new InvalidOperationException($"Antigravity Full card row must measure <= 33 DIP, got {agyRow.DesiredSize.Height:F2}.");

        // Column 2 (Claude) has only 1 slot (weekly only, no missing 5h slot) and header "Claude" on top
        var col2 = columnsList[1]!;
        var col2SlotsProp = col2.GetType().GetProperty("Slots")!;
        var col2Slots = (System.Collections.IList)col2SlotsProp.GetValue(col2)!;
        if (col2Slots.Count != 1)
            throw new InvalidOperationException($"Claude column must only have 1 quota slot when 5h data is missing, got {col2Slots.Count}");
        var col2ElemProp = col2.GetType().GetProperty("Element")!;
        var col2Panel = (StackPanel)col2ElemProp.GetValue(col2)!;
        if (col2Panel.Orientation != Orientation.Vertical || ((TextBlock)col2Panel.Children[0]).Text != "Claude")
            throw new InvalidOperationException("Claude column must be vertical StackPanel with 'Claude' header on top.");

        // Placeholder Antigravity widget (no report yet) must also measure <= 33 DIP for 2-row Gemini column
        using (var agyPlaceholderWidget = new AiUsageWidget(AiProviders.Antigravity, loaders[0]))
        {
            var phCardObj = agyCardField.GetValue(agyPlaceholderWidget)!;
            var phCard = agyProp.GetValue(phCardObj)!;
            var phCols = (System.Collections.IList)columnsProp.GetValue(phCard)!;
            var phGeminiCol = (StackPanel)col1ElemProp.GetValue(phCols[0]!)!;
            phGeminiCol.Measure(new System.Windows.Size(double.PositiveInfinity, double.PositiveInfinity));
            if (phGeminiCol.DesiredSize.Height <= 0 || phGeminiCol.DesiredSize.Height > 33)
                throw new InvalidOperationException($"Antigravity Full Gemini placeholder column must measure <= 33 DIP, got {phGeminiCol.DesiredSize.Height:F2}.");
        }

        // Full Antigravity group with 3 windows must cap card rows at 2 (keeping full list in flyout) and measure <= 33 DIP
        using (var agyThreeWinWidget = new AiUsageWidget(AiProviders.Antigravity, (provider, _) =>
        {
            var threeWinReport = new AiUsageReport(provider, "Google AI Pro",
            [
                new("AiUsageSession", 0.40, DateTimeOffset.UtcNow.AddHours(2), 18000L, Group: "AiUsageGroupGemini"),
                new("AiUsageWeekly", 0.65, DateTimeOffset.UtcNow.AddDays(5), 604800L, Group: "AiUsageGroupGemini"),
                new("AiUsageMonthly", 0.20, DateTimeOffset.UtcNow.AddDays(20), 2592000L, Group: "AiUsageGroupGemini")
            ], DateTimeOffset.UtcNow);
            return Task.FromResult<(AiUsageReport?, string?)>((threeWinReport, null));
        }))
        {
            agyThreeWinWidget.Activate();
            var twCardObj = agyCardField.GetValue(agyThreeWinWidget)!;
            var twCard = agyProp.GetValue(twCardObj)!;
            var twCols = (System.Collections.IList)columnsProp.GetValue(twCard)!;
            var twSlots = (System.Collections.IList)col1SlotsProp.GetValue(twCols[0]!)!;
            if (twSlots.Count != 2)
                throw new InvalidOperationException($"Antigravity Full group column must cap displayed card rows at 2 when 3+ windows are present, got {twSlots.Count}.");
            var twColPanel = (StackPanel)col1ElemProp.GetValue(twCols[0]!)!;
            twColPanel.Measure(new System.Windows.Size(double.PositiveInfinity, double.PositiveInfinity));
            if (twColPanel.DesiredSize.Height <= 0 || twColPanel.DesiredSize.Height > 33)
                throw new InvalidOperationException($"Antigravity Full 3-window group column must measure <= 33 DIP, got {twColPanel.DesiredSize.Height:F2}.");
            if (agyThreeWinWidget.BuildHoverFlyoutContent() is null)
                throw new InvalidOperationException("Antigravity 3-window hover flyout must still build with all windows.");
        }

        // Now test 3 pools format
        using var agyPoolWidget = new AiUsageWidget(AiProviders.Antigravity, (provider, _) =>
        {
            var agyPoolReport = new AiUsageReport(provider, "Google AI Pro",
            [
                new("AiUsagePool:Gemini Pro", 0.45, DateTimeOffset.UtcNow.AddHours(2), 18000L),
                new("AiUsagePool:Gemini Flash", 0.20, DateTimeOffset.UtcNow.AddHours(3), 18000L),
                new("AiUsagePool:Claude", 0.95, DateTimeOffset.UtcNow.AddMinutes(45), 18000L)
            ], DateTimeOffset.UtcNow);
            return Task.FromResult<(AiUsageReport?, string?)>((agyPoolReport, null));
        });
        agyPoolWidget.Activate();
        var agyPoolCardObj = agyCardField.GetValue(agyPoolWidget)!;
        var agyPoolCard = agyProp.GetValue(agyPoolCardObj)!;
        var poolContainer = (StackPanel)containerProp.GetValue(agyPoolCard)!;
        // 3 pools separated by 2 dividers = 5 children
        if (poolContainer.Children.Count != 5)
            throw new InvalidOperationException($"Antigravity pool container must have 5 children (3 columns + 2 dividers), got {poolContainer.Children.Count}");
        var poolColumnsList = (System.Collections.IList)columnsProp.GetValue(agyPoolCard)!;
        var poolCol0 = (StackPanel)col1ElemProp.GetValue(poolColumnsList[0]!)!;
        poolCol0.Measure(new System.Windows.Size(double.PositiveInfinity, double.PositiveInfinity));
        if (poolCol0.DesiredSize.Height <= 0 || poolCol0.DesiredSize.Height > 33)
            throw new InvalidOperationException($"Antigravity Pool column must measure <= 33 DIP to avoid vertical clipping, got {poolCol0.DesiredSize.Height:F2}.");

        checks += 16;

        // Width estimate checks: CompactWidthEstimate must be strictly smaller than FullWidthEstimate for every AI provider
        foreach (var provider in new[]
        {
            AiProviders.Claude, AiProviders.Codex, AiProviders.Antigravity,
            AiProviders.OpenCode, AiProviders.Copilot, AiProviders.DeepSeek, AiProviders.OpenRouter
        })
        {
            using var w = new AiUsageWidget(provider, loaders[1]);
            if (w.CompactWidthEstimate >= w.FullWidthEstimate)
                throw new InvalidOperationException($"{provider} CompactWidthEstimate ({w.CompactWidthEstimate}) must be smaller than FullWidthEstimate ({w.FullWidthEstimate}).");
            checks++;
        }

        // DualQuotaBars (Claude/Codex) AiUsageRing + Compact + HoverExpand checks
        {
            using var claudeWidget = new AiUsageWidget(AiProviders.Claude, (provider, _) =>
            {
                var report = new AiUsageReport(provider, "Max",
                [
                    new("AiUsageSession", 0.30, DateTimeOffset.UtcNow.AddHours(2), 18000L),
                    new("AiUsageWeekly", 0.65, DateTimeOffset.UtcNow.AddDays(4), 604800L)
                ], DateTimeOffset.UtcNow);
                return Task.FromResult<(AiUsageReport?, string?)>((report, null));
            });
            claudeWidget.Activate();
            UiText.Current.SetLanguage(AppLanguage.English);

            var cardObj = agyCardField.GetValue(claudeWidget)!;
            var bars = cardObj.GetType().GetProperty("Codex")!.GetValue(cardObj)!;
            var sessionRow = (StackPanel)bars.GetType().GetProperty("SessionRow")!.GetValue(bars)!;
            var weeklyRow = (StackPanel)bars.GetType().GetProperty("WeeklyRow")!.GetValue(bars)!;
            var sessionTrack = (Border)bars.GetType().GetProperty("SessionTrack")!.GetValue(bars)!;
            var weeklyTrack = (Border)bars.GetType().GetProperty("WeeklyTrack")!.GetValue(bars)!;
            var sessionReset = (TextBlock)bars.GetType().GetProperty("SessionReset")!.GetValue(bars)!;
            var weeklyReset = (TextBlock)bars.GetType().GetProperty("WeeklyReset")!.GetValue(bars)!;

            // Full + Session => only SessionRow visible
            claudeWidget.ApplySettings(new DockSettings(Language: AppLanguage.English, AiUsageRing: AiUsageRing.Session));
            if (sessionRow.Visibility != System.Windows.Visibility.Visible || weeklyRow.Visibility != System.Windows.Visibility.Collapsed)
                throw new InvalidOperationException("Claude Full+Session must show only SessionRow.");

            // Full + Weekly => only WeeklyRow visible
            claudeWidget.ApplySettings(new DockSettings(Language: AppLanguage.English, AiUsageRing: AiUsageRing.Weekly));
            if (sessionRow.Visibility != System.Windows.Visibility.Collapsed || weeklyRow.Visibility != System.Windows.Visibility.Visible)
                throw new InvalidOperationException("Claude Full+Weekly must show only WeeklyRow.");

            // Compact + Highest => single row with higher Used (weekly 65% > session 30%), 26px bar track, concise percentage text preserved across Tick
            claudeWidget.ApplySettings(new DockSettings(Language: AppLanguage.English, AiUsageRing: AiUsageRing.Highest));
            claudeWidget.SetCompactState(true);
            if (sessionRow.Visibility != System.Windows.Visibility.Collapsed || weeklyRow.Visibility != System.Windows.Visibility.Visible)
                throw new InvalidOperationException("Claude Compact+Highest must show only the fuller row (WeeklyRow 65%).");
            if (Math.Abs(weeklyTrack.Width - 26) > 0.01)
                throw new InvalidOperationException($"Claude Compact bar track width must be 26px, got {weeklyTrack.Width}.");
            if (weeklyReset.Text != "65%")
                throw new InvalidOperationException($"Claude Compact reset text must be '65%', got '{weeklyReset.Text}'.");
            claudeWidget.Tick(DateTime.Now.AddMinutes(3));
            if (weeklyReset.Text != "65%")
                throw new InvalidOperationException($"Claude Compact percentage text must survive OnTick countdown update, got '{weeklyReset.Text}'.");

            // Hover expand while compact temporarily restores Full two-row 38px layout
            claudeWidget.SetHoverExpanded(true);
            if (sessionRow.Visibility != System.Windows.Visibility.Visible || weeklyRow.Visibility != System.Windows.Visibility.Visible || Math.Abs(sessionTrack.Width - 38) > 0.01)
                throw new InvalidOperationException("Hover-expanding a compact Claude widget must render Full two-row 38px bars.");
            claudeWidget.SetHoverExpanded(false);
            checks += 5;
        }

        // Antigravity groups filtering, placeholder fallback, AiUsageRing, and Compact mode checks
        {
            using var agyFilterWidget = new AiUsageWidget(AiProviders.Antigravity, (provider, _) =>
            {
                var report = new AiUsageReport(provider, "Google AI Pro",
                [
                    new("AiUsageSession", 0.25, DateTimeOffset.UtcNow.AddHours(3), 18000L, Group: "AiUsageGroupGemini"),
                    new("AiUsageWeekly", 0.55, DateTimeOffset.UtcNow.AddDays(5), 604800L, Group: "AiUsageGroupGemini"),
                    new("AiUsageSession", 0.80, DateTimeOffset.UtcNow.AddHours(1), 18000L, Group: "AiUsageGroupClaudeGpt"),
                    new("AiUsageWeekly", 0.40, DateTimeOffset.UtcNow.AddDays(4), 604800L, Group: "AiUsageGroupClaudeGpt")
                ], DateTimeOffset.UtcNow);
                return Task.FromResult<(AiUsageReport?, string?)>((report, null));
            });
            agyFilterWidget.Activate();
            UiText.Current.SetLanguage(AppLanguage.English);

            var cardObj = agyCardField.GetValue(agyFilterWidget)!;
            var card = agyProp.GetValue(cardObj)!;
            var cols = (System.Collections.IList)columnsProp.GetValue(card)!;

            // Filter to Gemini only => 1 column
            agyFilterWidget.ApplySettings(new DockSettings(Language: AppLanguage.English, AntigravityGroups: AntigravityGroups.Gemini));
            if (cols.Count != 1)
                throw new InvalidOperationException($"Antigravity filtered to Gemini must have 1 column, got {cols.Count}.");

            // Filter to Argon only (when report has no Argon windows) => must build Argon placeholder column instead of empty card
            agyFilterWidget.ApplySettings(new DockSettings(Language: AppLanguage.English, AntigravityGroups: AntigravityGroups.Argon));
            if (cols.Count != 1)
                throw new InvalidOperationException($"Antigravity filtered to missing Argon group must show 1 Argon placeholder column, got {cols.Count}.");
            var argonColPanel = (StackPanel)col1ElemProp.GetValue(cols[0]!)!;
            if (((TextBlock)argonColPanel.Children[0]).Text != "Argon")
                throw new InvalidOperationException("Antigravity missing Argon fallback must show 'Argon' header.");
            argonColPanel.Measure(new System.Windows.Size(double.PositiveInfinity, double.PositiveInfinity));
            if (argonColPanel.DesiredSize.Height <= 0 || argonColPanel.DesiredSize.Height > 33)
                throw new InvalidOperationException($"Antigravity Placeholder column must measure <= 33 DIP, got {argonColPanel.DesiredSize.Height:F2}.");

            // Compact + All => 2 matched columns, each with 1 slot, 22px bar width, and concise percentage text preserved across Tick
            agyFilterWidget.ApplySettings(new DockSettings(Language: AppLanguage.English, AntigravityGroups: AntigravityGroups.All, AiUsageRing: AiUsageRing.Highest));
            agyFilterWidget.SetCompactState(true);
            if (cols.Count != 2)
                throw new InvalidOperationException($"Antigravity Compact+All with Gemini & Claude data must have 2 columns, got {cols.Count}.");
            var compactCol0 = (StackPanel)col1ElemProp.GetValue(cols[0]!)!;
            compactCol0.Measure(new System.Windows.Size(double.PositiveInfinity, double.PositiveInfinity));
            if (compactCol0.DesiredSize.Height <= 0 || compactCol0.DesiredSize.Height > 33)
                throw new InvalidOperationException($"Antigravity Compact column must measure <= 33 DIP, got {compactCol0.DesiredSize.Height:F2}.");
            var compactSlots0 = (System.Collections.IList)col1SlotsProp.GetValue(cols[0]!)!;
            var compactSlots1 = (System.Collections.IList)col1SlotsProp.GetValue(cols[1]!)!;
            if (compactSlots0.Count != 1 || compactSlots1.Count != 1)
                throw new InvalidOperationException("Antigravity Compact mode must render exactly 1 row per enabled group.");
            var slot0Reset = (TextBlock)compactSlots0[0]!.GetType().GetProperty("Reset")!.GetValue(compactSlots0[0]!)!;
            if (slot0Reset.Text != "55%")
                throw new InvalidOperationException($"Antigravity Compact+Highest Gemini slot must show '55%', got '{slot0Reset.Text}'.");
            agyFilterWidget.Tick(DateTime.Now.AddMinutes(4));
            if (slot0Reset.Text != "55%")
                throw new InvalidOperationException($"Antigravity Compact percentage text must survive OnTick, got '{slot0Reset.Text}'.");
            checks += 7;
        }

        // OpenCode and Copilot/DeepSeek compact mode checks
        {
            using var ocWidget = new AiUsageWidget(AiProviders.OpenCode, (provider, _) =>
            {
                var report = new AiUsageReport(provider, "Go",
                [
                    new("AiUsageSession", 0.20, DateTimeOffset.UtcNow.AddHours(2), 18000L),
                    new("AiUsageWeekly", 0.70, DateTimeOffset.UtcNow.AddDays(3), 604800L),
                    new("AiUsageMonthly", 0.40, DateTimeOffset.UtcNow.AddDays(15), 2592000L)
                ], DateTimeOffset.UtcNow);
                return Task.FromResult<(AiUsageReport?, string?)>((report, null));
            });
            ocWidget.Activate();
            var ocCardObj = agyCardField.GetValue(ocWidget)!;
            var ocBars = ocCardObj.GetType().GetProperty("OpenCode")!.GetValue(ocCardObj)!;
            var hourCol = (StackPanel)ocBars.GetType().GetProperty("HourCol")!.GetValue(ocBars)!;
            var weekCol = (StackPanel)ocBars.GetType().GetProperty("WeekCol")!.GetValue(ocBars)!;
            var monthCol = (StackPanel)ocBars.GetType().GetProperty("MonthCol")!.GetValue(ocBars)!;
            var weekReset = (TextBlock)ocBars.GetType().GetProperty("WeekReset")!.GetValue(ocBars)!;

            hourCol.Measure(new System.Windows.Size(double.PositiveInfinity, double.PositiveInfinity));
            if (hourCol.DesiredSize.Height <= 0 || hourCol.DesiredSize.Height > 33)
                throw new InvalidOperationException($"OpenCode Full column height must fit inside 34 DIP card (<= 33), got {hourCol.DesiredSize.Height:F2}.");

            ocWidget.ApplySettings(new DockSettings(AiUsageRing: AiUsageRing.Session));
            if (hourCol.Visibility != System.Windows.Visibility.Visible || weekCol.Visibility != System.Windows.Visibility.Collapsed || monthCol.Visibility != System.Windows.Visibility.Collapsed)
                throw new InvalidOperationException("OpenCode Full+Session must show only HourCol.");

            ocWidget.ApplySettings(new DockSettings(AiUsageRing: AiUsageRing.Highest));
            ocWidget.SetCompactState(true);
            if (hourCol.Visibility != System.Windows.Visibility.Collapsed || weekCol.Visibility != System.Windows.Visibility.Visible || monthCol.Visibility != System.Windows.Visibility.Collapsed || weekReset.Visibility != System.Windows.Visibility.Collapsed)
                throw new InvalidOperationException("OpenCode Compact+Highest must show only the highest column (WeekCol 70%) and collapse reset text.");

            using var copilotWidget = new AiUsageWidget(AiProviders.Copilot, (provider, _) =>
            {
                var report = new AiUsageReport(provider, "Individual",
                [
                    new("AiUsageMonthly", 0.28, DateTimeOffset.UtcNow.AddDays(10), 2592000L)
                ], DateTimeOffset.UtcNow);
                return Task.FromResult<(AiUsageReport?, string?)>((report, null));
            });
            copilotWidget.Activate();
            var cpCardObj = agyCardField.GetValue(copilotWidget)!;
            var cpName = (TextBlock)cpCardObj.GetType().GetProperty("Name")!.GetValue(cpCardObj)!;
            var cpSimple = cpCardObj.GetType().GetProperty("Simple")!.GetValue(cpCardObj)!;
            var cpSep = (Border)cpSimple.GetType().GetProperty("Separator")!.GetValue(cpSimple)!;
            var cpMeta = (TextBlock)cpSimple.GetType().GetProperty("MetaText")!.GetValue(cpSimple)!;
            var cpMiniTrack = (Border)cpSimple.GetType().GetProperty("MiniBarTrack")!.GetValue(cpSimple)!;

            var cpValStack = (StackPanel)cpMeta.Parent;
            cpValStack.Measure(new System.Windows.Size(double.PositiveInfinity, double.PositiveInfinity));
            if (cpValStack.DesiredSize.Height <= 0 || cpValStack.DesiredSize.Height > 33)
                throw new InvalidOperationException($"Copilot Full value stack height must fit inside 34 DIP card (<= 33), got {cpValStack.DesiredSize.Height:F2}.");

            copilotWidget.SetCompactState(true);
            if (cpName.Visibility != System.Windows.Visibility.Collapsed
                || cpSep.Visibility != System.Windows.Visibility.Collapsed
                || cpMeta.Visibility != System.Windows.Visibility.Collapsed
                || cpMiniTrack.Visibility != System.Windows.Visibility.Collapsed)
                throw new InvalidOperationException("Copilot Compact mode must collapse Name, Separator, MetaText, and MiniBarTrack.");

            copilotWidget.SetCompactState(false);
            if (cpName.Visibility != System.Windows.Visibility.Visible
                || cpSep.Visibility != System.Windows.Visibility.Visible
                || cpMeta.Visibility != System.Windows.Visibility.Visible
                || cpMiniTrack.Visibility != System.Windows.Visibility.Visible)
                throw new InvalidOperationException("Copilot Full mode must restore Name, Separator, MetaText, and MiniBarTrack.");
            checks += 6;
        }

        UiText.Current.SetLanguage(AppLanguage.Turkish);

        // Cursor live-refresh on SessionChanged check (without touching user DPAPI store)
        var cursorLoads = 0;
        using (var cursorWidget = new AiUsageWidget(AiProviders.Cursor, (provider, _) =>
        {
            cursorLoads++;
            if (cursorLoads == 1)
                return Task.FromResult<(AiUsageReport?, string?)>((null, "ErrorAiUsageSignIn"));
            var report = new AiUsageReport(provider, "Plus",
                [new("AiUsageWeekly", 0.4, DateTimeOffset.UtcNow.AddDays(4), 604800L)], DateTimeOffset.UtcNow);
            return Task.FromResult<(AiUsageReport?, string?)>((report, null));
        }))
        {
            var prevPath = CursorAuthService.TokenFilePath;
            var tmpPath = Path.Combine(Path.GetTempPath(), $"ot-cursor-widget-{Guid.NewGuid():N}", "cursor-auth.bin");
            CursorAuthService.TokenFilePath = tmpPath;
            try
            {
                cursorWidget.Activate();
                if (cursorLoads != 1)
                    throw new InvalidOperationException("Cursor widget must perform an initial load on Activate.");
                CursorAuthService.SaveTokens("live-access", "live-refresh");
                if (cursorLoads != 2)
                    throw new InvalidOperationException("Cursor widget must reset sign-in backoff and reload immediately on SessionChanged.");
                checks += 2;
            }
            finally
            {
                CursorAuthService.TokenFilePath = prevPath;
                var dir = Path.GetDirectoryName(tmpPath);
                if (dir is not null && Directory.Exists(dir)) Directory.Delete(dir, recursive: true);
            }
        }

        pending.SetCanceled();
        return checks;
    }

    public static int RunUiChecks(Application application)
    {
        var checks = 0;
        void Check(bool condition, string message)
        {
            if (!condition) throw new InvalidOperationException(message);
            checks++;
        }

        var settingsPath = Path.Combine(Path.GetTempPath(), $"ot-ai-settings-ui-{Guid.NewGuid():N}.json");
        var secretRoot = Path.Combine(Path.GetTempPath(), $"ot-ai-secrets-ui-{Guid.NewGuid():N}");
        var previousRoot = AiSecretStore.Root;
        var previousLauncher = AiUsageService.ProcessLauncherOverride;
        AiSecretStore.Root = secretRoot;
        try
        {
            var store = new DockSettingsStore(settingsPath);
            Check(store.Save(new DockSettings(Language: AppLanguage.English)) is null,
                "Initial AI UI test settings must save cleanly.");
            var preferences = new ApplicationPreferences(store);
            var changeCount = 0;
            preferences.Changed += (_, _) => changeCount++;

            // Verify AiUsageWidget immediately re-polls on CredentialsChanged without waiting for the 15-minute cooldown.
            var loadCalls = 0;
            using (var liveWidget = new AiUsageWidget(AiProviders.DeepSeek, (_, _) =>
                   {
                       loadCalls++;
                       return Task.FromResult<(AiUsageReport?, string?)>((null, "ErrorAiUsageSignIn"));
                   }))
            {
                liveWidget.Activate();
                PumpDispatcher(application.Dispatcher, 60);
                var initialCalls = loadCalls;
                Check(initialCalls >= 1, "Activated AiUsageWidget must perform initial load.");
                AiUsageService.NotifyCredentialsChanged(AiProviders.DeepSeek);
                PumpDispatcher(application.Dispatcher, 60);
                Check(loadCalls > initialCalls,
                    "CredentialsChanged must bypass the sign-in cooldown and trigger an immediate reload.");
            }

            var sw = new SettingsWindow(preferences)
            {
                Left = -20000,
                Top = -20000,
                ShowActivated = false,
                ShowInTaskbar = false
            };
            try
            {
                sw.Show();
                PumpDispatcher(application.Dispatcher, 120);

                var optionsField = typeof(SettingsWindow).GetField("_widgetOptions",
                    System.Reflection.BindingFlags.NonPublic | System.Reflection.BindingFlags.Instance)!;
                var options = ((System.Collections.IEnumerable)optionsField.GetValue(sw)!).Cast<DockWidgetOption>().ToList();

                foreach (var def in DockWidgetService.AiUsageWidgets)
                {
                    var option = options.Single(o => o.Id == def.Id);
                    Check(option.SettingsView is Panel, $"{def.Id} must expose a SettingsView panel.");
                    var view = option.SettingsView!;

                    var section = FindTagged<FrameworkElement>(view, $"AiAccountSection:{def.Id}");
                    var statusText = FindTagged<TextBlock>(view, $"AiAccountStatus:{def.Provider}");
                    var refreshBtn = FindTagged<Button>(view, $"AiAccountRefresh:{def.Provider}");
                    Check(section is not null && statusText is not null && refreshBtn is not null,
                        $"{def.Id} settings panel must expose account status and refresh controls.");

                    if (AiUsageService.IsCliSignInProvider(def.Provider))
                    {
                        var signInBtn = FindTagged<Button>(view, $"AiAccountSignIn:{def.Provider}");
                        var cliHint = FindTagged<TextBlock>(view, $"AiAccountCliHint:{def.Provider}");
                        Check(signInBtn is not null && cliHint is not null && !string.IsNullOrWhiteSpace(cliHint.Text),
                            $"{def.Id} must expose CLI sign-in button and command hint.");
                    }

                    if (AiUsageService.IsApiKeyProvider(def.Provider))
                    {
                        var keyBox = FindTagged<PasswordBox>(view, $"AiAccountKeyBox:{def.Provider}");
                        var saveBtn = FindTagged<Button>(view, $"AiAccountKeySave:{def.Provider}");
                        var clearBtn = FindTagged<Button>(view, $"AiAccountKeyClear:{def.Provider}");
                        Check(keyBox is not null && saveBtn is not null && clearBtn is not null,
                            $"{def.Id} must expose PasswordBox, Save, and Clear controls.");
                    }
                }

                // Exercise DeepSeek API key Save & Clear via UI controls without mutating settings.json.
                var deepSeekView = options.Single(o => o.Id == "AiUsageDeepSeek").SettingsView!;
                var dsKeyBox = FindTagged<PasswordBox>(deepSeekView, $"AiAccountKeyBox:{AiProviders.DeepSeek}")!;
                var dsSaveBtn = FindTagged<Button>(deepSeekView, $"AiAccountKeySave:{AiProviders.DeepSeek}")!;
                var dsClearBtn = FindTagged<Button>(deepSeekView, $"AiAccountKeyClear:{AiProviders.DeepSeek}")!;
                var dsStatus = FindTagged<TextBlock>(deepSeekView, $"AiAccountStatus:{AiProviders.DeepSeek}")!;

                dsKeyBox.Password = "sk-ui-deepseek-secret";
                dsSaveBtn.RaiseEvent(new RoutedEventArgs(Button.ClickEvent));
                PumpDispatcher(application.Dispatcher, 60);

                Check(string.IsNullOrEmpty(dsKeyBox.Password), "Saving API key must clear the PasswordBox.");
                Check(AiUsageService.HasSavedSecret(AiProviders.DeepSeek) && AiUsageService.ReadDeepSeekToken() == "sk-ui-deepseek-secret",
                    "Saving API key from SettingsWindow must persist the DPAPI secret.");
                Check(dsStatus.Text == UiText.Get("AiUsageAccountConnected"),
                    "Account status must switch to Connected after saving an API key.");
                Check(!File.ReadAllText(settingsPath).Contains("sk-ui-deepseek-secret", StringComparison.Ordinal),
                    "API keys must never be written into settings.json.");
                Check(changeCount == 0,
                    "Saving an API key must not fire spurious preference edits.");

                dsClearBtn.RaiseEvent(new RoutedEventArgs(Button.ClickEvent));
                PumpDispatcher(application.Dispatcher, 60);
                Check(!AiUsageService.HasSavedSecret(AiProviders.DeepSeek),
                    "Clicking Clear must remove the saved DPAPI secret.");
                Check(changeCount == 0,
                    "Clearing an API key must not fire spurious preference edits.");

                // Exercise CLI Sign-In button safely via ProcessLauncherOverride.
                ProcessStartInfo? capturedSignIn = null;
                AiUsageService.ProcessLauncherOverride = psi => { capturedSignIn = psi; return true; };
                var claudeView = options.Single(o => o.Id == "AiUsageClaude").SettingsView!;
                var claudeSignInBtn = FindTagged<Button>(claudeView, $"AiAccountSignIn:{AiProviders.Claude}")!;
                claudeSignInBtn.RaiseEvent(new RoutedEventArgs(Button.ClickEvent));
                PumpDispatcher(application.Dispatcher, 40);
                var launchedCommandOk = capturedSignIn is not null
                    && !capturedSignIn.CreateNoWindow
                    && capturedSignIn.WindowStyle == ProcessWindowStyle.Normal
                    && (capturedSignIn.ArgumentList.SequenceEqual(["-w", "new", "--", "cmd.exe", "/c", "claude", "auth", "login"])
                        || (capturedSignIn.UseShellExecute && capturedSignIn.Arguments == "/c claude auth login"));
                Check(launchedCommandOk,
                    "Claude Sign-in button must launch an interactive visible CLI login command.");
                Check(changeCount == 0, "Clicking CLI Sign-in must not trigger preference saves.");
            }
            finally
            {
                sw.Close();
                PumpDispatcher(application.Dispatcher, 220);
            }

            // Firing CredentialsChanged after SettingsWindow closes must be a safe no-op (clean unsubscribe).
            AiUsageService.NotifyCredentialsChanged(AiProviders.Claude);
            PumpDispatcher(application.Dispatcher, 40);
            Check(changeCount == 0, "Closing SettingsWindow after AI panel checks must not mutate preferences.");
        }
        finally
        {
            AiUsageService.ProcessLauncherOverride = previousLauncher;
            AiSecretStore.Root = previousRoot;
            if (File.Exists(settingsPath)) File.Delete(settingsPath);
            if (File.Exists(settingsPath + ".tmp")) File.Delete(settingsPath + ".tmp");
            if (Directory.Exists(secretRoot))
            {
                try { Directory.Delete(secretRoot, recursive: true); } catch (IOException) { }
            }
        }

        return checks;
    }

    private static T? FindTagged<T>(DependencyObject root, string tag) where T : FrameworkElement
    {
        if (root is T match && Equals(match.Tag, tag)) return match;
        if (root is Panel panel)
        {
            foreach (var child in panel.Children)
            {
                if (child is DependencyObject dep && FindTagged<T>(dep, tag) is { } found) return found;
            }
        }
        else if (root is Border { Child: DependencyObject borderChild } && FindTagged<T>(borderChild, tag) is { } foundBorder)
        {
            return foundBorder;
        }
        else if (root is ContentControl { Content: DependencyObject contentChild } && FindTagged<T>(contentChild, tag) is { } foundContent)
        {
            return foundContent;
        }
        for (var i = 0; i < VisualTreeHelper.GetChildrenCount(root); i++)
        {
            if (FindTagged<T>(VisualTreeHelper.GetChild(root, i), tag) is { } foundVisual) return foundVisual;
        }
        return null;
    }

    private static void PumpDispatcher(Dispatcher dispatcher, int milliseconds)
    {
        var frame = new DispatcherFrame();
        var timer = new DispatcherTimer(DispatcherPriority.Background, dispatcher)
        {
            Interval = TimeSpan.FromMilliseconds(milliseconds)
        };
        timer.Tick += (_, _) =>
        {
            timer.Stop();
            frame.Continue = false;
        };
        timer.Start();
        Dispatcher.PushFrame(frame);
    }

    private sealed class StubHttpHandler(Func<HttpRequestMessage, HttpResponseMessage> responder) : HttpMessageHandler
    {
        protected override Task<HttpResponseMessage> SendAsync(HttpRequestMessage request, CancellationToken cancellationToken) =>
            Task.FromResult(responder(request));
    }
}
