//! 端到端：用真实的 Linux x86-64 静态 ELF 跑模拟器。
//!
//! 需要现场编译 guest 的用例只在 **Linux 宿主**上跑（见 `can_build_linux_elf`），
//! 其他平台跳过并说明原因，不让环境差异把门禁搞成红的。
//! `busybox` 系列读仓库里自带的静态 Linux ELF，**任何平台都真跑**——
//! Windows CI 的端到端覆盖靠的就是这一组。

use std::path::{Path, PathBuf};
use std::process::Command;

/// 被测的模拟器可执行文件（cargo 会保证它已构建）。
const EMU: &str = env!("CARGO_BIN_EXE_wbox-linux");

fn repo_root() -> PathBuf {
    // crates/wbox-linux -> 仓库根
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .unwrap()
        .to_path_buf()
}

/// 能否在本机构建**Linux** x86-64 ELF。
///
/// 只看 `gcc` 存在是不够的：Windows runner 上的 gcc 产出的是 PE，不是 Linux
/// ELF，拿去喂模拟器只会得到"不是 ELF"。所以这些用例限定在 Linux 宿主上跑；
/// 其余平台跳过并说明原因，而不是假装通过、也不是红。
///
/// 注意这**不会**让 Windows CI 失去覆盖：`busybox` 系列用例读的是仓库里
/// 自带的静态 Linux ELF，任何平台都真跑；指令语义那 61 项也与工具链无关。
fn can_build_linux_elf() -> bool {
    if !cfg!(target_os = "linux") {
        eprintln!("跳过：本宿主的 gcc 不产出 Linux ELF（只在 Linux 宿主上构建 guest）");
        return false;
    }
    if !have("gcc") {
        eprintln!("跳过：环境里没有 gcc");
        return false;
    }
    true
}

fn have(tool: &str) -> bool {
    Command::new(tool)
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// 每个测试用独立的临时目录，避免并行跑时互相踩。
fn tmpdir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("wbox-linux-test-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

struct Out {
    code: i32,
    stdout: String,
    stderr: String,
}

/// 在模拟器里跑一个 guest 程序。
fn emulate(args: &[&str], env: &[(&str, &str)]) -> Out {
    let mut c = Command::new(EMU);
    c.args(args);
    for (k, v) in env {
        c.env(k, v);
    }
    let o = c.output().expect("启动 wbox-linux 失败");
    Out {
        code: o.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
    }
}

/// 断言辅助：失败时把 stderr 一起打出来，否则模拟器的诊断信息看不到。
fn assert_ok(o: &Out, want_code: i32, ctx: &str) {
    assert_eq!(
        o.code, want_code,
        "{ctx}\n--- stdout ---\n{}\n--- stderr ---\n{}",
        o.stdout, o.stderr
    );
}

#[test]
fn version_flag_works_without_a_guest() {
    let o = emulate(&["--version"], &[]);
    assert_ok(&o, 0, "--version 应成功");
    assert!(o.stdout.contains("wbox-linux"), "{}", o.stdout);
}

#[test]
fn missing_guest_reports_a_clear_error() {
    let o = emulate(&["/definitely/not/here"], &[]);
    assert_ne!(o.code, 0);
    assert!(
        o.stderr.contains("读取") && o.stderr.contains("/definitely/not/here"),
        "错误信息要指名找不到哪个文件：{}",
        o.stderr
    );
}

#[test]
fn non_elf_input_is_rejected_not_silently_run() {
    let d = tmpdir("nonelf");
    let f = d.join("junk");
    std::fs::write(&f, b"this is not an ELF at all\n").unwrap();
    let o = emulate(&[f.to_str().unwrap()], &[]);
    assert_ne!(o.code, 0);
    assert!(o.stderr.contains("ELF"), "{}", o.stderr);
}

#[test]
fn raw_syscall_asm_guest_writes_and_exits() {
    if !can_build_linux_elf() {
        return;
    }
    let d = tmpdir("asm");
    let src = d.join("hi.s");
    std::fs::write(
        &src,
        r#"
.globl _start
.section .rodata
msg: .ascii "hello from guest\n"
.set msglen, . - msg
.text
_start:
    mov $1, %eax
    mov $1, %edi
    lea msg(%rip), %rsi
    mov $msglen, %edx
    syscall
    mov $60, %eax
    mov $7, %edi
    syscall
"#,
    )
    .unwrap();
    let bin = d.join("hi");
    let st = Command::new("gcc")
        .args(["-nostdlib", "-static", "-o"])
        .arg(&bin)
        .arg(&src)
        .status()
        .unwrap();
    assert!(st.success(), "汇编 guest 编译失败");

    let o = emulate(&[bin.to_str().unwrap()], &[]);
    assert_ok(&o, 7, "exit(7) 的退出码要透传");
    assert_eq!(o.stdout, "hello from guest\n");
}

#[test]
fn static_libc_guest_runs_printf_malloc_and_string_functions() {
    if !can_build_linux_elf() {
        return;
    }
    let d = tmpdir("libc");
    let src = d.join("t.c");
    // 这些函数覆盖的是模拟器最容易出错的几处：printf 的可变参数与格式化、
    // malloc 要的 brk/mmap、strlen/strcmp 的 SSE2 实现、以及退出码。
    std::fs::write(
        &src,
        r#"
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
int main(int argc, char **argv) {
    printf("argc=%d\n", argc);
    char buf[64];
    strcpy(buf, "hello");
    strcat(buf, " world");
    printf("len=%zu buf=%s\n", strlen(buf), buf);
    int *p = malloc(4 * sizeof(int));
    for (int i = 0; i < 4; i++) p[i] = i * i;
    printf("sq=%d%d%d%d\n", p[0], p[1], p[2], p[3]);
    free(p);
    printf("cmp=%d\n", strcmp("abc", "abd") < 0 ? -1 : 1);
    long sum = 0;
    for (long i = 0; i < 100000; i++) sum += i;   /* 让循环/进位路径跑久一点 */
    printf("sum=%ld\n", sum);
    return 3;
}
"#,
    )
    .unwrap();
    let bin = d.join("t");
    let st = Command::new("gcc")
        .args(["-static", "-O1", "-o"])
        .arg(&bin)
        .arg(&src)
        .status()
        .unwrap();
    if !st.success() {
        eprintln!("跳过：这个环境没有静态 libc 可链接");
        return;
    }

    let o = emulate(&[bin.to_str().unwrap(), "x", "y"], &[]);
    assert_ok(&o, 3, "return 3 要变成退出码 3");
    assert!(o.stdout.contains("argc=3"), "{}", o.stdout);
    assert!(o.stdout.contains("len=11 buf=hello world"), "{}", o.stdout);
    assert!(o.stdout.contains("sq=0149"), "{}", o.stdout);
    assert!(o.stdout.contains("cmp=-1"), "{}", o.stdout);
    assert!(o.stdout.contains("sum=4999950000"), "{}", o.stdout);
}

#[test]
fn dynamically_linked_guest_runs_through_its_interpreter() {
    if !can_build_linux_elf() {
        return;
    }
    let d = tmpdir("dynamic");
    let src = d.join("dy.c");
    std::fs::write(
        &src,
        r#"
#include <stdio.h>
int main(void) { puts("dynamic ok"); return 0; }
"#,
    )
    .unwrap();
    let bin = d.join("dy");
    // 默认就是动态链接：这条路径要跑通 PT_INTERP（加载 ld.so）、
    // 动态链接器自身的重定位、以及 TLS 初始化——静态用例完全覆盖不到。
    let st = Command::new("gcc")
        .args(["-O1", "-o"])
        .arg(&bin)
        .arg(&src)
        .status()
        .unwrap();
    assert!(st.success(), "动态 guest 编译失败");

    let o = emulate(&[bin.to_str().unwrap()], &[]);
    assert_ok(&o, 0, "动态链接 guest");
    assert_eq!(o.stdout, "dynamic ok\n");
}

#[test]
fn file_io_roundtrip_through_the_guest() {
    if !can_build_linux_elf() {
        return;
    }
    let d = tmpdir("fileio");
    let src = d.join("io.c");
    std::fs::write(
        &src,
        r#"
#include <stdio.h>
#include <string.h>
int main(int argc, char **argv) {
    FILE *f = fopen(argv[1], "w");
    if (!f) { perror("fopen w"); return 1; }
    fputs("line one\nline two\n", f);
    fclose(f);
    f = fopen(argv[1], "r");
    if (!f) { perror("fopen r"); return 2; }
    char buf[128];
    int n = 0;
    while (fgets(buf, sizeof buf, f)) { printf("got:%s", buf); n++; }
    fclose(f);
    printf("lines=%d\n", n);
    return 0;
}
"#,
    )
    .unwrap();
    let bin = d.join("io");
    let st = Command::new("gcc")
        .args(["-static", "-O1", "-o"])
        .arg(&bin)
        .arg(&src)
        .status()
        .unwrap();
    if !st.success() {
        eprintln!("跳过：这个环境没有静态 libc 可链接");
        return;
    }
    let data = d.join("data.txt");
    let o = emulate(&[bin.to_str().unwrap(), data.to_str().unwrap()], &[]);
    assert_ok(&o, 0, "文件读写往返");
    assert!(o.stdout.contains("got:line one"), "{}", o.stdout);
    assert!(o.stdout.contains("lines=2"), "{}", o.stdout);
    // 落盘的内容宿主侧也要能读到
    assert_eq!(
        std::fs::read_to_string(&data).unwrap(),
        "line one\nline two\n"
    );
}

// ------------------------------------------------------------- busybox

fn busybox() -> Option<PathBuf> {
    let p = repo_root().join("busybox");
    if p.is_file() {
        Some(p)
    } else {
        eprintln!("跳过：仓库里没有 busybox");
        None
    }
}

/// 搭一个只含 busybox 的临时 rootfs，返回该目录。
///
/// busybox 系列用例一律走**容器语义**（`WBOX_PREFIX` 指向这个目录，
/// guest 用 `/busybox`），原因有两条：
///
/// 1. 这就是产品路径 —— `EmuBackend` 总会设前缀；
/// 2. **必须**这么做才能跨平台。busybox 靠 `basename(argv[0])` 选 applet，
///    而 basename 是按 `/` 切的。直接把宿主路径传进去，在 Windows 上
///    argv[0] 是 `D:\a\wbox\wbox\busybox`（没有 `/`），busybox 会把整串
///    当成 applet 名，报 "applet not found"——CI 上实测踩到过。
///    容器语义下 argv[0] 是 `/busybox`，两个平台都对。
///
/// 另外用临时目录而不是仓库根：guest 会在 cwd 下建文件，指向仓库根会污染
/// 工作区（这一点也实测踩到过）。
fn busybox_rootfs(tag: &str) -> Option<PathBuf> {
    let bb = busybox()?;
    let d = tmpdir(tag);
    std::fs::copy(&bb, d.join("busybox")).expect("复制 busybox 失败");
    Some(d)
}

/// 在 busybox rootfs 里跑一条 busybox 命令。
fn bb_run(root: &Path, args: &[&str], extra_env: &[(&str, &str)]) -> Out {
    let mut argv = vec!["/busybox"];
    argv.extend_from_slice(args);
    let mut env: Vec<(&str, &str)> = vec![("WBOX_PREFIX", root.to_str().unwrap())];
    env.extend_from_slice(extra_env);
    emulate(&argv, &env)
}

#[test]
fn busybox_basic_applets() {
    let Some(root) = busybox_rootfs("bbapplets") else { return };

    let o = bb_run(&root, &["echo", "hello", "busybox"], &[]);
    assert_ok(&o, 0, "busybox echo");
    assert_eq!(o.stdout, "hello busybox\n");

    // uname 的内容来自我们的 sys_uname，必须自洽
    let o = bb_run(&root, &["uname", "-a"], &[]);
    assert_ok(&o, 0, "busybox uname");
    assert!(o.stdout.contains("Linux"), "{}", o.stdout);
    assert!(o.stdout.contains("x86_64"), "{}", o.stdout);

    // 退出码：true/false 必须分得清
    assert_ok(&bb_run(&root, &["true"], &[]), 0, "busybox true");
    assert_ok(&bb_run(&root, &["false"], &[]), 1, "busybox false");
}

#[test]
fn busybox_reads_files_and_directories() {
    let Some(root) = busybox_rootfs("bbfs") else { return };
    std::fs::write(root.join("a.txt"), "alpha\n").unwrap();
    std::fs::write(root.join("b.txt"), "beta\n").unwrap();
    std::fs::create_dir(root.join("sub")).unwrap();

    // cat：走 openat/read/write
    let o = bb_run(&root, &["cat", "/a.txt"], &[]);
    assert_ok(&o, 0, "busybox cat");
    assert_eq!(o.stdout, "alpha\n");

    // ls：走 getdents64
    let o = bb_run(&root, &["ls", "/"], &[]);
    assert_ok(&o, 0, "busybox ls");
    for want in ["a.txt", "b.txt", "sub"] {
        assert!(o.stdout.contains(want), "ls 少了 {want}：{}", o.stdout);
    }

    // wc -c：走 stat/read，验证字节数正确
    let o = bb_run(&root, &["wc", "-c", "/a.txt"], &[]);
    assert_ok(&o, 0, "busybox wc");
    assert!(o.stdout.starts_with('6'), "{}", o.stdout);

    // 不存在的文件要报错并给非零退出码
    let o = bb_run(&root, &["cat", "/nope"], &[]);
    assert_ne!(o.code, 0, "读不存在的文件必须非零退出");
}

#[test]
fn busybox_sha256_matches_known_value() {
    let Some(root) = busybox_rootfs("bbsha") else { return };
    std::fs::write(root.join("x"), "abc").unwrap();
    let o = bb_run(&root, &["sha256sum", "/x"], &[]);
    assert_ok(&o, 0, "busybox sha256sum");
    // "abc" 的 SHA-256 是已知常量：算错一位就说明整数/移位路径有问题
    assert!(
        o.stdout
            .starts_with("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"),
        "sha256 不对，说明算术路径有 bug：{}",
        o.stdout
    );
}

#[test]
fn wbox_prefix_confines_the_guest_to_the_rootfs() {
    let Some(bb) = busybox() else { return };
    let d = tmpdir("prefix");
    // 搭一个最小 rootfs：/bin/busybox + /hello.txt
    std::fs::create_dir_all(d.join("bin")).unwrap();
    std::fs::copy(&bb, d.join("bin/busybox")).unwrap();
    std::fs::write(d.join("hello.txt"), "inside\n").unwrap();
    // rootfs **之外**放一个诱饵，guest 不应该读到它
    let outside = d.parent().unwrap().join("wbox-linux-canary.txt");
    std::fs::write(&outside, "SHOULD-NOT-BE-READABLE\n").unwrap();

    let prefix = d.to_str().unwrap();

    // guest 视角的绝对路径应解析到 rootfs 里
    let o = emulate(
        &["/bin/busybox", "cat", "/hello.txt"],
        &[("WBOX_PREFIX", prefix)],
    );
    assert_ok(&o, 0, "prefix 下读 rootfs 内文件");
    assert_eq!(o.stdout, "inside\n");

    // 用 ../ 往上爬不能逃出 prefix
    let escape = format!("/../{}", outside.file_name().unwrap().to_string_lossy());
    let o = emulate(
        &["/bin/busybox", "cat", &escape],
        &[("WBOX_PREFIX", prefix)],
    );
    assert_ne!(o.code, 0, "越过 prefix 的读取必须失败");
    assert!(
        !o.stdout.contains("SHOULD-NOT-BE-READABLE"),
        "guest 逃出了 rootfs！读到了 {}",
        outside.display()
    );

    // 更深的爬升同样不行
    let o = emulate(
        &["/bin/busybox", "cat", "/../../../../etc/hostname"],
        &[("WBOX_PREFIX", prefix)],
    );
    assert_ne!(o.code, 0, "多级 ../ 也必须被限制在 prefix 内");
}

#[test]
fn internal_control_env_is_not_leaked_to_the_guest() {
    let Some(root) = busybox_rootfs("bbenv") else { return };
    // WBOX_*/BLINK_* 是内部控制键，按 PRD §F7.2 不得透传给 guest
    let o = bb_run(
        &root,
        &["env"],
        &[("WBOX_SECRET_KNOB", "1"), ("BLINK_SECRET_KNOB", "1"), ("VISIBLE", "yes")],
    );
    assert_ok(&o, 0, "busybox env");
    assert!(o.stdout.contains("VISIBLE=yes"), "普通变量该透传：{}", o.stdout);
    assert!(
        !o.stdout.contains("WBOX_SECRET_KNOB") && !o.stdout.contains("BLINK_SECRET_KNOB"),
        "内部控制键泄漏给了 guest：{}",
        o.stdout
    );
}

#[test]
fn instruction_budget_env_stops_a_runaway_guest() {
    let Some(root) = busybox_rootfs("bbbudget") else { return };
    // 给一个极小的指令预算：guest 一定跑不完，必须被终止而不是挂住
    let o = bb_run(&root, &["echo", "hi"], &[("WBOX_MAX_INSNS", "1000")]);
    assert_ne!(o.code, 0, "预算耗尽应非零退出");
    assert!(
        o.stderr.contains("signal") || o.stderr.contains("fatal"),
        "应有明确诊断：{}",
        o.stderr
    );
}

#[test]
fn shebang_script_runs_through_its_interpreter() {
    let Some(root) = busybox_rootfs("shebang") else { return };
    // 用 busybox 的 echo applet 当解释器：#!/busybox echo
    // 解释器路径必须是 **guest** 路径，否则容器里找不到它。
    std::fs::write(root.join("s"), "#!/busybox echo\n").unwrap();
    let o = emulate(
        &["/s", "tail"],
        &[("WBOX_PREFIX", root.to_str().unwrap())],
    );
    assert_ok(&o, 0, "#! 脚本");
    // echo 会把 [echo] <脚本路径> tail 都打出来
    assert!(o.stdout.contains("tail"), "{}", o.stdout);
    assert!(o.stdout.contains("/s"), "{}", o.stdout);
}

// -------------------------------------------------- 进程族（fork/execve/wait4）

/// `bb_run` 的 shell 版：`/busybox sh -c '<script>'`。
fn bb_sh(root: &Path, script: &str) -> Out {
    bb_run(root, &["sh", "-c", script], &[])
}

#[test]
fn shell_and_and_sequences_fork_and_exec() {
    let Some(root) = busybox_rootfs("procseq") else { return };

    // `a && b` 的右边要 shell fork+exec 出一个新进程。这条正是 Windows
    // 产品用例 WP.3W 跑的形状（写进私有 rootfs 再读回来）。
    let o = bb_sh(&root, "echo private-ok > /w && /busybox cat /w");
    assert_ok(&o, 0, "&& 链");
    assert_eq!(o.stdout, "private-ok\n");

    // `;` 顺序执行，两条都要跑到
    let o = bb_sh(&root, "echo one; /busybox echo two");
    assert_ok(&o, 0, "; 序列");
    assert_eq!(o.stdout, "one\ntwo\n");

    // 退出码要从子进程一路传回来：`false && ...` 不执行右边，整体非 0
    let o = bb_sh(&root, "/busybox false && echo unreachable");
    assert_eq!(o.code, 1, "失败的 && 链应保留非零退出码：{}", o.stderr);
    assert_eq!(o.stdout, "");
}

#[test]
fn command_substitution_reads_child_output_through_a_pipe() {
    let Some(root) = busybox_rootfs("procsubst") else { return };
    // `$(...)`：父进程建管道、fork、子进程 exec 后写、父进程读到 **EOF**。
    // 读端必须在写端全部关闭后返回 0；返回 EAGAIN 会让 shell 无限自旋。
    let o = bb_sh(&root, "v=$(/busybox echo inner); echo got=$v");
    assert_ok(&o, 0, "命令替换");
    assert_eq!(o.stdout, "got=inner\n");
}

#[test]
fn pipeline_passes_data_between_two_processes() {
    let Some(root) = busybox_rootfs("procpipe") else { return };
    let o = bb_sh(&root, "/busybox echo abcd | /busybox wc -c");
    assert_ok(&o, 0, "管道");
    assert_eq!(o.stdout.trim(), "5"); // abcd + 换行
}

#[test]
fn loops_fork_repeatedly_without_leaking_the_budget() {
    let Some(root) = busybox_rootfs("procloop") else { return };
    // 同一层里连续 fork 很多次：不该撞上 fork 嵌套深度上限
    // （那个上限管的是 fork 里再 fork，不是顺序 fork）。
    let o = bb_sh(&root, "for i in 1 2 3 4 5; do /busybox echo n$i; done");
    assert_ok(&o, 0, "循环里反复 fork");
    assert_eq!(o.stdout, "n1\nn2\nn3\nn4\nn5\n");
}

#[test]
fn dev_null_and_zero_are_synthesised() {
    let Some(root) = busybox_rootfs("devnodes") else { return };

    // rootfs 里**没有** /dev 目录，这几个节点全靠我们合成。
    // `2>/dev/null` 是 shell 脚本里最常见的一句，缺了它脚本第一行就挂。
    let o = bb_sh(&root, "/busybox cat /nonexistent 2>/dev/null; echo rc=$?");
    assert_ok(&o, 0, "2>/dev/null");
    assert_eq!(o.stdout, "rc=1\n");

    // /dev/null 读出来是 EOF
    let o = bb_sh(&root, "/busybox wc -c < /dev/null");
    assert_ok(&o, 0, "读 /dev/null");
    assert_eq!(o.stdout.trim(), "0");

    // /dev/zero 读出来是 0 字节；用 dd 取固定长度，避免无限读
    let o = bb_sh(&root, "/busybox dd if=/dev/zero bs=1 count=8 2>/dev/null | /busybox wc -c");
    assert_ok(&o, 0, "读 /dev/zero");
    assert_eq!(o.stdout.trim(), "8");

    // test -c 要认得它是字符设备（走的是我们合成的 stat）
    let o = bb_sh(&root, "if [ -c /dev/null ]; then echo chardev; fi");
    assert_ok(&o, 0, "test -c /dev/null");
    assert_eq!(o.stdout, "chardev\n");
}

#[test]
fn proc_self_exe_points_at_the_running_image() {
    let Some(root) = busybox_rootfs("selfexe") else { return };
    // readlink /proc/self/exe 必须给**真的** guest 路径：回一条
    // "/proc/self/exe" 自身会让 guest 的 re-exec 无声失败。
    let o = bb_run(&root, &["readlink", "/proc/self/exe"], &[]);
    assert_ok(&o, 0, "readlink /proc/self/exe");
    assert_eq!(o.stdout.trim(), "/busybox");
}
