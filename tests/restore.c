/* Wine test helper (never shipped): restores the minimised Tachyon window ("min" argument: minimises it). */
#include <windows.h>
#include <string.h>
int main(int argc, char** argv) {
    HWND h = FindWindowW(L"TachyonV4", NULL); if (!h) return 1;
    ShowWindow(h, argc > 1 && !strcmp(argv[1], "min") ? SW_MINIMIZE : SW_RESTORE);
    return 0;
}
