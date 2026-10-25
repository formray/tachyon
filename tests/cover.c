/* Wine test helper (never shipped): an opaque 900x900 window at 0,0 for 20 s, to test the occlusion skip. */
#include <windows.h>
int main(void) {
    HWND h = CreateWindowExW(0, L"STATIC", L"cover", WS_POPUP | WS_VISIBLE, 0, 0, 900, 900, NULL, NULL, NULL, NULL);
    SetWindowPos(h, HWND_TOP, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE);
    DWORD t = GetTickCount(); MSG m;
    while (GetTickCount() - t < 20000) { while (PeekMessageW(&m, NULL, 0, 0, PM_REMOVE)) DispatchMessageW(&m); Sleep(50); }
    return 0;
}
