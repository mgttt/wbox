#ifndef BLINK_BROKERFS_H_
#define BLINK_BROKERFS_H_

#include "blink/vfs.h"

extern struct VfsSystem g_brokerfs;
int BrokerfsMountConfigured(void);

#endif /* BLINK_BROKERFS_H_ */
