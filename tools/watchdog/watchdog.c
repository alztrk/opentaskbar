#define UNICODE
#define _UNICODE
#include <windows.h>
#include <shellapi.h>
#include <stdio.h>
#include <stdlib.h>

#define ABS_AUTOHIDE    0x0000001
#define ABS_ALWAYSONTOP 0x0000002

static void AttachToDefaultDesktop(void) {
    HDESK hDesk = OpenDesktopW(L"Default", 0, FALSE, GENERIC_ALL);
    if (hDesk) {
        SetThreadDesktop(hDesk);
    }
}

static BOOL CALLBACK EnumSecondaryTrays(HWND hwnd, LPARAM lParam) {
    (void)lParam;
    WCHAR className[64];
    if (GetClassNameW(hwnd, className, 64) > 0) {
        if (wcscmp(className, L"Shell_SecondaryTrayWnd") == 0) {
            ShowWindow(hwnd, SW_SHOW);
            SetWindowPos(hwnd, NULL, 0, 0, 0, 0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_SHOWWINDOW | SWP_FRAMECHANGED);
        }
    }
    return TRUE;
}

// Returns Start/Search/Control Center to the positions recorded before the dock anchored them. The journal is
// atomic (written via a temp file) and PID-checked so a recycled HWND can never be moved by a stale entry.
// Returns FALSE when an entry belongs to another process now, so the journal is kept for a later recovery pass.
static BOOL RestoreFlyoutPositions(void) {
    WCHAR localAppData[MAX_PATH];
    if (GetEnvironmentVariableW(L"LOCALAPPDATA", localAppData, MAX_PATH) == 0) return TRUE;

    WCHAR path[MAX_PATH];
    wsprintfW(path, L"%s\\OpenTaskbar\\flyout-positions.txt", localAppData);
    FILE *file = _wfopen(path, L"r");
    if (!file) return TRUE;

    BOOL ownedBySomeoneElse = FALSE;
    unsigned long long handleValue = 0;
    unsigned int processId = 0;
    int left = 0;
    int top = 0;
    while (fwscanf(file, L"%llu %u %d %d", &handleValue, &processId, &left, &top) == 4) {
        HWND handle = (HWND)(UINT_PTR)handleValue;
        if (!IsWindow(handle)) continue;

        DWORD actualProcessId = 0;
        GetWindowThreadProcessId(handle, &actualProcessId);
        if (actualProcessId != (DWORD)processId) {
            ownedBySomeoneElse = TRUE;
            continue;
        }

        SetWindowPos(handle, NULL, left, top, 0, 0,
            SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
    }
    fclose(file);
    if (!ownedBySomeoneElse) DeleteFileW(path);
    return !ownedBySomeoneElse;
}

// A killed tray host leaves app icons registered with its destroyed window; ask apps to re-register them.
static void BroadcastTaskbarCreated(void) {
    UINT message = RegisterWindowMessageW(L"TaskbarCreated");
    if (message) SendNotifyMessageW((HWND)0xFFFF, message, 0, 0);
}

static void RestoreNativeTaskbars(void) {
    AttachToDefaultDesktop();

    // 0. The dock may be gone, but its flyouts are hosted by long-lived shell processes. Put them back first.
    RestoreFlyoutPositions();

    // 1. Read journal if available to recover original auto-hide/always-on-top state. Keep it until the
    //    taskbar is actually visible; deleting it on an Explorer restart would destroy the only recovery clue.
    WCHAR journalPath[MAX_PATH] = {0};
    BOOL hasJournal = FALSE;
    WCHAR localAppData[MAX_PATH];
    DWORD state = 0;
    if (GetEnvironmentVariableW(L"LOCALAPPDATA", localAppData, MAX_PATH) > 0) {
        wsprintfW(journalPath, L"%s\\OpenTaskbar\\taskbar-state.txt", localAppData);
        FILE *jf = _wfopen(journalPath, L"r");
        if (jf) {
            unsigned int readVal = 0;
            if (fwscanf(jf, L"%u", &readVal) == 1) {
                state = (DWORD)readVal & (ABS_AUTOHIDE | ABS_ALWAYSONTOP);
                hasJournal = TRUE;
            }
            fclose(jf);
        }
    }

    // 2. Locate primary taskbar
    HWND hPrimary = FindWindowW(L"Shell_TrayWnd", NULL);
    if (!hPrimary) return;

    // 3. Re-assert state and visibility with retries (defeats transient Explorer re-hides)
    for (int attempt = 0; attempt < 3; attempt++) {
        APPBARDATA abd;
        ZeroMemory(&abd, sizeof(abd));
        abd.cbSize = sizeof(APPBARDATA);
        abd.hWnd = hPrimary;
        abd.lParam = (LPARAM)state;
        SHAppBarMessage(ABM_SETSTATE, &abd);

        ShowWindow(hPrimary, SW_SHOW);
        SetWindowPos(hPrimary, NULL, 0, 0, 0, 0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_SHOWWINDOW | SWP_FRAMECHANGED);

        EnumWindows(EnumSecondaryTrays, 0);

        if (attempt < 2) {
            Sleep(60);
        }
    }

    // Explorer can re-hide the tray right after we show it; do not drop the journal unless it stayed visible.
    for (int attempt = 0; attempt < 3 && !IsWindowVisible(hPrimary); attempt++) {
        ShowWindow(hPrimary, SW_SHOW);
        EnumWindows(EnumSecondaryTrays, 0);
        Sleep(60);
    }
    if (!IsWindowVisible(hPrimary)) return;

    if (hasJournal) DeleteFileW(journalPath);
    BroadcastTaskbarCreated();
}

int WINAPI WinMain(HINSTANCE hInstance, HINSTANCE hPrevInstance, LPSTR lpCmdLine, int nCmdShow) {
    (void)hInstance;
    (void)hPrevInstance;
    (void)lpCmdLine;
    (void)nCmdShow;

    AttachToDefaultDesktop();

    int argc = 0;
    LPWSTR *argv = CommandLineToArgvW(GetCommandLineW(), &argc);
    if (!argv || argc < 2) return 1;

    int parentPid = 0;
    WCHAR token[128] = {0};

    for (int i = 1; i < argc; i++) {
        if (wcsncmp(argv[i], L"--watchdog=", 11) == 0) {
            LPCWSTR data = argv[i] + 11;
            LPCWSTR colon = wcschr(data, L':');
            if (colon && colon > data) {
                parentPid = _wtoi(data);
                wcsncpy_s(token, 128, colon + 1, _TRUNCATE);
                break;
            }
        }
    }
    LocalFree(argv);

    if (parentPid <= 0 || token[0] == L'\0') {
        return 1;
    }

    HANDLE hProcess = OpenProcess(SYNCHRONIZE, FALSE, (DWORD)parentPid);
    if (!hProcess) {
        RestoreNativeTaskbars();
        return 0;
    }

    WCHAR eventName[256];
    wsprintfW(eventName, L"Local\\OpenTaskbar.CleanExit.%d.%s", parentPid, token);
    HANDLE hCleanExit = OpenEventW(SYNCHRONIZE, FALSE, eventName);

    HANDLE waitHandles[2];
    DWORD waitCount = 0;

    if (hCleanExit) {
        waitHandles[0] = hCleanExit;
        waitHandles[1] = hProcess;
        waitCount = 2;
    } else {
        waitHandles[0] = hProcess;
        waitCount = 1;
    }

    DWORD waitResult = WaitForMultipleObjects(waitCount, waitHandles, FALSE, INFINITE);

    BOOL cleanExit = FALSE;
    if (waitCount == 2) {
        if (waitResult == WAIT_OBJECT_0) {
            cleanExit = TRUE;
        } else if (waitResult == WAIT_OBJECT_0 + 1) {
            if (WaitForSingleObject(hCleanExit, 0) == WAIT_OBJECT_0) {
                cleanExit = TRUE;
            }
        }
    }

    if (hCleanExit) CloseHandle(hCleanExit);
    CloseHandle(hProcess);

    if (!cleanExit) {
        RestoreNativeTaskbars();
    }

    return 0;
}
