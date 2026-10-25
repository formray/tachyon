/* Wine test helper (never shipped): writes its PID to argv[1], optionally starts argv[2..] as a child, then sleeps. */
#include <windows.h>
#include <stdio.h>
int main(int argc, char** argv) {
    FILE* f = fopen(argv[1], "w"); if (f) { fprintf(f, "%lu\n", GetCurrentProcessId()); fclose(f); }
    if (argc > 2) {
        char cmd[2048] = ""; for (int i = 2; i < argc; i++) { strcat(cmd, "\""); strcat(cmd, argv[i]); strcat(cmd, "\" "); }
        STARTUPINFOA si = { sizeof si }; PROCESS_INFORMATION pi;
        CreateProcessA(NULL, cmd, NULL, NULL, FALSE, 0, NULL, NULL, &si, &pi);
    }
    Sleep(INFINITE);
    return 0;
}
