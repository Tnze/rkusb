//! SHA-256 —— 用 RustCrypto 的 `sha2` crate（crates.io 上使用最广的 SHA-256 实现），
//! 纯 Rust、零 C 依赖，Linux / Windows(MSVC/GNU) / macOS 均可直接编译链接。
//!
//! 历史：最初实现是 FFI 直链系统 libcrypto 的 `SHA256()`（原则："不自己手写哈希，
//! 裁判必须是独立实现"）。但 `#[link(name = "crypto")]` 在 Windows MSVC 下要找
//! OpenSSL 的 crypto.lib，裸 Windows 没有 → LNK1181 链接失败。改为 `sha2` crate：
//! 依然不是我手写的实现（RustCrypto 社区实现，crates.io 下载量最高的 sha256 库），
//! 且彻底摆脱平台 C 库依赖。
//!
//! 裁判独立性由两层测试保证：
//!   1. `known_answer_vectors`：FIPS 180-2 / RFC 6234 公开标准向量（第三方数值）；
//!   2. `agrees_with_system_sha256sum`：与系统 `sha256sum` 独立进程交叉验证
//!      （没有该命令的系统——例如 Windows——自动跳过）。

use sha2::{Digest, Sha256};

/// 摘要长度（字节）
pub const SHA256_LEN: usize = 32;

/// 计算 SHA-256。
pub fn sha256(data: &[u8]) -> [u8; SHA256_LEN] {
    Sha256::digest(data).into()
}

/// 十六进制小写字符串，便于和 `sha256sum` 的输出对拍。
pub fn hex(b: &[u8]) -> String {
    let mut s = String::with_capacity(b.len() * 2);
    for x in b {
        s.push_str(&format!("{x:02x}"));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    /// FIPS 180-2 / RFC 6234 里的标准向量。这三条不是我自己算的，
    /// 是公开测试向量，用来确认算的确实是 SHA-256 而不是别的什么。
    #[test]
    fn known_answer_vectors() {
        assert_eq!(
            hex(&sha256(b"")),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            hex(&sha256(b"abc")),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        // 448 bit 的那条（56 字节，正好触发一次额外的填充块）
        assert_eq!(
            hex(&sha256(
                b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"
            )),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
    }

    /// 和系统 `sha256sum` 交叉验证：独立实现、独立进程。
    /// 这条能抓住"实现本身有错"；没有 sha256sum 的平台（Windows）自动跳过。
    #[test]
    fn agrees_with_system_sha256sum() {
        use std::io::Write;
        use std::process::{Command, Stdio};

        // 覆盖空输入、短输入、以及跨多个压缩块的长输入
        let cases: Vec<Vec<u8>> = vec![
            Vec::new(),
            b"abc".to_vec(),
            (0..55u8).collect(),
            (0..56u8).collect(),
            (0..64u8).collect(),
            (0..1000u32).map(|i| (i % 251) as u8).collect(),
        ];

        for data in cases {
            let mut child = match Command::new("sha256sum")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .spawn()
            {
                Ok(c) => c,
                Err(_) => {
                    eprintln!("skip: 本系统没有 sha256sum");
                    return;
                }
            };
            child
                .stdin
                .as_mut()
                .unwrap()
                .write_all(&data)
                .unwrap();
            let out = child.wait_with_output().unwrap();
            let want = String::from_utf8(out.stdout).unwrap();
            let want = want.split_whitespace().next().unwrap().to_string();
            assert_eq!(hex(&sha256(&data)), want, "len={}", data.len());
        }
    }
}
