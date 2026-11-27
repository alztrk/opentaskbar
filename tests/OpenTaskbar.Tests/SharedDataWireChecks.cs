using System.IO;
using System.IO.Compression;
using System.Text;
using OpenTaskbar;
using OpenTaskbar.Models;
using OpenTaskbar.Services;

/// <summary>
/// Faz 3: byte-level parity checks for the files C#, Rust and the C watchdog all read and write.
/// Every fixture is created in a throwaway temp folder; the real %LOCALAPPDATA% tree is never touched.
/// </summary>
internal static class SharedDataWireChecks
{
    internal static int Run()
    {
        var checks = 0;
        void Check(bool condition, string message)
        {
            if (!condition) throw new InvalidOperationException(message);
            checks++;
        }

        CheckSharedFolderAssumptions(Check);
        CheckSecretEntropyAndBackups(Check);
        CheckThemeFolderAndArchive(Check);
        CheckRenderModeWire(Check);
        CheckUpdaterWire(Check);
        return checks;
    }

    private static void CheckSharedFolderAssumptions(Action<bool, string> check)
    {
        var local = Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData);
        var shared = Path.Combine(local, "OpenTaskbar");
        check(string.Equals(Path.GetDirectoryName(ShellFlyoutPositionJournal.JournalFilePath), shared,
                StringComparison.OrdinalIgnoreCase)
              && string.Equals(Path.GetDirectoryName(TaskbarReplacementSession.JournalFilePath), shared,
                  StringComparison.OrdinalIgnoreCase)
              && string.Equals(Path.GetDirectoryName(RenderModePreference.PreferencePath), shared,
                  StringComparison.OrdinalIgnoreCase),
            "Journals and render-mode.json must live in the shared %LOCALAPPDATA%\\OpenTaskbar folder.");
        check(string.Equals(AiSecretStore.DefaultRoot, Path.Combine(shared, "secrets"),
                StringComparison.OrdinalIgnoreCase),
            "DPAPI secrets must live in OpenTaskbar\\secrets, the folder the Rust port reads.");
        check(string.Equals(Path.GetFileName(Path.TrimEndingDirectorySeparator(ThemePacks.Root)), "Themes",
                StringComparison.OrdinalIgnoreCase),
            "The user theme root must be the big-T 'Themes' folder shared with Rust.");

        // The fork mutex only arbitrates C# instances. The Rust fork uses its own singleton, so both can
        // run side by side; settings.json consistency comes from a SEPARATE short-lived shared lock.
        check(App.SingletonMutexName == @"Local\OpenTaskbar.Singleton"
              && App.DevSingletonMutexName == @"Local\OpenTaskbar.Dev"
              && App.SharedSettingsLockName == @"Local\OpenTaskbar.Settings.Lock"
              && App.SingletonMutexName != App.SharedSettingsLockName,
            "The C# singleton mutex must not be confused with the shared settings read-modify-write lock.");
    }

    private static void CheckSecretEntropyAndBackups(Action<bool, string> check)
    {
        // These literals are the Rust side of the DPAPI contract (telemetry.rs / cursor_auth.rs).
        check(AiSecretStore.Entropy == "OpenTaskbar.AiSecretStore.v1",
            "AI secret entropy must match Rust exactly or keys become undecryptable.");
        check(CursorAuthService.Entropy == "OpenTaskbar.CursorAuth.v1",
            "Cursor token entropy must match Rust exactly.");
        check(AiSecretStore.MaxSecretFileBytes == 65_536
              && Encoding.ASCII.GetBytes(AiSecretStore.Entropy).Length == 28
              && Encoding.ASCII.GetBytes(CursorAuthService.Entropy).Length == 25,
            "The 64 KiB cap and ASCII entropy bytes must match the Rust decryptors.");
        check(CursorAuthService.TokenFileName == "cursor-auth.bin" && AiSecretStore.SecretsFolderName == "secrets",
            "Both ports must use the same secrets folder and token file name.");

        var temp = Path.Combine(Path.GetTempPath(), "ot-shared-secrets-" + Guid.NewGuid().ToString("N"));
        var secrets = Path.Combine(temp, "secrets");
        var themes = Path.Combine(temp, "Themes");
        Directory.CreateDirectory(secrets);
        Directory.CreateDirectory(themes);
        var previousRoot = AiSecretStore.Root;
        try
        {
            AiSecretStore.Root = secrets;
            var path = AiSecretStore.SecretPath("api-key/open router");
            check(string.Equals(Path.GetDirectoryName(path), secrets, StringComparison.OrdinalIgnoreCase)
                  && Path.GetFileName(path) == "api-key_open_router.bin",
                "Unsafe key characters must be sanitized without escaping the secrets root.");

            // The blob need not be decryptable here: the backup allowlist must reject the file regardless.
            var marker = "OT-PHASE3-SECRET-" + Guid.NewGuid().ToString("N");
            File.WriteAllText(path, marker);
            File.WriteAllText(Path.Combine(secrets, "cursor-auth.bin"), marker);

            var settings = new DockSettings();
            var settingsJson = SettingsBackup.Serialize(settings, Array.Empty<PinBackup>());
            check(!settingsJson.Contains(marker, StringComparison.Ordinal),
                "A settings backup must never serialize a DPAPI secret.");

            var archivePath = Path.Combine(temp, "backup.otbackup");
            var error = FullBackupArchive.Export(archivePath, settings, themesRoot: themes,
                pinsOverride: Array.Empty<PinBackup>());
            check(error is null, "A settings-only full backup must export successfully.");
            using var archive = ZipFile.OpenRead(archivePath);
            var names = archive.Entries.Select(entry => entry.FullName).ToArray();
            check(names.Length == 2
                  && names.Contains(FullBackupArchive.ManifestFileName)
                  && names.Contains(FullBackupArchive.SettingsFileName),
                "A full backup may contain only the manifest and settings entries (plus theme files).");
            foreach (var entry in archive.Entries)
            {
                using var reader = new StreamReader(entry.Open(), Encoding.UTF8);
                check(!reader.ReadToEnd().Contains(marker, StringComparison.Ordinal),
                    $"Backup entry '{entry.FullName}' leaked a secret.");
            }
        }
        finally
        {
            AiSecretStore.Root = previousRoot;
            try { Directory.Delete(temp, recursive: true); } catch { }
        }
    }

    private static void CheckThemeFolderAndArchive(Action<bool, string> check)
    {
        var previousRoot = ThemePacks.Root;
        var root = Path.Combine(Path.GetTempPath(), "ot-shared-themes-" + Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(root);
        ThemePacks.Root = root;
        try
        {
            // Built-in installation must not overwrite anything the user already has.
            var userNord = "{\"name\":\"User Nord\",\"preserveOriginalIcons\":true}";
            Directory.CreateDirectory(Path.Combine(root, "nord"));
            var userManifest = Path.Combine(root, "nord", ThemePack.ManifestFileName);
            File.WriteAllText(userManifest, userNord);
            check(ThemePacks.InstallBuiltInThemes() is null
                  && File.ReadAllText(userManifest) == userNord
                  && File.Exists(Path.Combine(root, "catppuccin", ThemePack.ManifestFileName)),
                "Built-in themes must install into Themes/ without replacing user files.");

            // Rust writes pretty JSON with these exact keys; C# must read it unchanged.
            var themeDir = Path.Combine(root, "rust-theme");
            Directory.CreateDirectory(Path.Combine(themeDir, ThemePack.WidgetFolder));
            File.WriteAllText(Path.Combine(themeDir, ThemePack.ManifestFileName), """
                {
                  "name": "Rust Theme",
                  "author": "Rust",
                  "preserveOriginalIcons": true
                }
                """);
            File.WriteAllText(Path.Combine(themeDir, ThemePack.WidgetFolder, WidgetPack.ManifestFileName),
                "{\"tokens\":{\"WidgetAccent\":\"#FF00BFFF\"}}");
            var (loaded, error, problems) = ThemePack.Load(themeDir);
            check(error is null && problems.Count == 0
                  && loaded is { DisplayName: "Rust Theme", Widgets: not null }
                  && loaded.Manifest.PreserveOriginalIcons,
                "A Rust-written theme.json must load in C# with preserveOriginalIcons intact.");

            // C# writes the same keys; its .ottheme entries must be Rust-readable relative paths.
            var created = ThemePackTransfer.CreateTheme("Csharp Theme", "C#", "nord");
            var createdJson = File.ReadAllText(Path.Combine(root, created, ThemePack.ManifestFileName));
            check(createdJson.Contains("\"name\"", StringComparison.Ordinal)
                  && createdJson.Contains("\"preserveOriginalIcons\": true", StringComparison.Ordinal),
                "C# must serialize the theme manifest keys Rust reads.");
            var archivePath = Path.Combine(root, "csharp.ottheme");
            check(ThemePackTransfer.ExportTheme("rust-theme", archivePath) is null, "C# .ottheme export must succeed.");
            using (var archive = ZipFile.OpenRead(archivePath))
            {
                var entries = archive.Entries.Select(entry => entry.FullName).OrderBy(name => name).ToArray();
                check(entries.SequenceEqual(["rust-theme/theme.json", "rust-theme/widgets/pack.json"]),
                    "C# must export a single Rust-readable theme folder with forward-slash entry names.");
            }

            // A Rust-shaped .ottheme (one folder at the archive root) must import into C#.
            var rustArchive = Path.Combine(root, "rust.ottheme");
            using (var archive = ZipFile.Open(rustArchive, ZipArchiveMode.Create))
            {
                WriteEntry(archive, "rust-theme/theme.json", "{\"name\":\"Imported Rust Theme\"}");
                WriteEntry(archive, "rust-theme/widgets/pack.json", "{\"tokens\":{\"WidgetAccent\":\"#FF00BFFF\"}}");
            }
            var (importedId, importError) = ThemePackTransfer.ImportTheme(rustArchive);
            check(importError is null && importedId is not null
                  && File.Exists(Path.Combine(root, importedId, ThemePack.ManifestFileName)),
                "A Rust-exported .ottheme must import without a staging directory left behind.");

            var before = Directory.EnumerateDirectories(root, ".import-*").Count();
            var badArchive = Path.Combine(root, "bad.ottheme");
            using (var archive = ZipFile.Open(badArchive, ZipArchiveMode.Create))
            {
                WriteEntry(archive, "../escape.txt", "must be rejected");
            }
            var (badId, badError) = ThemePackTransfer.ImportTheme(badArchive);
            check(badId is null && badError is not null
                  && Directory.EnumerateDirectories(root, ".import-*").Count() == before,
                "A failed .ottheme import must report an error and clean up its .import-<guid> staging folder.");
        }
        finally
        {
            ThemePacks.Select(null);
            ThemePacks.Root = previousRoot;
            try { Directory.Delete(root, recursive: true); } catch { }
        }
    }

    private static void CheckRenderModeWire(Action<bool, string> check)
    {
        check(Path.GetFileName(RenderModePreference.PreferencePath) == "render-mode.json",
            "Render mode must live in the shared render-mode.json file.");
        var path = Path.Combine(Path.GetTempPath(), "ot-render-" + Guid.NewGuid().ToString("N") + ".json");
        try
        {
            // Rust set_software writes exactly these bytes; no BOM and no whitespace.
            File.WriteAllBytes(path, Encoding.UTF8.GetBytes("{\"software\":true}"));
            check(RenderModePreference.IsSoftwareFrom(path), "C# must read Rust's render-mode.json as software.");
            File.WriteAllBytes(path, Encoding.UTF8.GetBytes("{\"software\":false}"));
            check(!RenderModePreference.IsSoftwareFrom(path), "Rust's false preference must disable software rendering.");

            RenderModePreference.SetSoftwareTo(path, true);
            check(File.ReadAllText(path) == "{\"software\":true}" && RenderModePreference.IsSoftwareFrom(path),
                "C# must write the exact {\"software\":bool} key Rust reads.");
            RenderModePreference.SetSoftwareTo(path, false);
            check(File.ReadAllText(path) == "{\"software\":false}", "C# must round-trip the false preference.");
        }
        finally
        {
            try { File.Delete(path); } catch { }
        }
    }

    private static void CheckUpdaterWire(Action<bool, string> check)
    {
        check(string.Equals(UpdateChecker.UpdateTempDirectory,
                Path.Combine(Path.GetTempPath(), "OpenTaskbar", "update"), StringComparison.OrdinalIgnoreCase),
            "Updates must be staged under %TEMP%\\OpenTaskbar\\update, not the journals folder.");
        check(UpdateChecker.EscapeBatchValue(@"C:\100% Update\app.exe") == @"C:\100%% Update\app.exe",
            "A literal percent in an updater path must be doubled for cmd.exe.");
        var script = UpdateChecker.GenerateUpdaterScript(@"C:\Temp\100%\OpenTaskbar.exe", @"C:\Apps\50%\OpenTaskbar.exe");
        check(script.StartsWith("@echo off\r\nchcp 65001 >nul\r\n", StringComparison.Ordinal)
              && script.Contains(@"set ""SRC=C:\Temp\100%%\OpenTaskbar.exe""", StringComparison.Ordinal)
              && script.Contains(@"set ""DST=C:\Apps\50%%\OpenTaskbar.exe""", StringComparison.Ordinal),
            "The updater script must use CRLF, UTF-8 switch and escaped percent signs exactly like Rust.");
        check(UpdateChecker.UpdaterScriptEncoding.GetPreamble().Length == 0,
            "The updater script encoding must never emit a BOM.");
    }

    private static void WriteEntry(ZipArchive archive, string name, string content)
    {
        var entry = archive.CreateEntry(name, CompressionLevel.Optimal);
        using var writer = new StreamWriter(entry.Open(), new UTF8Encoding(encoderShouldEmitUTF8Identifier: false));
        writer.Write(content);
    }
}
