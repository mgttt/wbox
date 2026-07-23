#ifndef WBOX_COMPAT_SYS_PRCTL_H
#define WBOX_COMPAT_SYS_PRCTL_H
int prctl(int, ...);
#define PR_SET_NAME 15
#define PR_GET_NAME 16
#endif
