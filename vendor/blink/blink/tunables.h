#ifndef BLINK_TUNABLES_H_
#define BLINK_TUNABLES_H_
#include <stdint.h>

#include "blink/builtin.h"

#define BLINK_MAJOR 1
#define BLINK_MINOR 1
#define BLINK_PATCH 0

#define LINUX_MAJOR 4
#define LINUX_MINOR 5
#define LINUX_PATCH 0

#define MKVERSION_(x, y, z) #x "." #y "." #z
#define MKVERSION(x, y, z)  MKVERSION_(x, y, z)
#define LINUX_VERSION       MKVERSION(LINUX_MAJOR, LINUX_MINOR, LINUX_PATCH)
#define BLINK_VERSION       MKVERSION(BLINK_MAJOR, BLINK_MINOR, BLINK_PATCH)

#if CAN_64BIT && defined(__APPLE__)
#define kSkew 0x088800000000
#elif CAN_64BIT && defined(_WIN32) && !defined(__CYGWIN__)
// wbox win32: guest VA window is reserved at [kSkew, kSkew+vaspace).
// kSkew is a RUNTIME value on win32: WboxMemInit() reserves the window
// with a system-chosen base (VirtualAlloc NULL + ZeroBits semantics) and
// stores the base here, because probing TB-sized fixed-address reserves
// is unreliable under Wine (and conflicts with host mappings).
// Defined in win32/w32mem.c; initialized before any ToHost() use.
extern uint64_t kSkew;
#else
#define kSkew 0x000000000000
#endif

#define kAslrMask      0x03ffff000000  // 16mb before pointers get nasty
#define kImageStart    0x110001000000
#define kAutomapStart  0x200000000000
#define kAutomapEnd    0x400000000000
#define kDynInterpAddr 0x454000000000
#define kStackTop      0x500000000000

#define kRealSize  (16 * 1024 * 1024)  // size of ram for real mode
#define kStackSize (8 * 1024 * 1024)   // size of stack for user mode
#define kNullSize  (2 * 1024 * 1024)   // minimum user mode image address

#define kMinBlinkFd   123  // fds owned by the vm start here
#define kPollingMs    50   // busy loop for futex(), poll(), etc.
#define kBusCount     256  // # load balanced semaphores in virtual bus
#define kBusRegion    128  // 16 is sufficient for 8-byte loads/stores
#define kFutexMax     100
#define kRedzoneSize  128
#define kSmcQueueSize 32
#define kMaxMapSize   (UINT64_C(8) * 1024 * 1024 * 1024)
#define kMaxResident  (UINT64_C(8) * 1024 * 1024 * 1024)
#define kMaxVirtual   (kMaxResident * 8)
#define kMaxAncillary 1000
#define kMaxShebang   512
#define kMaxSigDepth  8

#define kStraceArgMax 256
#define kStraceBufMax 32

#endif /* BLINK_TUNABLES_H_ */
