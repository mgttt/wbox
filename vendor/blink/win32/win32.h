#ifndef WBOX_WIN32_H_
#define WBOX_WIN32_H_
// internal interface of the wbox win32 port layer
#include <stdint.h>

int WboxMemInit(void);
uintptr_t WboxMemLimit(void);
int WboxMemVabits(void);

void WboxSigInit(void);

#endif
