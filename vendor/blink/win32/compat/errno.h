#ifndef WBOX_COMPAT_ERRNO_H
#define WBOX_COMPAT_ERRNO_H
#include_next <errno.h>
#ifndef ENOTSUP
#define ENOTSUP EOPNOTSUPP
#endif
#ifndef EOPNOTSUPP
#define EOPNOTSUPP 95
#endif
#ifndef ENODATA
#define ENODATA 61
#endif
#ifndef EOVERFLOW
#define EOVERFLOW 75
#endif
#ifndef EBADFD
#define EBADFD 77
#endif
#ifndef ERESTART
#define ERESTART 85
#endif
#ifndef EREMOTEIO
#define EREMOTEIO 121
#endif
#ifndef EDQUOT
#define EDQUOT 122
#endif
#ifndef ENOMEDIUM
#define ENOMEDIUM 123
#endif
#ifndef EMEDIUMTYPE
#define EMEDIUMTYPE 124
#endif
#ifndef ECANCELED
#define ECANCELED 125
#endif
#ifndef ENOKEY
#define ENOKEY 126
#endif
#ifndef EOWNERDEAD
#define EOWNERDEAD 130
#endif
#ifndef ENOTRECOVERABLE
#define ENOTRECOVERABLE 131
#endif
#ifndef ERFKILL
#define ERFKILL 132
#endif
#ifndef EHWPOISON
#define EHWPOISON 133
#endif
#ifndef ETIME
#define ETIME 62
#endif
#ifndef ETXTBSY
#define ETXTBSY 26
#endif
#ifndef EMULTIHOP
#define EMULTIHOP 72
#endif
#ifndef ENOLINK
#define ENOLINK 67
#endif
#ifndef EPROTO
#define EPROTO 71
#endif
#ifndef EBADMSG
#define EBADMSG 74
#endif
#ifndef EIDRM
#define EIDRM 43
#endif
#ifndef ENOMSG
#define ENOMSG 42
#endif
#ifndef ENOSR
#define ENOSR 63
#endif
#ifndef ENOSTR
#define ENOSTR 60
#endif
#ifndef ECHRNG
#define ECHRNG 44
#endif
#ifndef EL2HLT
#define EL2HLT 51
#endif
#ifndef EL3HLT
#define EL3HLT 46
#endif
#ifndef EL3RST
#define EL3RST 47
#endif
#ifndef ELNRNG
#define ELNRNG 48
#endif
#ifndef EUNATCH
#define EUNATCH 49
#endif
#ifndef ENOCSI
#define ENOCSI 50
#endif
#ifndef EADV
#define EADV 68
#endif
#ifndef ESRMNT
#define ESRMNT 69
#endif
#ifndef ECOMM
#define ECOMM 70
#endif
#ifndef EDOTDOT
#define EDOTDOT 73
#endif
#ifndef ENOTUNIQ
#define ENOTUNIQ 76
#endif
#ifndef ELIBACC
#define ELIBACC 79
#endif
#ifndef ELIBBAD
#define ELIBBAD 80
#endif
#ifndef ELIBSCN
#define ELIBSCN 81
#endif
#ifndef ELIBMAX
#define ELIBMAX 82
#endif
#ifndef ELIBEXEC
#define ELIBEXEC 83
#endif
#ifndef ESTRPIPE
#define ESTRPIPE 86
#endif
#ifndef EUSERS
#define EUSERS 87
#endif
#ifndef ESOCKTNOSUPPORT
#define ESOCKTNOSUPPORT 94
#endif
#ifndef EPFNOSUPPORT
#define EPFNOSUPPORT 96
#endif
#ifndef EAFNOSUPPORT
#define EAFNOSUPPORT 97
#endif
#ifndef EADDRINUSE
#define EADDRINUSE 98
#endif
#ifndef EADDRNOTAVAIL
#define EADDRNOTAVAIL 99
#endif
#ifndef ENETDOWN
#define ENETDOWN 100
#endif
#ifndef ENETUNREACH
#define ENETUNREACH 101
#endif
#ifndef ENETRESET
#define ENETRESET 102
#endif
#ifndef ECONNABORTED
#define ECONNABORTED 103
#endif
#ifndef ECONNRESET
#define ECONNRESET 104
#endif
#ifndef ENOBUFS
#define ENOBUFS 105
#endif
#ifndef EISCONN
#define EISCONN 106
#endif
#ifndef ENOTCONN
#define ENOTCONN 107
#endif
#ifndef ESHUTDOWN
#define ESHUTDOWN 108
#endif
#ifndef ETOOMANYREFS
#define ETOOMANYREFS 109
#endif
#ifndef ETIMEDOUT
#define ETIMEDOUT 110
#endif
#ifndef ECONNREFUSED
#define ECONNREFUSED 111
#endif
#ifndef EHOSTDOWN
#define EHOSTDOWN 112
#endif
#ifndef EHOSTUNREACH
#define EHOSTUNREACH 113
#endif
#ifndef EALREADY
#define EALREADY 114
#endif
#ifndef EINPROGRESS
#define EINPROGRESS 115
#endif
#ifndef ESTALE
#define ESTALE 116
#endif
#ifndef EDESTADDRREQ
#define EDESTADDRREQ 89
#endif
#ifndef EMSGSIZE
#define EMSGSIZE 90
#endif
#ifndef EPROTOTYPE
#define EPROTOTYPE 91
#endif
#ifndef ENOPROTOOPT
#define ENOPROTOOPT 92
#endif
#ifndef EPROTONOSUPPORT
#define EPROTONOSUPPORT 93
#endif
#ifndef EISNAM
#define EISNAM 120
#endif
#ifndef ENAVAIL
#define ENAVAIL 119
#endif
#ifndef EREMOTE
#define EREMOTE 66
#endif
#ifndef ENOPKG
#define ENOPKG 65
#endif
#ifndef EBADE
#define EBADE 52
#endif
#ifndef EBADR
#define EBADR 53
#endif
#ifndef EXFULL
#define EXFULL 54
#endif
#ifndef ENOANO
#define ENOANO 55
#endif
#ifndef EBADRQC
#define EBADRQC 56
#endif
#ifndef EBADSLT
#define EBADSLT 57
#endif
#ifndef EDEADLOCK
#define EDEADLOCK EDEADLK
#endif
#ifndef EBFONT
#define EBFONT 59
#endif
#ifndef ENONET
#define ENONET 64
#endif
#ifndef ENOEXEC
#define ENOEXEC 8
#endif
#ifndef ELOOP
#define ELOOP 40
#endif
#ifndef ENOTEMPTY
#define ENOTEMPTY 39
#endif
#ifndef ENAMETOOLONG
#define ENAMETOOLONG 36
#endif
#ifndef EMLINK
#define EMLINK 31
#endif
#ifndef EDOM
#define EDOM 33
#endif
#ifndef ERANGE
#define ERANGE 34
#endif
#endif
