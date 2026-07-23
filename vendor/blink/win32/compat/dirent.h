#ifndef WBOX_COMPAT_DIRENT_H
#define WBOX_COMPAT_DIRENT_H
#include <sys/types.h>
#define DT_UNKNOWN 0
#define DT_FIFO 1
#define DT_CHR 2
#define DT_DIR 4
#define DT_BLK 6
#define DT_REG 8
#define DT_LNK 10
#define DT_SOCK 12
struct dirent {
  ino_t d_ino;
  off_t d_off;
  unsigned short d_reclen;
  unsigned char d_type;
  char d_name[256];
};
typedef struct __wbox_DIR DIR;
DIR *opendir(const char *);
DIR *fdopendir(int);
int closedir(DIR *);
struct dirent *readdir(DIR *);
void rewinddir(DIR *);
void seekdir(DIR *, long);
long telldir(DIR *);
int dirfd(DIR *);
int alphasort(const struct dirent **, const struct dirent **);
int scandir(const char *, struct dirent ***, int (*)(const struct dirent *),
            int (*)(const struct dirent **, const struct dirent **));
#endif
