#include "win32/win32.h"

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <windows.h>

#define WBOX_BROKER_MAGIC 0x57424f58u
#define WBOX_BROKER_VERSION 1u
#define WBOX_BROKER_HELLO 1u
#define WBOX_BROKER_PING 2u
#define WBOX_BROKER_HEADER 24u

static HANDLE g_broker;

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

static int ReadOk(unsigned opcode, unsigned long long request_id) {
  unsigned char header[WBOX_BROKER_HEADER];
  if (ReadMessage(header, sizeof(header))) return -1;
  if (Get32(header) != WBOX_BROKER_MAGIC ||
      Get16(header + 4) != WBOX_BROKER_VERSION ||
      Get16(header + 6) != opcode || Get64(header + 8) != request_id ||
      (long)Get32(header + 16) != 0 || Get32(header + 20) != 0) {
    SetLastError(ERROR_INVALID_DATA);
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
