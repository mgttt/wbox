//! `TlsStream`：握手完成后把记录层包成普通的 `Read`/`Write`。
//!
//! 调用方（`wbox-http`）不该看见记录、序号或密钥——它只想要一根字节管道。

use crate::handshake::{self, Handshake};
use crate::record::{self, ContentType, Keys};
use std::io::{self, Read, Write};

/// 一条已完成握手的 TLS 连接。
pub struct TlsStream<S: Read + Write> {
    inner: S,
    client: Keys,
    server: Keys,
    /// 已解密、尚未被调用方读走的应用数据。
    inbox: Vec<u8>,
    /// 读取位置（不用 `drain` 是为了避免每次读都搬移整个缓冲）。
    inbox_pos: usize,
    /// 对端已发 close_notify 或连接已关。
    eof: bool,
}

impl<S: Read + Write> TlsStream<S> {
    /// 与 `hostname` 完成 TLS 1.3 握手。
    ///
    /// `now` 是当前 Unix 秒（用于证书有效期）；`extra_roots` 是在内置根之外
    /// **追加**的信任根（`SSL_CERT_FILE` 走这条路），不替换内置根。
    pub fn connect(
        mut inner: S,
        hostname: &str,
        now: i64,
        extra_roots: &[Vec<u8>],
    ) -> io::Result<Self> {
        let Handshake { secrets } =
            handshake::client_handshake(&mut inner, hostname, now, extra_roots)
                .map_err(io::Error::other)?;
        Ok(TlsStream {
            inner,
            client: secrets.client,
            server: secrets.server,
            inbox: Vec::new(),
            inbox_pos: 0,
            eof: false,
        })
    }

    /// 读一条记录并把应用数据放进 inbox。返回 `false` 表示到达流末尾。
    fn pump(&mut self) -> io::Result<bool> {
        let mut header = [0u8; 5];
        match read_full(&mut self.inner, &mut header) {
            Ok(true) => {}
            // 对端直接断开而没发 close_notify。这在实践中很常见
            // （尤其 `Connection: close` 的 HTTP 响应），当正常结束处理。
            Ok(false) => {
                self.eof = true;
                return Ok(false);
            }
            Err(e) => return Err(e),
        }
        let len = u16::from_be_bytes([header[3], header[4]]) as usize;
        if len > record::MAX_CIPHERTEXT {
            return Err(io::Error::other(format!("TLS：记录过长（{len}）")));
        }
        let mut body = vec![0u8; len];
        self.inner.read_exact(&mut body)?;

        match record::ContentType::from_u8(header[0]) {
            // 握手结束后还可能收到中间设备兼容用的 CCS，忽略。
            Some(ContentType::ChangeCipherSpec) => return Ok(true),
            Some(ContentType::ApplicationData) => {}
            _ => return Err(io::Error::other("TLS：收到意外的记录类型")),
        }

        let ty = self
            .server
            .open(&header, &mut body)
            .map_err(io::Error::other)?;
        match ty {
            ContentType::ApplicationData => {
                // 攒够一整条再挪：inbox 已读完的部分先压掉。
                if self.inbox_pos > 0 && self.inbox_pos == self.inbox.len() {
                    self.inbox.clear();
                    self.inbox_pos = 0;
                }
                self.inbox.extend_from_slice(&body);
            }
            ContentType::Alert => {
                // close_notify(0) 是正常收尾，其它 alert 是错误。
                if body.len() >= 2 && body[1] == 0 {
                    self.eof = true;
                    return Ok(false);
                }
                return Err(io::Error::other(format!(
                    "TLS：对端发来 alert（代码 {}）",
                    body.get(1).copied().unwrap_or(255)
                )));
            }
            // 握手后的 NewSessionTicket / KeyUpdate 之类：我们不做会话恢复，
            // 直接忽略。KeyUpdate 若真的到来会导致后续解密失败并报错——
            // 那是**明确失败**，不是静默错乱。
            ContentType::Handshake => {}
            ContentType::ChangeCipherSpec => {}
        }
        Ok(true)
    }
}

/// 读满 `buf`；一开始就 EOF 时返回 `Ok(false)`。
fn read_full(r: &mut impl Read, buf: &mut [u8]) -> io::Result<bool> {
    let mut got = 0;
    while got < buf.len() {
        match r.read(&mut buf[got..]) {
            Ok(0) => {
                if got == 0 {
                    return Ok(false);
                }
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "TLS：记录头读到一半连接就断了",
                ));
            }
            Ok(n) => got += n,
            Err(ref e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(true)
}

impl<S: Read + Write> Read for TlsStream<S> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        while self.inbox_pos >= self.inbox.len() {
            if self.eof {
                return Ok(0);
            }
            if !self.pump()? {
                return Ok(0);
            }
        }
        let n = buf.len().min(self.inbox.len() - self.inbox_pos);
        buf[..n].copy_from_slice(&self.inbox[self.inbox_pos..self.inbox_pos + n]);
        self.inbox_pos += n;
        Ok(n)
    }
}

impl<S: Read + Write> Write for TlsStream<S> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        // 超过一条记录的明文上限就要切分——这是协议硬限制，
        // 不切的话对端会直接断开。
        let n = buf.len().min(record::MAX_PLAINTEXT);
        let wire = self.client.seal(ContentType::ApplicationData, &buf[..n]);
        self.inner.write_all(&wire)?;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 一根内存里的双向管道，用来在不起真实服务器的前提下测记录层往返。
    struct Loopback {
        to_peer: Vec<u8>,
        from_peer: std::io::Cursor<Vec<u8>>,
    }

    impl Read for Loopback {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.from_peer.read(buf)
        }
    }
    impl Write for Loopback {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.to_peer.extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    /// 直接构造一个握手后的流（跳过握手，只测记录层这一段）。
    fn make(server_records: Vec<u8>) -> TlsStream<Loopback> {
        let key = [0x33u8; 16];
        let iv = [0x44u8; 12];
        TlsStream {
            inner: Loopback {
                to_peer: Vec::new(),
                from_peer: std::io::Cursor::new(server_records),
            },
            client: Keys::new(&key, &iv),
            server: Keys::new(&key, &iv),
            inbox: Vec::new(),
            inbox_pos: 0,
            eof: false,
        }
    }

    fn seal(seq_keys: &mut Keys, ty: ContentType, data: &[u8]) -> Vec<u8> {
        seq_keys.seal(ty, data)
    }

    #[test]
    fn reads_application_data_across_records() {
        let key = [0x33u8; 16];
        let iv = [0x44u8; 12];
        let mut server = Keys::new(&key, &iv);
        let mut wire = seal(&mut server, ContentType::ApplicationData, b"hello ");
        wire.extend_from_slice(&seal(&mut server, ContentType::ApplicationData, b"world"));

        let mut s = make(wire);
        let mut out = String::new();
        s.read_to_string(&mut out).unwrap();
        assert_eq!(out, "hello world");
    }

    #[test]
    fn close_notify_is_a_clean_eof() {
        let key = [0x33u8; 16];
        let iv = [0x44u8; 12];
        let mut server = Keys::new(&key, &iv);
        let mut wire = seal(&mut server, ContentType::ApplicationData, b"data");
        // alert(warning=1, close_notify=0)
        wire.extend_from_slice(&seal(&mut server, ContentType::Alert, &[1, 0]));

        let mut s = make(wire);
        let mut out = Vec::new();
        s.read_to_end(&mut out).unwrap();
        assert_eq!(out, b"data");
    }

    #[test]
    fn fatal_alert_becomes_an_error() {
        let key = [0x33u8; 16];
        let iv = [0x44u8; 12];
        let mut server = Keys::new(&key, &iv);
        // alert(fatal=2, unknown_ca=48)
        let wire = seal(&mut server, ContentType::Alert, &[2, 48]);
        let mut s = make(wire);
        let mut out = Vec::new();
        let e = s.read_to_end(&mut out).unwrap_err();
        assert!(e.to_string().contains("48"), "{e}");
    }

    #[test]
    fn abrupt_disconnect_is_treated_as_eof() {
        // 对端不发 close_notify 直接断开，在 HTTP/1.1 `Connection: close`
        // 下非常常见，当正常结束处理而不是报错。
        let mut s = make(Vec::new());
        let mut out = Vec::new();
        assert_eq!(s.read_to_end(&mut out).unwrap(), 0);
    }

    #[test]
    fn post_handshake_records_are_ignored_not_fatal() {
        let key = [0x33u8; 16];
        let iv = [0x44u8; 12];
        let mut server = Keys::new(&key, &iv);
        // NewSessionTicket 之类的握手记录夹在应用数据之间。
        let mut wire = seal(&mut server, ContentType::Handshake, &[4, 0, 0, 1, 0]);
        wire.extend_from_slice(&seal(&mut server, ContentType::ApplicationData, b"payload"));
        let mut s = make(wire);
        let mut out = Vec::new();
        s.read_to_end(&mut out).unwrap();
        assert_eq!(out, b"payload");
    }

    #[test]
    fn writes_are_split_at_the_record_limit() {
        // 超过 2^14 的明文必须切分，否则对端直接断开。
        let mut s = make(Vec::new());
        let big = vec![0u8; record::MAX_PLAINTEXT + 5000];
        let n = s.write(&big).unwrap();
        assert_eq!(n, record::MAX_PLAINTEXT, "单次最多写一条记录的量");
        // write_all 会自动把剩下的补上
        s.write_all(&big[n..]).unwrap();
        assert!(s.inner.to_peer.len() > record::MAX_PLAINTEXT);
    }

    #[test]
    fn partial_reads_do_not_lose_data() {
        let key = [0x33u8; 16];
        let iv = [0x44u8; 12];
        let mut server = Keys::new(&key, &iv);
        let wire = seal(&mut server, ContentType::ApplicationData, b"abcdefghij");
        let mut s = make(wire);
        let mut buf = [0u8; 3];
        let mut got = Vec::new();
        loop {
            let n = s.read(&mut buf).unwrap();
            if n == 0 {
                break;
            }
            got.extend_from_slice(&buf[..n]);
        }
        assert_eq!(got, b"abcdefghij");
    }
}
