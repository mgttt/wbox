// wbox win32: centralized host-error -> Linux-errno mapping tables.
// Every Win32/WSA failure code that the port translates flows through one
// of these three tables; previously they were scattered as static
// functions across w32fd.c (W32Err) and w32sock.c (WsaErrno, GaiErr).
// (The guest-facing errno ABI translation, including the mingw
// EHOSTDOWN == EINPROGRESS alias guard, lives in blink/xlat.c.)
#include <errno.h>
#include <netdb.h>

#include <windows.h>

#include "win32.h"

// GetLastError() code -> Linux errno
int W32ErrFromHost(unsigned long e) {
  switch (e) {
    case ERROR_FILE_NOT_FOUND:
    case ERROR_PATH_NOT_FOUND:
      return ENOENT;
    case ERROR_ACCESS_DENIED:
      return EACCES;
    case ERROR_ALREADY_EXISTS:
    case ERROR_FILE_EXISTS:
      return EEXIST;
    case ERROR_INVALID_HANDLE:
      return EBADF;
    case ERROR_INVALID_PARAMETER:
      return EINVAL;
    case ERROR_NOT_ENOUGH_MEMORY:
      return ENOMEM;
    case ERROR_WRITE_PROTECT:
      return EROFS;
    case ERROR_SHARING_VIOLATION:
    case ERROR_LOCK_VIOLATION:
      return EACCES;
    case ERROR_HANDLE_EOF:
      return 0;
    case ERROR_BROKEN_PIPE:
      return EPIPE;
    case ERROR_DISK_FULL:
      return ENOSPC;
    case ERROR_DIR_NOT_EMPTY:
      return ENOTEMPTY;
    case ERROR_DIRECTORY:
      return ENOTDIR;
    case ERROR_IS_SUBSTED:
      return EEXIST;
    default:
      return EINVAL;
  }
}

// WSAGetLastError() code -> Linux errno (numeric winsock constants so this
// table needs no winsock headers)
int W32ErrFromWsa(int e) {
  switch (e) {
    case 10004: return EINTR;
    case 10013: return EACCES;
    case 10014: return EFAULT;
    case 10022: return EINVAL;
    case 10024: return EMFILE;
    case 10035: return EAGAIN;      // WSAEWOULDBLOCK
    case 10036: return EINPROGRESS;
    case 10037: return EALREADY;
    case 10038: return ENOTSOCK;
    case 10039: return EDESTADDRREQ;
    case 10040: return EMSGSIZE;
    case 10041: return EPROTOTYPE;
    case 10042: return ENOPROTOOPT;
    case 10043: return EPROTONOSUPPORT;
    case 10044: return ESOCKTNOSUPPORT;
    case 10045: return EOPNOTSUPP;
    case 10046: return EPFNOSUPPORT;
    case 10047: return EAFNOSUPPORT;
    case 10048: return EADDRINUSE;
    case 10049: return EADDRNOTAVAIL;
    case 10050: return ENETDOWN;
    case 10051: return ENETUNREACH;
    case 10052: return ENETRESET;
    case 10053: return ECONNABORTED;
    case 10054: return ECONNRESET;
    case 10055: return ENOBUFS;
    case 10056: return EISCONN;
    case 10057: return ENOTCONN;
    case 10058: return EPIPE;       // WSAESHUTDOWN
    case 10060: return ETIMEDOUT;
    case 10061: return ECONNREFUSED;
    case 10065: return EHOSTUNREACH;
    case 11001: return ENOENT;      // WSAHOST_NOT_FOUND
    case 11002: return EAGAIN;      // WSATRY_AGAIN
    case 11003: return EIO;         // WSANO_RECOVERY
    case 11004: return ENOENT;      // WSANO_DATA
    default: return EINVAL;
  }
}

// winsock getaddrinfo return code -> Linux EAI_* (positive winsock codes:
// WSATRY_AGAIN / WSAHOST_NOT_FOUND / ...). The compat netdb.h supplies
// the EAI_* constants.
int W32GaiErrFromWsa(int wsrc) {
  switch (wsrc) {
    case 11002: return EAI_AGAIN;
    case 11001:
    case 11004: return EAI_NONAME;
    default: return EAI_FAIL;
  }
}
