#ifndef WBOX_COMPAT_GRP_H
#define WBOX_COMPAT_GRP_H
#include <sys/types.h>
struct group { char *gr_name; char *gr_passwd; gid_t gr_gid; char **gr_mem; };
struct group *getgrgid(gid_t);
struct group *getgrnam(const char *);
int getgroups(int, gid_t *);
int setgroups(size_t, const gid_t *);
int initgroups(const char *, gid_t);
#endif
