using System.Diagnostics;
using System.Runtime.InteropServices;
using System.Text;
using System.Windows;
using System.Windows.Interop;
using System.Windows.Media.Imaging;
using OpenTaskbar.Models;

namespace OpenTaskbar.Services;

internal sealed class NativeJumpListSession : IDisposable
{
    // Read-only ABI references: Open-Shell JumpLists.cpp and System Informer appresolverp.h.
    private static readonly Guid ResolverClass = new("660B90C8-73A9-4B58-8CAE-355B7F55341B");
    private static readonly Guid ResolverInterface = new("DE25675A-72DE-44B4-9373-05170450C140");
    private static readonly Guid DestinationClass = new("77F10CF0-3DB5-4966-B520-B7C54FD35ED6");
    private static readonly Guid DestinationInterface = new("507101CD-F6AD-46C8-8E20-EEB9E6BAC47F");
    private static readonly Guid AutomaticClass = new("F0AE1542-F497-484B-A175-A20DB09144BA");
    private static readonly Guid AutomaticInterface = new("E9C5EF8D-FD41-4F72-BA87-EB03BAD5817C");
    private static readonly Guid ShellItemInterface = new("43826D1E-E718-42EE-BC55-A1E261C37BFE");
    private static readonly Guid ObjectArrayInterface = new("92CA9DCD-5622-4BBA-A805-5E9F541BD8C9");
    private static readonly Guid PropertyStoreInterface = new("886D8EEB-8CF2-4446-8D02-CDBA1DBDCF99");
    private static readonly Guid ContextMenuInterface = new("000214E4-0000-0000-C000-000000000046");
    private static readonly Guid ImageFactoryInterface = new("BCC18B79-BA16-442F-80C4-8A59C30C463B");
    private static readonly Guid ShellLinkInterface = new("000214F9-0000-0000-C000-000000000046");
    private static readonly PropertyKey AppIdKey = new(new("9F4C2855-9F79-4B39-A8D0-E1D42DE1D5F3"), 5);
    private static readonly PropertyKey SeparatorKey = new(AppIdKey.Format, 6);
    private static readonly PropertyKey HostEnvironmentKey = new(AppIdKey.Format, 14);
    private static readonly PropertyKey ActivationContextKey = new(AppIdKey.Format, 20);
    private static readonly PropertyKey TitleKey = new(new("F29F85E0-4FF9-1068-AB91-08002B27B3D9"), 2);
    // Firefox/LibreWolf jump list tasks store -private-window in the property store (PKEY_Link_Arguments)
    // rather than in IShellLink::GetArguments, so a direct launch without this lookup loses the task flag.
    private static readonly PropertyKey LinkArgumentsKey = new(new("436F2667-14E2-4FEB-B30A-146C53B5B674"), 100);
    private const uint SLgpRawPath = 4;
    private readonly Dictionary<int, ShellComObject> _commands = [];
    private int _nextCommand;
    // The owning application window the jump list was read for. Shell activation contexts must be derived
    // from this window, not from OpenTaskbar's own hwnd, or single-instance apps (Firefox/LibreWolf) lose
    // task flags such as -private-window when the already-running instance handles the launch.
    private nint _targetWindow;

    public ApplicationJumpList Read(long sessionId, nint window, string? shortcut, CancellationToken cancellation)
    {
        if (IntPtr.Size != 8) return new(sessionId, JumpListStatus.Unsupported, []);
        _targetWindow = window;
        var appId = ResolveIdentity(window, shortcut);
        if (string.IsNullOrWhiteSpace(appId)) return new(sessionId, JumpListStatus.IdentityUnavailable, []);
        cancellation.ThrowIfCancellationRequested();
        using var custom = ShellComObject.Create(DestinationClass, DestinationInterface);
        Marshal.ThrowExceptionForHR(custom.Method<SetAppId>(4)(custom.Handle, appId));
        using var automatic = ShellComObject.Create(AutomaticClass, AutomaticInterface);
        Marshal.ThrowExceptionForHR(automatic.Method<InitializeAutomatic>(3)(automatic.Handle, appId, 0, 0));
        var groups = new List<JumpListGroup>();
        AddAutomatic(groups, automatic, 0, UiText.Get("PinnedDestinations"), cancellation);
        var categoryResult = custom.Method<GetCount>(6)(custom.Handle, out var count);
        if (MissingList(categoryResult)) count = 0;
        else Marshal.ThrowExceptionForHR(categoryResult);
        if (count > 32) throw new InvalidOperationException("Jump List category limit exceeded.");
        var hasKnownCategory = false;
        for (uint index = 0; index < count && _commands.Count < 100; index++)
        {
            cancellation.ThrowIfCancellationRequested();
            Marshal.ThrowExceptionForHR(custom.Method<GetCategory>(7)(custom.Handle, index, 1, out var category));
            try
            {
                if (category.Type == 1)
                {
                    var kind = category.Kind;
                    if (kind is 1 or 2)
                    {
                        hasKnownCategory = true;
                        AddAutomatic(groups, automatic, 3 - kind, kind == 1 ? UiText.Get("Frequent") : UiText.Get("Recent"), cancellation);
                    }
                }
                else if (category.Type is 0 or 2)
                {
                    var iid = ObjectArrayInterface;
                    Marshal.ThrowExceptionForHR(custom.Method<EnumerateCategory>(9)(custom.Handle, index, ref iid, out var pointer));
                    using var array = new ShellComObject(pointer);
                    var title = category.Type == 2 ? UiText.Get("Tasks") : ResolveText(Marshal.PtrToStringUni(category.NameOrKind));
                    AddGroup(groups, title, array, cancellation);
                }
            }
            finally
            {
                if (category.Type == 0 && category.NameOrKind != 0) Marshal.FreeCoTaskMem(category.NameOrKind);
            }
        }
        if (!hasKnownCategory) AddAutomatic(groups, automatic, 1, UiText.Get("Recent"), cancellation);
        return new(sessionId, groups.Count > 0 ? JumpListStatus.Ready : JumpListStatus.Empty, groups);
    }

    private static string? ResolveIdentity(nint window, string? shortcut)
    {
        if (window != 0)
        {
            var iid = PropertyStoreInterface;
            Marshal.ThrowExceptionForHR(SHGetPropertyStoreForWindow(window, ref iid, out var pointer));
            using var store = new ShellComObject(pointer);
            var explicitId = ReadString(store, AppIdKey);
            if (!string.IsNullOrWhiteSpace(explicitId)) return explicitId;
        }
        using var resolver = ShellComObject.Create(ResolverClass, ResolverInterface);
        nint appId;
        if (window != 0)
        {
            Marshal.ThrowExceptionForHR(resolver.Method<ResolveWindow>(5)(resolver.Handle, window, out appId, 0, 0, 0));
        }
        else if (!string.IsNullOrWhiteSpace(shortcut))
        {
            var iid = ShellItemInterface;
            Marshal.ThrowExceptionForHR(SHCreateItemFromParsingName(shortcut, 0, ref iid, out var pointer));
            using var item = new ShellComObject(pointer);
            Marshal.ThrowExceptionForHR(resolver.Method<ResolveShortcut>(3)(resolver.Handle, item.Handle, out appId));
        }
        else return null;
        try { return Marshal.PtrToStringUni(appId); }
        finally { if (appId != 0) Marshal.FreeCoTaskMem(appId); }
    }

    // Explicit window identity without throwing, for dock icons. ApplicationFrameHost frames publish the hosted app's ID here.
    internal static string? ReadWindowAppId(nint window)
    {
        var iid = PropertyStoreInterface;
        if (window == 0 || SHGetPropertyStoreForWindow(window, ref iid, out var pointer) < 0 || pointer == 0) return null;
        using var store = new ShellComObject(pointer);
        var key = AppIdKey;
        if (store.Method<GetValue>(5)(store.Handle, ref key, out var value) < 0) return null;
        try { return value.Type is 31 or 8 && Marshal.PtrToStringUni(value.Pointer) is { } id && !string.IsNullOrWhiteSpace(id) ? id : null; }
        finally { PropVariantClear(ref value); }
    }

    private void AddAutomatic(List<JumpListGroup> groups, ShellComObject automatic, int kind, string title, CancellationToken cancellation)
    {
        var iid = ObjectArrayInterface;
        var result = automatic.Method<GetAutomaticList>(5)(automatic.Handle, kind, 20, 1, ref iid, out var pointer);
        if (MissingList(result)) return;
        Marshal.ThrowExceptionForHR(result);
        using var array = new ShellComObject(pointer);
        AddGroup(groups, title, array, cancellation);
    }

    private void AddGroup(List<JumpListGroup> groups, string? title, ShellComObject array, CancellationToken cancellation)
    {
        if (string.IsNullOrWhiteSpace(title)) return;
        Marshal.ThrowExceptionForHR(array.Method<GetCount>(3)(array.Handle, out var count));
        var entries = new List<JumpListEntry>();
        for (uint index = 0; index < Math.Min(count, 40u) && _commands.Count < 100; index++)
        {
            cancellation.ThrowIfCancellationRequested();
            var iid = new Guid("00000000-0000-0000-C000-000000000046");
            Marshal.ThrowExceptionForHR(array.Method<GetAt>(4)(array.Handle, index, ref iid, out var pointer));
            var item = new ShellComObject(pointer);
            var retained = false;
            try
            {
                using var store = item.Query(PropertyStoreInterface);
                if (store is not null && ReadSeparator(store))
                {
                    if (entries.Count > 0 && !entries[^1].IsSeparator) entries.Add(new(0, "", null, true));
                    continue;
                }
                var label = store is null ? null : ResolveText(ReadString(store, TitleKey));
                using var shellItem = item.Query(ShellItemInterface);
                if (string.IsNullOrWhiteSpace(label) && shellItem is not null)
                {
                    Marshal.ThrowExceptionForHR(shellItem.Method<GetDisplayName>(5)(shellItem.Handle, 0, out var name));
                    try { label = Marshal.PtrToStringUni(name); }
                    finally { Marshal.FreeCoTaskMem(name); }
                }
                if (string.IsNullOrWhiteSpace(label)) continue;
                var id = ++_nextCommand;
                entries.Add(new(id, label, ReadIcon(item)));
                _commands.Add(id, item);
                retained = true;
            }
            finally { if (!retained) item.Dispose(); }
        }
        if (entries.Count > 0 && entries[^1].IsSeparator) entries.RemoveAt(entries.Count - 1);
        if (entries.Count > 0) groups.Add(new(title, entries));
    }

    private static string? ReadString(ShellComObject store, PropertyKey key)
    {
        Marshal.ThrowExceptionForHR(store.Method<GetValue>(5)(store.Handle, ref key, out var value));
        try { return value.Type is 31 or 8 ? Marshal.PtrToStringUni(value.Pointer) : null; }
        finally { PropVariantClear(ref value); }
    }

    private static bool ReadSeparator(ShellComObject store)
    {
        var key = SeparatorKey;
        Marshal.ThrowExceptionForHR(store.Method<GetValue>(5)(store.Handle, ref key, out var value));
        try { return value.Type == 11 && value.Bool != 0; }
        finally { PropVariantClear(ref value); }
    }

    private static string? ResolveText(string? text)
    {
        if (string.IsNullOrEmpty(text) || !text.StartsWith('@')) return text;
        var resolved = new StringBuilder(1024);
        Marshal.ThrowExceptionForHR(SHLoadIndirectString(text, resolved, (uint)resolved.Capacity, 0));
        return resolved.ToString();
    }

    private static BitmapSource? ReadIcon(ShellComObject item)
    {
        using var factory = item.Query(ImageFactoryInterface);
        if (factory is not null)
        {
            var result = factory.Method<GetImage>(3)(factory.Handle, new NativeSize(24, 24), 4, out var bitmap);
            if (result >= 0 && bitmap != 0)
            {
                try
                {
                    var source = Imaging.CreateBitmapSourceFromHBitmap(bitmap, 0, Int32Rect.Empty, BitmapSizeOptions.FromEmptyOptions());
                    source.Freeze();
                    return source;
                }
                finally { DeleteObject(bitmap); }
            }
        }
        // Tasks are often IShellLink objects with an explicit resource, rather than Shell images.
        using var link = item.Query(ShellLinkInterface);
        if (link is null) return null;
        var path = new StringBuilder(2048);
        var iconResult = link.Method<GetIconLocation>(16)(link.Handle, path, path.Capacity, out var iconIndex);
        if (iconResult < 0) return null;
        if (path.Length == 0)
        {
            var pathResult = link.Method<GetLinkPath>(3)(link.Handle, path, path.Capacity, 0, 4);
            if (pathResult < 0 || path.Length == 0) return null;
            iconIndex = 0;
        }
        return ApplicationIconProvider.GetForResource(path.ToString(), iconIndex);
    }

    private static bool MissingList(int result) => result is unchecked((int)0x80030002) or unchecked((int)0x80070002);

    public void Invoke(int commandId, nint owner)
    {
        if (!_commands.TryGetValue(commandId, out var item)) throw new InvalidOperationException("Jump List session expired.");
        using var store = item.Query(PropertyStoreInterface);
        var appId = store is null ? null : ReadString(store, AppIdKey);
        // Packaged task links activate their AppsFolder target with the published activation context.
        if (appId?.Contains('!') == true)
        {
            var folder = new Guid("1E87508D-89C2-42F0-8A7E-645A0F50CA58");
            var iid = ShellItemInterface;
            Marshal.ThrowExceptionForHR(SHCreateItemInKnownFolder(ref folder, 0, appId, ref iid, out var pointer));
            using var target = new ShellComObject(pointer);
            using var properties = Bind(target, new("0384E1A4-1523-439C-A4C8-AB911052F586"), PropertyStoreInterface);
            var key = HostEnvironmentKey;
            Marshal.ThrowExceptionForHR(properties.Method<GetValue>(5)(properties.Handle, ref key, out var host));
            try
            {
                if (host.Type == 19 && host.UInt32 != 0)
                {
                    using var targetMenu = Bind(target, new("3981E225-F559-11D3-8E3A-00C04F6837D5"), ContextMenuInterface);
                    InvokeDefault(targetMenu, owner, store is null ? null : ReadString(store, ActivationContextKey));
                    return;
                }
            }
            finally { PropVariantClear(ref host); }
        }
        // Unpackaged tasks (Firefox, LibreWolf, ...) carry their target and arguments in a shell link. Invoking
        // the link through IContextMenu with OUR hwnd makes ShellExecute derive the activation context from
        // OpenTaskbar, so the running app's single-instance IPC drops task flags such as -private-window and
        // opens a normal tab/window. Invoke through IContextMenu with the TARGET app's hwnd instead, which is
        // the same activation context Explorer's taskbar supplies. Fall back to launching the link target
        // directly only when no shell menu is available (rare; e.g. synthetic links without a handler).
        nint contextOwner = _targetWindow != 0 ? _targetWindow : owner;
        using (var direct = item.Query(ContextMenuInterface))
        {
            if (direct is not null)
            {
                InvokeDefault(direct, contextOwner, null);
                return;
            }
        }
        using (var shellItem = item.Query(ShellItemInterface))
        {
            if (shellItem is not null)
            {
                using var bound = Bind(shellItem, new("3981E225-F559-11D3-8E3A-00C04F6837D5"), ContextMenuInterface);
                InvokeDefault(bound, contextOwner, null);
                return;
            }
        }
        if (TryLaunchShellLink(item)) return;
        throw new COMException("No Shell invocation interface.");
    }

    /// <summary>
    /// Reads an IShellLink task and starts its target with the arguments the application published. Returns false
    /// when the item is not a shell link, so the caller can fall back to shell invocation.
    /// </summary>
    internal static bool TryLaunchShellLink(ShellComObject item)
    {
        using var link = item.Query(ShellLinkInterface);
        if (link is null) return false;

        var path = new StringBuilder(260);
        if (link.Method<GetLinkPath>(3)(link.Handle, path, path.Capacity, 0, SLgpRawPath) < 0 || path.Length == 0)
            return false;

        var arguments = new StringBuilder(2048);
        var args = link.Method<GetLinkArguments>(9)(link.Handle, arguments, arguments.Capacity) >= 0
            ? arguments.ToString() : string.Empty;
        if (string.IsNullOrWhiteSpace(args))
        {
            using var store = item.Query(PropertyStoreInterface);
            args = store is null ? string.Empty : ReadString(store, LinkArgumentsKey) ?? string.Empty;
        }
        var directory = new StringBuilder(260);
        var workingDirectory = link.Method<GetLinkWorkingDirectory>(11)(link.Handle, directory, directory.Capacity) >= 0
            ? directory.ToString() : string.Empty;

        Process.Start(BuildLaunchInfo(path.ToString(), args, workingDirectory));
        AppLog.Info($"Jump List task launched directly: {path} {args}".TrimEnd());
        return true;
    }

    internal static ProcessStartInfo BuildLaunchInfo(string path, string arguments, string workingDirectory)
    {
        var info = new ProcessStartInfo(path) { Arguments = arguments, UseShellExecute = true };
        if (!string.IsNullOrWhiteSpace(workingDirectory)) info.WorkingDirectory = workingDirectory;
        return info;
    }

    private static ShellComObject Bind(ShellComObject item, Guid handler, Guid iid)
    {
        Marshal.ThrowExceptionForHR(item.Method<BindHandler>(3)(item.Handle, 0, ref handler, ref iid, out var pointer));
        return new ShellComObject(pointer);
    }

    private static void InvokeDefault(ShellComObject menu, nint owner, string? activationContext)
    {
        var nativeMenu = CreatePopupMenu();
        if (nativeMenu == 0) throw new System.ComponentModel.Win32Exception(Marshal.GetLastWin32Error());
        try
        {
            Marshal.ThrowExceptionForHR(menu.Method<QueryMenu>(3)(menu.Handle, nativeMenu, 0, 1, 0x7fff, 1));
            var defaultId = GetMenuDefaultItem(nativeMenu, false, 0);
            if (defaultId == uint.MaxValue || defaultId == 0) throw new InvalidOperationException("Shell default command unavailable.");
            var command = new InvokeInfo
            {
                Size = Marshal.SizeOf<InvokeInfo>(), Mask = 0x4000, Window = owner,
                Verb = (nint)(defaultId - 1), VerbW = (nint)(defaultId - 1), Show = 1,
                Parameters = activationContext, ParametersW = activationContext
            };
            Marshal.ThrowExceptionForHR(menu.Method<InvokeMenu>(4)(menu.Handle, ref command));
        }
        finally { DestroyMenu(nativeMenu); }
    }

    public void Dispose()
    {
        foreach (var item in _commands.Values) item.Dispose();
        _commands.Clear();
    }

    [StructLayout(LayoutKind.Sequential)] private struct PropertyKey(Guid format, uint id) { public Guid Format = format; public uint Id = id; }
    [StructLayout(LayoutKind.Explicit, Size = 24)] private struct Variant { [FieldOffset(0)] public ushort Type; [FieldOffset(8)] public nint Pointer; [FieldOffset(8)] public short Bool; [FieldOffset(8)] public uint UInt32; }
    [StructLayout(LayoutKind.Explicit, Size = 64)] private struct Category { [FieldOffset(0)] public int Type; [FieldOffset(8)] public nint NameOrKind; [FieldOffset(8)] public int Kind; }
    [StructLayout(LayoutKind.Sequential)] private readonly struct NativeSize(int width, int height) { public readonly int Width = width; public readonly int Height = height; }
    [StructLayout(LayoutKind.Sequential)] private struct InvokeInfo
    {
        public int Size; public uint Mask; public nint Window; public nint Verb;
        [MarshalAs(UnmanagedType.LPStr)] public string? Parameters;
        public nint Directory; public int Show; public uint HotKey; public nint Icon; public nint Title; public nint VerbW;
        [MarshalAs(UnmanagedType.LPWStr)] public string? ParametersW;
        public nint DirectoryW; public nint TitleW; public int PointX; public int PointY;
    }

    [UnmanagedFunctionPointer(CallingConvention.StdCall)] private delegate int SetAppId(nint self, [MarshalAs(UnmanagedType.LPWStr)] string id);
    [UnmanagedFunctionPointer(CallingConvention.StdCall)] private delegate int GetCount(nint self, out uint count);
    [UnmanagedFunctionPointer(CallingConvention.StdCall)] private delegate int GetCategory(nint self, uint index, int flags, out Category category);
    [UnmanagedFunctionPointer(CallingConvention.StdCall)] private delegate int EnumerateCategory(nint self, uint index, ref Guid iid, out nint result);
    [UnmanagedFunctionPointer(CallingConvention.StdCall)] private delegate int InitializeAutomatic(nint self, [MarshalAs(UnmanagedType.LPWStr)] string id, nint link, nint extra);
    [UnmanagedFunctionPointer(CallingConvention.StdCall)] private delegate int GetAutomaticList(nint self, int kind, uint max, uint flags, ref Guid iid, out nint result);
    [UnmanagedFunctionPointer(CallingConvention.StdCall)] private delegate int ResolveWindow(nint self, nint window, out nint id, nint prevented, nint explicitId, nint embedded);
    [UnmanagedFunctionPointer(CallingConvention.StdCall)] private delegate int ResolveShortcut(nint self, nint item, out nint id);
    [UnmanagedFunctionPointer(CallingConvention.StdCall)] private delegate int GetValue(nint self, ref PropertyKey key, out Variant value);
    [UnmanagedFunctionPointer(CallingConvention.StdCall)] private delegate int GetAt(nint self, uint index, ref Guid iid, out nint item);
    [UnmanagedFunctionPointer(CallingConvention.StdCall)] private delegate int GetDisplayName(nint self, uint kind, out nint name);
    [UnmanagedFunctionPointer(CallingConvention.StdCall)] private delegate int GetImage(nint self, NativeSize size, uint flags, out nint bitmap);
    [UnmanagedFunctionPointer(CallingConvention.StdCall, CharSet = CharSet.Unicode)] private delegate int GetIconLocation(nint self, StringBuilder path, int length, out int index);
    [UnmanagedFunctionPointer(CallingConvention.StdCall, CharSet = CharSet.Unicode)] private delegate int GetLinkPath(nint self, StringBuilder path, int length, nint findData, uint flags);
    [UnmanagedFunctionPointer(CallingConvention.StdCall, CharSet = CharSet.Unicode)] private delegate int GetLinkArguments(nint self, StringBuilder arguments, int length);
    [UnmanagedFunctionPointer(CallingConvention.StdCall, CharSet = CharSet.Unicode)] private delegate int GetLinkWorkingDirectory(nint self, StringBuilder directory, int length);
    [UnmanagedFunctionPointer(CallingConvention.StdCall)] private delegate int BindHandler(nint self, nint context, ref Guid handler, ref Guid iid, out nint result);
    [UnmanagedFunctionPointer(CallingConvention.StdCall)] private delegate int QueryMenu(nint self, nint menu, uint index, uint first, uint last, uint flags);
    [UnmanagedFunctionPointer(CallingConvention.StdCall)] private delegate int InvokeMenu(nint self, ref InvokeInfo info);
    [DllImport("shell32.dll")] private static extern int SHGetPropertyStoreForWindow(nint window, ref Guid iid, out nint store);
    [DllImport("shell32.dll", CharSet = CharSet.Unicode)] private static extern int SHCreateItemFromParsingName(string path, nint context, ref Guid iid, out nint item);
    [DllImport("shell32.dll", CharSet = CharSet.Unicode)] private static extern int SHCreateItemInKnownFolder(ref Guid folder, uint flags, string path, ref Guid iid, out nint item);
    [DllImport("shlwapi.dll", CharSet = CharSet.Unicode)] private static extern int SHLoadIndirectString(string source, StringBuilder output, uint count, nint reserved);
    [DllImport("ole32.dll")] private static extern int PropVariantClear(ref Variant value);
    [DllImport("gdi32.dll")] [return: MarshalAs(UnmanagedType.Bool)] private static extern bool DeleteObject(nint bitmap);
    [DllImport("user32.dll", SetLastError = true)] private static extern nint CreatePopupMenu();
    [DllImport("user32.dll")] private static extern uint GetMenuDefaultItem(nint menu, [MarshalAs(UnmanagedType.Bool)] bool byPosition, uint flags);
    [DllImport("user32.dll")] [return: MarshalAs(UnmanagedType.Bool)] private static extern bool DestroyMenu(nint menu);
}
