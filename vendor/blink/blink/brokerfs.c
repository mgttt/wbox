#include "blink/brokerfs.h"

#if defined(_WIN32) && !defined(__CYGWIN__) && !defined(DISABLE_VFS)

#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "blink/assert.h"
#include "blink/errno.h"
#include "blink/hostfs.h"
#include "blink/linux.h"
#include "win32/win32.h"

struct BrokerfsDevice {
  uint32_t mount_id;
};

struct BrokerfsInfo {
  struct HostfsInfo host;
  char *path;
  uint64_t size;
  uint64_t inode;
  uint32_t cursor;
  uint32_t page_index;
  int dir_error;
  struct W32BrokerDirPage page;
  struct dirent entry;
};

static int BrokerfsDup(struct VfsInfo *, struct VfsInfo **);

static struct BrokerfsInfo *BrokerfsNewInfo(
    const char *path, const struct W32BrokerMetadata *metadata) {
  struct BrokerfsInfo *info;
  info = calloc(1, sizeof(*info));
  if (!info) {
    enomem();
    return NULL;
  }
  info->path = strdup(path);
  if (!info->path) {
    free(info);
    enomem();
    return NULL;
  }
  info->host.filefd = -1;
  info->host.mode = metadata->mode;
  info->size = metadata->size;
  info->inode = metadata->inode;
  return info;
}

static int BrokerfsFreeInfo(void *data) {
  struct BrokerfsInfo *info = data;
  if (!info) return 0;
  W32BrokerFreeDirPage(&info->page);
  if (info->host.filefd != -1) close(info->host.filefd);
  free(info->path);
  free(info);
  return 0;
}

static int BrokerfsFreeDevice(void *data) {
  free(data);
  return 0;
}

static int BrokerfsJoin(const struct BrokerfsInfo *parent, const char *name,
                        char output[VFS_PATH_MAX]) {
  size_t parent_len, name_len;
  if (!parent || !name) return efault();
  parent_len = strlen(parent->path);
  name_len = strlen(name);
  if (!name_len || strchr(name, '/') || strchr(name, '\\') ||
      !strcmp(name, ".") || !strcmp(name, "..")) {
    return einval();
  }
  if (parent_len + !!parent_len + name_len >= VFS_PATH_MAX) {
    return enametoolong();
  }
  memcpy(output, parent->path, parent_len);
  if (parent_len) output[parent_len++] = '/';
  memcpy(output + parent_len, name, name_len + 1);
  return 0;
}

static int BrokerfsFillStat(struct VfsInfo *info, struct stat *st) {
  struct BrokerfsInfo *broker;
  if (!info || !st) return efault();
  broker = info->data;
  memset(st, 0, sizeof(*st));
  st->st_dev = info->dev;
  st->st_ino = broker->inode;
  st->st_nlink = 1;
  st->st_mode = broker->host.mode;
  st->st_size = (off_t)broker->size;
  st->st_blksize = 4096;
  st->st_blocks = (broker->size + 511) / 512;
  return 0;
}

static int BrokerfsCreateNode(struct VfsInfo *parent, const char *name,
                              const char *path,
                              const struct W32BrokerMetadata *metadata,
                              struct VfsInfo **output) {
  struct BrokerfsInfo *broker = NULL;
  *output = NULL;
  broker = BrokerfsNewInfo(path, metadata);
  if (!broker) return -1;
  if (VfsCreateInfo(output) == -1) goto Fail;
  (*output)->data = broker;
  unassert(!VfsAcquireDevice(parent->device, &(*output)->device));
  (*output)->name = strdup(name);
  if (!(*output)->name) {
    enomem();
    goto Fail;
  }
  (*output)->namelen = strlen(name);
  (*output)->dev = parent->dev;
  (*output)->ino = metadata->inode;
  (*output)->mode = metadata->mode;
  unassert(!VfsAcquireInfo(parent, &(*output)->parent));
  return 0;
Fail:
  if (*output) {
    unassert(!VfsFreeInfo(*output));
  } else {
    BrokerfsFreeInfo(broker);
  }
  return -1;
}

static int BrokerfsInit(const char *source, u64 flags, const void *data,
                        struct VfsDevice **device, struct VfsMount **mount) {
  struct BrokerfsDevice *broker_device = NULL;
  struct BrokerfsInfo *root_info = NULL;
  struct W32BrokerMetadata metadata;
  unsigned long mount_id;
  char *end;
  (void)data;
  if (!source || !*source || !(flags & MS_RDONLY_LINUX)) return einval();
  mount_id = strtoul(source, &end, 10);
  if (*end || !mount_id || mount_id > UINT32_MAX) return einval();
  if (W32BrokerLookup(mount_id, "", &metadata)) return -1;
  if (!S_ISDIR(metadata.mode)) return enotdir();
  *device = NULL;
  *mount = NULL;
  broker_device = calloc(1, sizeof(*broker_device));
  if (!broker_device) return enomem();
  broker_device->mount_id = mount_id;
  if (VfsCreateDevice(device) == -1) goto Fail;
  (*device)->data = broker_device;
  (*device)->ops = &g_brokerfs.ops;
  *mount = calloc(1, sizeof(**mount));
  if (!*mount) {
    enomem();
    goto Fail;
  }
  if (VfsCreateInfo(&(*mount)->root) == -1) goto Fail;
  unassert(!VfsAcquireDevice(*device, &(*mount)->root->device));
  root_info = BrokerfsNewInfo("", &metadata);
  if (!root_info) goto Fail;
  (*mount)->root->data = root_info;
  (*mount)->root->mode = metadata.mode;
  (*mount)->root->ino = metadata.inode;
  (*device)->root = (*mount)->root;
  return 0;
Fail:
  if (*device) {
    unassert(!VfsFreeDevice(*device));
  } else {
    free(broker_device);
  }
  if (*mount) {
    if ((*mount)->root) {
      unassert(!VfsFreeInfo((*mount)->root));
    }
    free(*mount);
  }
  return -1;
}

static int BrokerfsReadmountentry(struct VfsDevice *device, char **spec,
                                  char **type, char **options) {
  struct BrokerfsDevice *broker = device->data;
  char id[16];
  snprintf(id, sizeof(id), "%u", broker->mount_id);
  *spec = strdup(id);
  *type = strdup("wboxfs");
  *options = strdup("ro");
  if (!*spec || !*type || !*options) {
    free(*spec);
    free(*type);
    free(*options);
    return enomem();
  }
  return 0;
}

static int BrokerfsFinddir(struct VfsInfo *parent, const char *name,
                           struct VfsInfo **output) {
  struct BrokerfsDevice *device;
  struct W32BrokerMetadata metadata;
  char path[VFS_PATH_MAX];
  if (!parent || !name || !output) return efault();
  if (!S_ISDIR(parent->mode)) return enotdir();
  if (!strcmp(name, ".")) {
    unassert(!VfsAcquireInfo(parent, output));
    return 0;
  }
  if (BrokerfsJoin(parent->data, name, path) == -1) return -1;
  device = parent->device->data;
  if (W32BrokerLookup(device->mount_id, path, &metadata)) return -1;
  return BrokerfsCreateNode(parent, name, path, &metadata, output);
}

static int BrokerfsOpen(struct VfsInfo *parent, const char *name, int flags,
                        int mode, struct VfsInfo **output) {
  struct BrokerfsDevice *device;
  struct BrokerfsInfo *broker;
  struct W32BrokerMetadata metadata;
  void *handle = NULL;
  int filefd = -1;
  char path[VFS_PATH_MAX];
  (void)mode;
  if (!parent || !name || !output) return efault();
  if ((flags & O_ACCMODE) != O_RDONLY ||
      (flags & (O_CREAT | O_EXCL | O_TRUNC | O_APPEND))) {
    return erofs();
  }
  if (!strcmp(name, ".")) return BrokerfsDup(parent, output);
  if (BrokerfsJoin(parent->data, name, path) == -1) return -1;
  device = parent->device->data;
  if (W32BrokerLookup(device->mount_id, path, &metadata)) return -1;
  if ((flags & O_DIRECTORY) && !S_ISDIR(metadata.mode)) return enotdir();
  if (S_ISREG(metadata.mode)) {
    if (W32BrokerOpenReadonly(device->mount_id, path, &handle, &metadata)) {
      return -1;
    }
    filefd = W32ImportOwnedHandle(handle, flags);
    if (filefd == -1) return -1;
  } else if (!S_ISDIR(metadata.mode)) {
    return enotsup();
  }
  if (BrokerfsCreateNode(parent, name, path, &metadata, output) == -1) {
    if (filefd != -1) close(filefd);
    return -1;
  }
  broker = (*output)->data;
  broker->host.filefd = filefd;
  return 0;
}

static int BrokerfsAccess(struct VfsInfo *parent, const char *name,
                          mode_t mode, int flags) {
  struct BrokerfsDevice *device;
  struct W32BrokerMetadata metadata;
  char path[VFS_PATH_MAX];
  (void)flags;
  if (mode & W_OK) return eacces();
  if (!strcmp(name, ".")) return 0;
  if (BrokerfsJoin(parent->data, name, path) == -1) return -1;
  device = parent->device->data;
  return W32BrokerLookup(device->mount_id, path, &metadata);
}

static int BrokerfsStat(struct VfsInfo *parent, const char *name,
                        struct stat *st, int flags) {
  struct VfsInfo *node;
  int rc;
  (void)flags;
  if (!strcmp(name, ".")) return BrokerfsFillStat(parent, st);
  if (BrokerfsFinddir(parent, name, &node) == -1) return -1;
  rc = BrokerfsFillStat(node, st);
  unassert(!VfsFreeInfo(node));
  return rc;
}

static int BrokerfsFstat(struct VfsInfo *info, struct stat *st) {
  return BrokerfsFillStat(info, st);
}

static ssize_t BrokerfsReadlink(struct VfsInfo *info, char **output) {
  (void)info;
  (void)output;
  return einval();
}

static int BrokerfsClose(struct VfsInfo *info) {
  struct BrokerfsInfo *broker;
  int rc = 0;
  if (!info) return efault();
  broker = info->data;
  if (broker->host.filefd != -1) {
    rc = close(broker->host.filefd);
    broker->host.filefd = -1;
  }
  return rc;
}

static int BrokerfsDup(struct VfsInfo *info, struct VfsInfo **output) {
  struct BrokerfsInfo *source, *copy = NULL;
  struct W32BrokerMetadata metadata;
  if (!info || !output) return efault();
  *output = NULL;
  source = info->data;
  metadata.mode = source->host.mode;
  metadata.size = source->size;
  metadata.inode = source->inode;
  copy = BrokerfsNewInfo(source->path, &metadata);
  if (!copy) return -1;
  if (source->host.filefd != -1) {
    copy->host.filefd = dup(source->host.filefd);
    if (copy->host.filefd == -1) goto Fail;
  }
  if (VfsCreateInfo(output) == -1) goto Fail;
  (*output)->data = copy;
  unassert(!VfsAcquireDevice(info->device, &(*output)->device));
  if (info->name) {
    (*output)->name = strdup(info->name);
    if (!(*output)->name) {
      enomem();
      goto Fail;
    }
    (*output)->namelen = info->namelen;
  }
  (*output)->dev = info->dev;
  (*output)->ino = info->ino;
  (*output)->mode = info->mode;
  unassert(!VfsAcquireInfo(info->parent, &(*output)->parent));
  return 0;
Fail:
  if (*output) {
    unassert(!VfsFreeInfo(*output));
  } else {
    BrokerfsFreeInfo(copy);
  }
  return -1;
}

static int BrokerfsOpendir(struct VfsInfo *info, struct VfsInfo **output) {
  struct BrokerfsInfo *broker;
  if (!info || !output) return efault();
  if (!S_ISDIR(info->mode)) return enotdir();
  broker = info->data;
  W32BrokerFreeDirPage(&broker->page);
  broker->cursor = 0;
  broker->page_index = 0;
  broker->dir_error = 0;
  unassert(!VfsAcquireInfo(info, output));
  return 0;
}

static struct dirent *BrokerfsReaddir(struct VfsInfo *info) {
  struct BrokerfsInfo *broker;
  struct BrokerfsDevice *device;
  const char *name;
  size_t length;
  if (!info) {
    efault();
    return NULL;
  }
  broker = info->data;
  device = info->device->data;
  if (broker->dir_error) {
    errno = broker->dir_error;
    return NULL;
  }
  if (broker->cursor < 2) {
    name = broker->cursor++ ? ".." : ".";
  } else {
    while (broker->page_index >= broker->page.count) {
      if (broker->page.eof) {
        errno = 0;
        return NULL;
      }
      W32BrokerFreeDirPage(&broker->page);
      if (W32BrokerReadDir(device->mount_id, broker->path,
                           broker->cursor - 2, &broker->page)) {
        broker->dir_error = errno ? errno : EIO;
        return NULL;
      }
      broker->page_index = 0;
      if (!broker->page.count && !broker->page.eof) {
        errno = EPROTO;
        broker->dir_error = errno;
        return NULL;
      }
    }
    name = broker->page.names[broker->page_index++];
    broker->cursor = broker->page.next_cursor + 2;
  }
  length = strlen(name);
  if (length >= sizeof(broker->entry.d_name)) {
    errno = ENAMETOOLONG;
    broker->dir_error = errno;
    return NULL;
  }
  memset(&broker->entry, 0, sizeof(broker->entry));
  memcpy(broker->entry.d_name, name, length + 1);
  {
    const unsigned char *p = (const unsigned char *)name;
    uint64_t hash = 1469598103934665603ull;
    while (*p) {
      hash ^= *p++;
      hash *= 1099511628211ull;
    }
    broker->entry.d_ino = hash ? hash : 1;
  }
  broker->entry.d_reclen = sizeof(broker->entry);
  broker->entry.d_type = DT_UNKNOWN;
  broker->entry.d_off = broker->cursor;
  return &broker->entry;
}

static void BrokerfsRewinddir(struct VfsInfo *info) {
  struct BrokerfsInfo *broker;
  if (!info) {
    efault();
    return;
  }
  broker = info->data;
  W32BrokerFreeDirPage(&broker->page);
  broker->cursor = 0;
  broker->page_index = 0;
  broker->dir_error = 0;
}

static int BrokerfsClosedir(struct VfsInfo *info) {
  if (!info) return efault();
  BrokerfsRewinddir(info);
  unassert(!VfsFreeInfo(info));
  return 0;
}

static int BrokerfsHex(char c) {
  if (c >= '0' && c <= '9') return c - '0';
  if (c >= 'a' && c <= 'f') return c - 'a' + 10;
  if (c >= 'A' && c <= 'F') return c - 'A' + 10;
  return -1;
}

static void BrokerfsForgetManifest(void) {
  _putenv_s("WBOX_BROKER_MOUNTS", "");
  SetEnvironmentVariableA("WBOX_BROKER_MOUNTS", NULL);
}

int BrokerfsMountConfigured(void) {
  const char *manifest = getenv("WBOX_BROKER_MOUNTS");
  const char *p;
  char id[16], target[VFS_PATH_MAX];
  size_t id_len, target_len;
  unsigned count = 0;
  if (!manifest || !*manifest) return 0;
  p = manifest;
  while (*p) {
    const char *colon = strchr(p, ':');
    const char *end = strchr(p, ';');
    const char *hex_end = end ? end : p + strlen(p);
    char *id_end;
    unsigned long mount_id;
    if (!colon || colon >= hex_end || ++count > 64) goto Bad;
    id_len = colon - p;
    if (!id_len || id_len >= sizeof(id)) goto Bad;
    memcpy(id, p, id_len);
    id[id_len] = 0;
    mount_id = strtoul(id, &id_end, 10);
    if (*id_end || !mount_id || mount_id > UINT32_MAX) goto Bad;
    ++colon;
    if ((hex_end - colon) & 1) goto Bad;
    target_len = 0;
    while (colon < hex_end) {
      int hi = BrokerfsHex(*colon++);
      int lo = BrokerfsHex(*colon++);
      if (hi < 0 || lo < 0 || target_len + 1 >= sizeof(target)) goto Bad;
      target[target_len++] = hi << 4 | lo;
    }
    target[target_len] = 0;
    if (!target_len || target[0] != '/' || !strcmp(target, "/")) goto Bad;
    if (VfsMount(id, target, "wboxfs", MS_RDONLY_LINUX, NULL) == -1) {
      BrokerfsForgetManifest();
      return -1;
    }
    p = end ? end + 1 : hex_end;
  }
  BrokerfsForgetManifest();
  return 0;
Bad:
  BrokerfsForgetManifest();
  return einval();
}

struct VfsSystem g_brokerfs = {
    .name = "wboxfs",
    .nodev = true,
    .ops =
        {
            .Init = BrokerfsInit,
            .Freeinfo = BrokerfsFreeInfo,
            .Freedevice = BrokerfsFreeDevice,
            .Readmountentry = BrokerfsReadmountentry,
            .Finddir = BrokerfsFinddir,
            .Readlink = BrokerfsReadlink,
            .Open = BrokerfsOpen,
            .Access = BrokerfsAccess,
            .Stat = BrokerfsStat,
            .Fstat = BrokerfsFstat,
            .Close = BrokerfsClose,
            .Read = HostfsRead,
            .Pread = HostfsPread,
            .Readv = HostfsReadv,
            .Preadv = HostfsPreadv,
            .Seek = HostfsSeek,
            .Fsync = HostfsFsync,
            .Fdatasync = HostfsFdatasync,
            .Flock = HostfsFlock,
            .Fcntl = HostfsFcntl,
            .Ioctl = HostfsIoctl,
            .Dup = BrokerfsDup,
            .Poll = HostfsPoll,
            .Opendir = BrokerfsOpendir,
            .Readdir = BrokerfsReaddir,
            .Rewinddir = BrokerfsRewinddir,
            .Closedir = BrokerfsClosedir,
            .Mmap = HostfsMmap,
            .Munmap = HostfsMunmap,
            .Mprotect = HostfsMprotect,
            .Msync = HostfsMsync,
        }};

#endif
