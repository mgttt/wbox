#include "win32/win32.h"

#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <windows.h>

#define WBOX_BROKER_MAGIC 0x57424f58u
#define WBOX_BROKER_VERSION 1u
#define WBOX_BROKER_HELLO 1u
#define WBOX_BROKER_PING 2u
#define WBOX_BROKER_OPEN 3u
#define WBOX_BROKER_LOOKUP 4u
#define WBOX_BROKER_READDIR 5u
#define WBOX_BROKER_HEADER 24u
#define WBOX_BROKER_MAX_PAYLOAD 4096u
#define WBOX_BROKER_MAX_PATH 1024u

static HANDLE g_broker;
static SRWLOCK g_broker_lock = SRWLOCK_INIT;
static unsigned long long g_request_id = 2;

static void Put16(unsigned char *p, unsigned value) {
  p[0] = value;
  p[1] = value >> 8;
}

static void Put32(unsigned char *p, unsigned long value) {
  p[0] = value;
  p[1] = value >> 8;
  p[2] = value >> 16;
  p[3] = value >> 24;
}

static void Put64(unsigned char *p, unsigned long long value) {
  Put32(p, (unsigned long)value);
  Put32(p + 4, (unsigned long)(value >> 32));
}

static unsigned Get16(const unsigned char *p) {
  return p[0] | (unsigned)p[1] << 8;
}

static unsigned long Get32(const unsigned char *p) {
  return p[0] | (unsigned long)p[1] << 8 | (unsigned long)p[2] << 16 |
         (unsigned long)p[3] << 24;
}

static unsigned long long Get64(const unsigned char *p) {
  return Get32(p) | (unsigned long long)Get32(p + 4) << 32;
}

static int WriteMessage(const void *data, DWORD size) {
  DWORD wrote;
  return WriteFile(g_broker, data, size, &wrote, NULL) && wrote == size ? 0
                                                                       : -1;
}

static int ReadMessage(void *data, DWORD size) {
  DWORD got;
  return ReadFile(g_broker, data, size, &got, NULL) && got == size ? 0 : -1;
}

static int HexNibble(char c) {
  if (c >= '0' && c <= '9') return c - '0';
  if (c >= 'a' && c <= 'f') return c - 'a' + 10;
  if (c >= 'A' && c <= 'F') return c - 'A' + 10;
  return -1;
}

static int SendRequest(unsigned opcode, unsigned long long request_id,
                       const unsigned char *payload, unsigned payload_size) {
  unsigned char header[WBOX_BROKER_HEADER] = {0};
  Put32(header, WBOX_BROKER_MAGIC);
  Put16(header + 4, WBOX_BROKER_VERSION);
  Put16(header + 6, opcode);
  Put64(header + 8, request_id);
  Put32(header + 16, payload_size);
  if (WriteMessage(header, sizeof(header))) return -1;
  if (payload_size && WriteMessage(payload, payload_size)) return -1;
  return 0;
}

static int ReadResponse(unsigned opcode, unsigned long long request_id,
                        unsigned char payload[WBOX_BROKER_MAX_PAYLOAD],
                        DWORD *payload_size, int32_t *response_status) {
  unsigned char header[WBOX_BROKER_HEADER];
  if (ReadMessage(header, sizeof(header))) return -1;
  if (Get32(header) != WBOX_BROKER_MAGIC ||
      Get16(header + 4) != WBOX_BROKER_VERSION ||
      Get16(header + 6) != opcode || Get64(header + 8) != request_id ||
      Get32(header + 20) > WBOX_BROKER_MAX_PAYLOAD) {
    SetLastError(ERROR_INVALID_DATA);
    return -1;
  }
  *response_status = (int32_t)Get32(header + 16);
  *payload_size = Get32(header + 20);
  if (*payload_size && ReadMessage(payload, *payload_size)) return -1;
  return 0;
}

static int ReadOk(unsigned opcode, unsigned long long request_id) {
  unsigned char payload[WBOX_BROKER_MAX_PAYLOAD];
  DWORD payload_size;
  int32_t status;
  if (ReadResponse(opcode, request_id, payload, &payload_size, &status) ||
      status || payload_size) {
    SetLastError(ERROR_INVALID_DATA);
    return -1;
  }
  return 0;
}

static int BrokerErrno(int32_t status) {
  switch (status) {
    case -2:
      return ENOENT;
    case -13:
      return EACCES;
    default:
      return EPROTO;
  }
}

static int BrokerCall(unsigned opcode, const unsigned char *request,
                      DWORD request_size,
                      unsigned char response[WBOX_BROKER_MAX_PAYLOAD],
                      DWORD *response_size) {
  unsigned long long request_id;
  int32_t status;
  int rc = -1;
  if (!g_broker) {
    errno = ENOTCONN;
    return -1;
  }
  AcquireSRWLockExclusive(&g_broker_lock);
  request_id = ++g_request_id;
  if (SendRequest(opcode, request_id, request, request_size) ||
      ReadResponse(opcode, request_id, response, response_size, &status)) {
    CloseHandle(g_broker);
    g_broker = NULL;
    errno = EIO;
  } else if (status) {
    errno = BrokerErrno(status);
  } else {
    rc = 0;
  }
  ReleaseSRWLockExclusive(&g_broker_lock);
  return rc;
}

static int PathSize(const char *path, size_t fixed, size_t *path_size) {
  if (!path) {
    errno = EFAULT;
    return -1;
  }
  *path_size = strlen(path);
  if (*path_size > WBOX_BROKER_MAX_PATH ||
      fixed + *path_size > WBOX_BROKER_MAX_PAYLOAD) {
    errno = ENAMETOOLONG;
    return -1;
  }
  return 0;
}

static void ForgetCredentials(void) {
  _putenv_s("WBOX_BROKER_HANDLE", "");
  _putenv_s("WBOX_BROKER_GENERATION", "");
  _putenv_s("WBOX_BROKER_NONCE", "");
  SetEnvironmentVariableA("WBOX_BROKER_HANDLE", NULL);
  SetEnvironmentVariableA("WBOX_BROKER_GENERATION", NULL);
  SetEnvironmentVariableA("WBOX_BROKER_NONCE", NULL);
}

int W32BrokerInit(void) {
  const char *handle_text = getenv("WBOX_BROKER_HANDLE");
  const char *generation_text;
  const char *nonce_text;
  unsigned long long handle_value;
  unsigned long long generation;
  unsigned char hello[24];
  char *end;
  int i;

  if (!handle_text || !*handle_text) return 0;
  generation_text = getenv("WBOX_BROKER_GENERATION");
  nonce_text = getenv("WBOX_BROKER_NONCE");
  if (!generation_text || !nonce_text || strlen(nonce_text) != 32) goto Bad;

  handle_value = _strtoui64(handle_text, &end, 10);
  if (!*handle_text || *end || !handle_value) goto Bad;
  generation = _strtoui64(generation_text, &end, 10);
  if (!*generation_text || *end) goto Bad;
  Put64(hello, generation);
  for (i = 0; i < 16; ++i) {
    int hi = HexNibble(nonce_text[i * 2]);
    int lo = HexNibble(nonce_text[i * 2 + 1]);
    if (hi < 0 || lo < 0) goto Bad;
    hello[8 + i] = hi << 4 | lo;
  }

  g_broker = (HANDLE)(uintptr_t)handle_value;
  ForgetCredentials();
  if (SendRequest(WBOX_BROKER_HELLO, 1, hello, sizeof(hello)) ||
      ReadOk(WBOX_BROKER_HELLO, 1) ||
      SendRequest(WBOX_BROKER_PING, 2, NULL, 0) ||
      ReadOk(WBOX_BROKER_PING, 2)) {
    fprintf(stderr, "wbox-linux: broker handshake failed (win32=%lu)\n",
            GetLastError());
    CloseHandle(g_broker);
    g_broker = NULL;
    return -1;
  }
  return 0;

Bad:
  ForgetCredentials();
  SetLastError(ERROR_INVALID_DATA);
  fprintf(stderr, "wbox-linux: invalid broker bootstrap data\n");
  return -1;
}

int W32BrokerLookup(uint32_t mount_id, const char *path,
                    struct W32BrokerMetadata *out) {
  unsigned char request[4 + WBOX_BROKER_MAX_PATH];
  unsigned char response[WBOX_BROKER_MAX_PAYLOAD];
  DWORD response_size;
  size_t path_size;
  if (!out) {
    errno = EFAULT;
    return -1;
  }
  if (!mount_id) {
    errno = EINVAL;
    return -1;
  }
  if (PathSize(path, 4, &path_size)) return -1;
  Put32(request, mount_id);
  memcpy(request + 4, path, path_size);
  if (BrokerCall(WBOX_BROKER_LOOKUP, request, 4 + path_size, response,
                 &response_size)) {
    return -1;
  }
  if (response_size != 24) {
    errno = EPROTO;
    return -1;
  }
  out->mode = Get32(response);
  out->size = Get64(response + 8);
  out->inode = Get64(response + 16);
  return 0;
}

int W32BrokerOpenReadonly(uint32_t mount_id, const char *path,
                          void **owned_handle,
                          struct W32BrokerMetadata *metadata) {
  unsigned char request[12 + WBOX_BROKER_MAX_PATH];
  unsigned char response[WBOX_BROKER_MAX_PAYLOAD];
  unsigned long long value;
  DWORD response_size;
  size_t path_size;
  if (!owned_handle || !metadata) {
    errno = EFAULT;
    return -1;
  }
  *owned_handle = NULL;
  if (!mount_id) {
    errno = EINVAL;
    return -1;
  }
  if (PathSize(path, 12, &path_size)) return -1;
  if (!path_size) {
    errno = EINVAL;
    return -1;
  }
  Put32(request, mount_id);
  Put32(request + 4, 0);
  Put32(request + 8, 0);
  memcpy(request + 12, path, path_size);
  if (BrokerCall(WBOX_BROKER_OPEN, request, 12 + path_size, response,
                 &response_size)) {
    return -1;
  }
  if (response_size != 32) {
    errno = EPROTO;
    return -1;
  }
  value = Get64(response);
  if (!value || value > UINTPTR_MAX ||
      (HANDLE)(uintptr_t)value == INVALID_HANDLE_VALUE) {
    errno = EPROTO;
    return -1;
  }
  *owned_handle = (void *)(uintptr_t)value;
  metadata->mode = Get32(response + 8);
  metadata->size = Get64(response + 16);
  metadata->inode = Get64(response + 24);
  return 0;
}

void W32BrokerFreeDirPage(struct W32BrokerDirPage *page) {
  uint32_t i;
  if (!page) return;
  if (page->names) {
    for (i = 0; i < page->count; ++i) free(page->names[i]);
  }
  free(page->names);
  memset(page, 0, sizeof(*page));
}

int W32BrokerReadDir(uint32_t mount_id, const char *path, uint32_t cursor,
                     struct W32BrokerDirPage *out) {
  unsigned char request[8 + WBOX_BROKER_MAX_PATH];
  unsigned char response[WBOX_BROKER_MAX_PAYLOAD];
  DWORD response_size;
  size_t path_size, offset;
  uint32_t i;
  if (!out) {
    errno = EFAULT;
    return -1;
  }
  memset(out, 0, sizeof(*out));
  if (!mount_id) {
    errno = EINVAL;
    return -1;
  }
  if (PathSize(path, 8, &path_size)) return -1;
  Put32(request, mount_id);
  Put32(request + 4, cursor);
  memcpy(request + 8, path, path_size);
  if (BrokerCall(WBOX_BROKER_READDIR, request, 8 + path_size, response,
                 &response_size)) {
    return -1;
  }
  if (response_size < 12) goto Protocol;
  out->next_cursor = Get32(response);
  out->eof = Get32(response + 4);
  out->count = Get32(response + 8);
  if (out->eof > 1 || out->count > (response_size - 12) / 2) goto Protocol;
  if ((uint64_t)cursor + out->count != out->next_cursor ||
      (!out->eof && !out->count)) {
    goto Protocol;
  }
  if (out->count) {
    out->names = calloc(out->count, sizeof(*out->names));
    if (!out->names) {
      errno = ENOMEM;
      goto Fail;
    }
  }
  offset = 12;
  for (i = 0; i < out->count; ++i) {
    size_t length;
    if (offset + 2 > response_size) goto Protocol;
    length = Get16(response + offset);
    offset += 2;
    if (!length || length > 255 || offset + length > response_size ||
        memchr(response + offset, 0, length) ||
        memchr(response + offset, '/', length) ||
        memchr(response + offset, '\\', length) ||
        !MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS,
                             (const char *)response + offset, length, NULL, 0)) {
      goto Protocol;
    }
    out->names[i] = malloc(length + 1);
    if (!out->names[i]) {
      errno = ENOMEM;
      goto Fail;
    }
    memcpy(out->names[i], response + offset, length);
    out->names[i][length] = 0;
    offset += length;
  }
  if (offset != response_size) goto Protocol;
  return 0;

Protocol:
  errno = EPROTO;
Fail:
  W32BrokerFreeDirPage(out);
  return -1;
}
