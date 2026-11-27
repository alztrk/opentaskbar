#define UNICODE
#define _UNICODE
#include <windows.h>
#include <shellapi.h>
#include <stdio.h>
#include <stdlib.h>

#define ABS_AUTOHIDE    0x0000001
#define ABS_ALWAYSONTOP 0x0000002

#define WATCHDOG_PATH_CAPACITY 1024
#define HEARTBEAT_TIMEOUT_MS 4000
#define HEARTBEAT_MISSES_REQUIRED 3

static void AttachToDefaultDesktop(void) {
    HDESK hDesk = OpenDesktopW(L"Default", 0, FALSE, GENERIC_ALL);
    if (hDesk) {
        SetThreadDesktop(hDesk);
        CloseDesktop(hDesk);
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
// atomic (written via flyout-positions.txt.tmp by BOTH the reference writer and Rust flyout_writer) and PID-checked
// so a recycled HWND can never be moved by a stale entry. The four scanned fields are the shared wire format
// "<hwnd> <pid> <left> <top>"; Rust may append generation/seq tokens, which fwscanf intentionally ignores.
// Returns FALSE when an entry belongs to another process now, so the journal is kept for a later recovery pass.
static BOOL RestoreFlyoutPositions(void) {
    WCHAR localAppData[WATCHDOG_PATH_CAPACITY];
    DWORD envLen = GetEnvironmentVariableW(L"LOCALAPPDATA", localAppData, WATCHDOG_PATH_CAPACITY);
    if (envLen == 0 || envLen >= WATCHDOG_PATH_CAPACITY) return TRUE;

    WCHAR path[WATCHDOG_PATH_CAPACITY];
    if (_snwprintf_s(path, WATCHDOG_PATH_CAPACITY, _TRUNCATE, L"%s\\OpenTaskbar\\flyout-positions.txt", localAppData) < 0) {
        return TRUE;
    }
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
    WCHAR journalPath[WATCHDOG_PATH_CAPACITY] = {0};
    BOOL hasJournal = FALSE;
    WCHAR localAppData[WATCHDOG_PATH_CAPACITY];
    DWORD state = 0;
    DWORD envLen = GetEnvironmentVariableW(L"LOCALAPPDATA", localAppData, WATCHDOG_PATH_CAPACITY);
    if (envLen > 0 && envLen < WATCHDOG_PATH_CAPACITY
        && _snwprintf_s(journalPath, WATCHDOG_PATH_CAPACITY, _TRUNCATE, L"%s\\OpenTaskbar\\taskbar-state.txt", localAppData) >= 0) {
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
    if (_snwprintf_s(eventName, 256, _TRUNCATE, L"Local\\OpenTaskbar.CleanExit.%d.%s", parentPid, token) < 0) {
        CloseHandle(hProcess);
        return 1;
    }
    HANDLE hCleanExit = OpenEventW(SYNCHRONIZE, FALSE, eventName);

    WCHAR heartbeatName[256];
    HANDLE hHeartbeat = NULL;
    if (_snwprintf_s(heartbeatName, 256, _TRUNCATE, L"Local\\OpenTaskbar.Heartbeat.%d.%s", parentPid, token) >= 0) {
        hHeartbeat = OpenEventW(SYNCHRONIZE, FALSE, heartbeatName);
    }

    HANDLE waitHandles[3];
    DWORD waitCount = 0;
    DWORD cleanExitIndex = (DWORD)-1;
    DWORD processIndex = (DWORD)-1;
    DWORD heartbeatIndex = (DWORD)-1;

    if (hCleanExit) {
        cleanExitIndex = waitCount;
        waitHandles[waitCount++] = hCleanExit;
    }
    processIndex = waitCount;
    waitHandles[waitCount++] = hProcess;
    if (hHeartbeat) {
        heartbeatIndex = waitCount;
        waitHandles[waitCount++] = hHeartbeat;
    }

    BOOL cleanExit = FALSE;
    if (hHeartbeat) {
        int heartbeatMisses = 0;
        while (TRUE) {
            DWORD waitResult = WaitForMultipleObjects(waitCount, waitHandles, FALSE, HEARTBEAT_TIMEOUT_MS);
            if (waitResult == WAIT_OBJECT_0 + cleanExitIndex) {
                cleanExit = TRUE;
                break;
            }
            if (waitResult == WAIT_OBJECT_0 + processIndex) {
                if (hCleanExit && WaitForSingleObject(hCleanExit, 0) == WAIT_OBJECT_0) {
                    cleanExit = TRUE;
                }
                break;
            }
            if (waitResult == WAIT_OBJECT_0 + heartbeatIndex) {
                heartbeatMisses = 0;
                continue;
            }
            if (waitResult == WAIT_TIMEOUT) {
                if (++heartbeatMisses >= HEARTBEAT_MISSES_REQUIRED) {
                    break;
                }
                continue;
            }
            break;
        }
    } else {
        DWORD waitResult = WaitForMultipleObjects(waitCount, waitHandles, FALSE, INFINITE);
        if (hCleanExit) {
            if (waitResult == WAIT_OBJECT_0 + cleanExitIndex) {
                cleanExit = TRUE;
            } else if (waitResult == WAIT_OBJECT_0 + processIndex) {
                if (WaitForSingleObject(hCleanExit, 0) == WAIT_OBJECT_0) {
                    cleanExit = TRUE;
                }
            }
        }
    }

    if (hHeartbeat) CloseHandle(hHeartbeat);
    if (hCleanExit) CloseHandle(hCleanExit);
    CloseHandle(hProcess);

    if (!cleanExit) {
        RestoreNativeTaskbars();
    }

    return 0;
}
