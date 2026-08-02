//! 指令语义测试。
//!
//! 机器码是手工汇编的字节序列（注释里写出对应的 AT&T 汇编），所以这套测试
//! **不依赖任何工具链**，在 Windows CI 上和 Linux 上跑的是同一批断言。
//! 端到端跑真 ELF 的测试在 `guest_elf.rs`，那个才需要 gcc。

use wbox_linux::core::CoreException;
use wbox_linux::cpu::*;
use wbox_linux::exec::load_code;
use wbox_linux::machine::{Exception, Machine};
use wbox_linux::mem::{PROT_READ, PROT_WRITE};
use wbox_linux::syscall::Os;

const CODE: u64 = 0x1_0000;
const STACK: u64 = 0x2_0000;
const DATA: u64 = 0x3_0000;

/// 装一段代码，另外给出栈和一页可读写数据区（DATA）。
fn mach(code: &[u8]) -> Machine {
    let mut m = Machine::new(Os::new());
    load_code(&mut m.core, CODE, code);
    m.mem.map(STACK, 0x4000, PROT_READ | PROT_WRITE);
    m.mem.map(DATA, 0x1000, PROT_READ | PROT_WRITE);
    m.cpu.set_rsp(STACK + 0x2000);
    m
}

/// 执行 n 条指令，任何异常都当失败。
fn steps(m: &mut Machine, n: usize) {
    for i in 0..n {
        if let Err(e) = m.step() {
            panic!("第 {i} 条指令出错：{e}");
        }
    }
}

#[test]
fn mov_imm_and_add() {
    // mov $0x11,%eax ; mov $0x22,%ebx ; add %ebx,%eax
    let mut m = mach(&[0xb8, 0x11, 0, 0, 0, 0xbb, 0x22, 0, 0, 0, 0x01, 0xd8]);
    steps(&mut m, 3);
    assert_eq!(m.cpu.regs[RAX], 0x33);
}

#[test]
fn public_core_step_returns_syscall_trap_without_linux_dispatch() {
    // mov $39,%eax ; syscall: the core reports the trap and does not mutate
    // Linux personality state or dispatch the syscall itself.
    let mut m = mach(&[0xb8, 39, 0, 0, 0, 0x0f, 0x05]);
    m.core
        .step()
        .expect("mov must execute through the core API");
    match m.core.step() {
        Err(CoreException::Syscall { ret_rip }) => assert_eq!(ret_rip, CODE + 7),
        other => panic!("expected core syscall trap, got {other:?}"),
    }
}

#[test]
fn movabs_loads_full_64_bit_immediate() {
    // movabs $0x1122334455667788,%rax
    let mut m = mach(&[0x48, 0xb8, 0x88, 0x77, 0x66, 0x55, 0x44, 0x33, 0x22, 0x11]);
    steps(&mut m, 1);
    assert_eq!(m.cpu.regs[RAX], 0x1122_3344_5566_7788);
}

#[test]
fn write_to_32bit_register_clears_upper_half() {
    // movabs $-1,%rax ; mov $1,%eax
    let mut m = mach(&[
        0x48, 0xb8, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xb8, 0x01, 0, 0, 0,
    ]);
    steps(&mut m, 2);
    assert_eq!(m.cpu.regs[RAX], 1, "32 位写必须零扩展");
}

#[test]
fn rex_b_selects_extended_registers() {
    // mov $0x5,%r8d  (41 b8 05 ...)
    let mut m = mach(&[0x41, 0xb8, 0x05, 0, 0, 0]);
    steps(&mut m, 1);
    assert_eq!(m.cpu.regs[R8], 5);
}

#[test]
fn byte_high_registers_without_rex() {
    // mov $0x1234,%eax ; mov %ah,%cl   (88 e1)
    let mut m = mach(&[0xb8, 0x34, 0x12, 0, 0, 0x88, 0xe1]);
    steps(&mut m, 2);
    assert_eq!(m.cpu.regs[RCX] & 0xff, 0x12, "%ah 是 bits 8..15");
}

#[test]
fn cmp_and_conditional_jump_taken() {
    // mov $5,%eax ; cmp $5,%eax ; je +2 ; mov $9,%eax ; (落点) nop
    let mut m = mach(&[
        0xb8, 0x05, 0, 0, 0, // mov $5,%eax
        0x83, 0xf8, 0x05, // cmp $5,%eax
        0x74, 0x05, // je .+5
        0xb8, 0x09, 0, 0, 0, // mov $9,%eax（应被跳过）
        0x90,
    ]);
    steps(&mut m, 4); // mov, cmp, je, nop
    assert_eq!(m.cpu.regs[RAX], 5, "je 应该跳过 mov $9");
    assert!(m.cpu.flags.zf);
}

#[test]
fn signed_comparison_uses_sf_and_of() {
    // mov $-1,%eax ; cmp $1,%eax ; jl +5 ; mov $9,%eax
    let mut m = mach(&[
        0xb8, 0xff, 0xff, 0xff, 0xff, 0x83, 0xf8, 0x01, 0x7c, 0x05, 0xb8, 0x09, 0, 0, 0, 0x90,
    ]);
    steps(&mut m, 4);
    assert_eq!(m.cpu.regs[RAX], 0xffff_ffff, "-1 < 1 应跳转");
}

#[test]
fn push_pop_call_ret() {
    // push $0x77 ; pop %rax ; call +0 (下一条) ; pop %rbx
    let mut m = mach(&[
        0x6a, 0x77, // push $0x77
        0x58, // pop %rax
        0xe8, 0x00, 0x00, 0x00, 0x00, // call 下一条指令
        0x5b, // pop %rbx  <- call 的返回地址
    ]);
    let sp0 = m.cpu.rsp();
    steps(&mut m, 2);
    assert_eq!(m.cpu.regs[RAX], 0x77);
    assert_eq!(m.cpu.rsp(), sp0, "push+pop 后栈指针复位");
    steps(&mut m, 2);
    // call 压的返回地址就是 call 之后那条指令
    assert_eq!(m.cpu.regs[RBX], CODE + 8);
    assert_eq!(m.cpu.rsp(), sp0);
}

#[test]
fn ret_jumps_to_pushed_address() {
    // mov $CODE+16,%eax ; push %rax ; ret ; ... ; (CODE+16) mov $0x42,%ebx
    let mut code = vec![0xb8];
    code.extend_from_slice(&((CODE + 16) as u32).to_le_bytes());
    code.extend_from_slice(&[0x50, 0xc3]); // push %rax ; ret
    while code.len() < 16 {
        code.push(0x90);
    }
    code.extend_from_slice(&[0xbb, 0x42, 0, 0, 0]); // mov $0x42,%ebx
    let mut m = mach(&code);
    steps(&mut m, 4);
    assert_eq!(m.cpu.rip, CODE + 21);
    assert_eq!(m.cpu.regs[RBX], 0x42);
}

#[test]
fn lea_with_sib_scale_and_index() {
    // mov $0x10,%rbx ; mov $0x3,%rcx ; lea 0x8(%rbx,%rcx,4),%rax
    let mut m = mach(&[
        0x48, 0xc7, 0xc3, 0x10, 0, 0, 0, // mov $0x10,%rbx
        0x48, 0xc7, 0xc1, 0x03, 0, 0, 0, // mov $0x3,%rcx
        0x48, 0x8d, 0x44, 0x8b, 0x08, // lea 0x8(%rbx,%rcx,4),%rax
    ]);
    steps(&mut m, 3);
    assert_eq!(m.cpu.regs[RAX], 0x10 + 3 * 4 + 8);
}

#[test]
fn rip_relative_addressing_is_relative_to_next_insn() {
    // lea 0x10(%rip),%rax  —— 指令 7 字节，所以 rax = CODE+7+0x10
    let mut m = mach(&[0x48, 0x8d, 0x05, 0x10, 0x00, 0x00, 0x00]);
    steps(&mut m, 1);
    assert_eq!(m.cpu.regs[RAX], CODE + 7 + 0x10);
}

#[test]
fn rip_relative_with_immediate_accounts_for_immediate_bytes() {
    // movl $0x5,disp(%rip) —— 关键点：位移基准是**整条指令之后**，
    // 包含那 4 字节立即数在内。指令共 10 字节，所以取
    // disp = DATA - (CODE + 10) 才能正好命中 DATA。
    // 如果实现错把基准取在立即数之前，写入会落到 DATA-4，断言就会失败。
    // 目标取 DATA+8（而不是 DATA），这样"少算 4 字节"的错误结果 DATA+4
    // 也在已映射的页内，可以直接断言它没被写。
    let target = DATA + 8;
    let disp = (target - (CODE + 10)) as u32;
    let mut code = vec![0xc7, 0x05];
    code.extend_from_slice(&disp.to_le_bytes());
    code.extend_from_slice(&5u32.to_le_bytes());
    let mut m = mach(&code);
    steps(&mut m, 1);
    assert_eq!(m.mem.read_u32(target).unwrap(), 5);
    assert_eq!(m.mem.read_u32(target - 4).unwrap(), 0, "不该差 4 字节");
}

#[test]
fn memory_load_store_roundtrip() {
    // mov $DATA,%rbx ; movq $0x1234,(%rbx) ; mov (%rbx),%rax
    let mut code = vec![0x48, 0xbb];
    code.extend_from_slice(&DATA.to_le_bytes()); // movabs $DATA,%rbx
    code.extend_from_slice(&[0x48, 0xc7, 0x03, 0x34, 0x12, 0x00, 0x00]); // movq $0x1234,(%rbx)
    code.extend_from_slice(&[0x48, 0x8b, 0x03]); // mov (%rbx),%rax
    let mut m = mach(&code);
    steps(&mut m, 3);
    assert_eq!(m.cpu.regs[RAX], 0x1234);
}

#[test]
fn movzx_and_movsx() {
    // mov $0xff,%eax ; movzbl %al,%ebx ; movsbl %al,%ecx
    let mut m = mach(&[
        0xb8, 0xff, 0, 0, 0, 0x0f, 0xb6, 0xd8, // movzbl %al,%ebx
        0x0f, 0xbe, 0xc8, // movsbl %al,%ecx
    ]);
    steps(&mut m, 3);
    assert_eq!(m.cpu.regs[RBX], 0xff);
    assert_eq!(m.cpu.regs[RCX], 0xffff_ffff, "movsx 要符号扩展");
}

#[test]
fn movsxd_widens_32_to_64() {
    // mov $-2,%eax ; movslq %eax,%rbx
    let mut m = mach(&[0xb8, 0xfe, 0xff, 0xff, 0xff, 0x48, 0x63, 0xd8]);
    steps(&mut m, 2);
    assert_eq!(m.cpu.regs[RBX], u64::MAX - 1);
}

#[test]
fn imul_and_idiv() {
    // mov $7,%eax ; mov $6,%ecx ; imul %ecx,%eax
    let mut m = mach(&[0xb8, 0x07, 0, 0, 0, 0xb9, 0x06, 0, 0, 0, 0x0f, 0xaf, 0xc1]);
    steps(&mut m, 3);
    assert_eq!(m.cpu.regs[RAX], 42);

    // mov $100,%eax ; cltd ; mov $7,%ecx ; idivl %ecx
    let mut m = mach(&[0xb8, 0x64, 0, 0, 0, 0x99, 0xb9, 0x07, 0, 0, 0, 0xf7, 0xf9]);
    steps(&mut m, 4);
    assert_eq!(m.cpu.regs[RAX], 14, "商");
    assert_eq!(m.cpu.regs[RDX], 2, "余数");
}

#[test]
fn divide_by_zero_raises_divide_error() {
    // mov $1,%eax ; xor %edx,%edx ; xor %ecx,%ecx ; divl %ecx
    let mut m = mach(&[0xb8, 0x01, 0, 0, 0, 0x31, 0xd2, 0x31, 0xc9, 0xf7, 0xf1]);
    steps(&mut m, 3);
    match m.step() {
        Err(Exception::DivideError { .. }) => {}
        other => panic!("除零应报 #DE，实际 {other:?}"),
    }
}

#[test]
fn shifts_and_flags() {
    // mov $1,%eax ; shl $4,%eax ; shr $2,%eax ; sar $1,%eax
    let mut m = mach(&[
        0xb8, 0x01, 0, 0, 0, 0xc1, 0xe0, 0x04, 0xc1, 0xe8, 0x02, 0xd1, 0xf8,
    ]);
    steps(&mut m, 4);
    assert_eq!(m.cpu.regs[RAX], 2);
}

#[test]
fn shift_by_cl() {
    // mov $1,%eax ; mov $5,%ecx ; shl %cl,%eax
    let mut m = mach(&[0xb8, 0x01, 0, 0, 0, 0xb9, 0x05, 0, 0, 0, 0xd3, 0xe0]);
    steps(&mut m, 3);
    assert_eq!(m.cpu.regs[RAX], 32);
}

#[test]
fn setcc_writes_byte_only() {
    // movabs $-1,%rax ; cmp %eax,%eax ; sete %al
    let mut m = mach(&[
        0x48, 0xb8, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x39, 0xc0, 0x0f, 0x94, 0xc0,
    ]);
    steps(&mut m, 3);
    assert_eq!(m.cpu.regs[RAX], 0xffff_ffff_ffff_ff01, "sete 只改低字节");
}

#[test]
fn cmov_respects_condition() {
    // mov $1,%eax ; mov $2,%ebx ; cmp %eax,%eax ; cmove %ebx,%eax
    let mut m = mach(&[
        0xb8, 0x01, 0, 0, 0, 0xbb, 0x02, 0, 0, 0, 0x39, 0xc0, 0x0f, 0x44, 0xc3,
    ]);
    steps(&mut m, 4);
    assert_eq!(m.cpu.regs[RAX], 2, "ZF 置位时 cmove 应搬运");

    // 条件不成立：cmovne 不搬
    let mut m = mach(&[
        0xb8, 0x01, 0, 0, 0, 0xbb, 0x02, 0, 0, 0, 0x39, 0xc0, 0x0f, 0x45, 0xc3,
    ]);
    steps(&mut m, 4);
    assert_eq!(m.cpu.regs[RAX], 1);
}

#[test]
fn rep_movsb_copies_buffer() {
    // 源在 DATA，目的在 DATA+0x100，拷 8 字节
    let mut code = vec![0x48, 0xbe];
    code.extend_from_slice(&DATA.to_le_bytes()); // movabs $DATA,%rsi
    code.extend_from_slice(&[0x48, 0xbf]);
    code.extend_from_slice(&(DATA + 0x100).to_le_bytes()); // movabs $DATA+0x100,%rdi
    code.extend_from_slice(&[0x48, 0xc7, 0xc1, 0x08, 0, 0, 0]); // mov $8,%rcx
    code.extend_from_slice(&[0xf3, 0xa4]); // rep movsb
    let mut m = mach(&code);
    m.mem.write(DATA, b"abcdefgh").unwrap();
    // rep 每 step 走一轮，8 轮 + 前 3 条指令
    steps(&mut m, 3 + 8);
    let mut out = [0u8; 8];
    m.mem.read(DATA + 0x100, &mut out).unwrap();
    assert_eq!(&out, b"abcdefgh");
    assert_eq!(m.cpu.regs[RCX], 0, "rcx 应归零");
}

#[test]
fn rep_stosb_fills_and_df_reverses_direction() {
    let mut code = vec![0x48, 0xbf];
    code.extend_from_slice(&(DATA + 0x10).to_le_bytes()); // movabs $DATA+0x10,%rdi
    code.extend_from_slice(&[0xb0, 0x5a]); // mov $0x5a,%al
    code.extend_from_slice(&[0x48, 0xc7, 0xc1, 0x04, 0, 0, 0]); // mov $4,%rcx
    code.extend_from_slice(&[0xfd]); // std（反向）
    code.extend_from_slice(&[0xf3, 0xaa]); // rep stosb
    let mut m = mach(&code);
    steps(&mut m, 4 + 4);
    // 反向填充：从 DATA+0x10 往下写 4 字节
    for a in (DATA + 0x0d)..=(DATA + 0x10) {
        assert_eq!(m.mem.read_u8(a).unwrap(), 0x5a, "地址 {a:#x}");
    }
    assert_eq!(m.mem.read_u8(DATA + 0x11).unwrap(), 0, "不该越过起点向上写");
}

#[test]
fn repne_scasb_stops_on_match() {
    // 在 "abcd\0" 里找 'c'
    let mut code = vec![0x48, 0xbf];
    code.extend_from_slice(&DATA.to_le_bytes());
    code.extend_from_slice(&[0xb0, 0x63]); // mov $'c',%al
    code.extend_from_slice(&[0x48, 0xc7, 0xc1, 0x08, 0, 0, 0]); // mov $8,%rcx
    code.extend_from_slice(&[0xf2, 0xae]); // repne scasb
    let mut m = mach(&code);
    m.mem.write(DATA, b"abcd\0\0\0\0").unwrap();
    // 3 条准备指令 + 3 轮（a,b,c）后 ZF 置位停下
    steps(&mut m, 3 + 3);
    assert!(m.cpu.flags.zf, "找到匹配时 ZF 置位");
    assert_eq!(m.cpu.regs[RDI], DATA + 3, "rdi 停在匹配字节之后");
}

#[test]
fn xchg_swaps_operands() {
    // mov $1,%eax ; mov $2,%ebx ; xchg %ebx,%eax
    let mut m = mach(&[0xb8, 0x01, 0, 0, 0, 0xbb, 0x02, 0, 0, 0, 0x87, 0xd8]);
    steps(&mut m, 3);
    assert_eq!((m.cpu.regs[RAX], m.cpu.regs[RBX]), (2, 1));
}

#[test]
fn cmpxchg_success_and_failure() {
    // 成功：内存值 == rax 时写入 rbx
    let mut code = vec![0x48, 0xb9];
    code.extend_from_slice(&DATA.to_le_bytes()); // movabs $DATA,%rcx
    code.extend_from_slice(&[0xb8, 0x05, 0, 0, 0]); // mov $5,%eax（期望值）
    code.extend_from_slice(&[0xbb, 0x09, 0, 0, 0]); // mov $9,%ebx（新值）
    code.extend_from_slice(&[0x0f, 0xb1, 0x19]); // cmpxchg %ebx,(%rcx)
    let mut m = mach(&code);
    m.mem.write_u32(DATA, 5).unwrap();
    steps(&mut m, 4);
    assert!(m.cpu.flags.zf);
    assert_eq!(m.mem.read_u32(DATA).unwrap(), 9);

    // 失败：内存值 != rax 时把内存值装回 rax，且不写内存
    let mut m = mach(&code);
    m.mem.write_u32(DATA, 7).unwrap();
    steps(&mut m, 4);
    assert!(!m.cpu.flags.zf);
    assert_eq!(m.mem.read_u32(DATA).unwrap(), 7, "失败时不得写入");
    assert_eq!(m.cpu.regs[RAX], 7, "失败时 rax 装载当前值");
}

#[test]
fn xadd_returns_old_and_stores_sum() {
    // mov $5,%eax ; mov $3,%ebx ; xadd %ebx,%eax
    let mut m = mach(&[0xb8, 0x05, 0, 0, 0, 0xbb, 0x03, 0, 0, 0, 0x0f, 0xc1, 0xd8]);
    steps(&mut m, 3);
    assert_eq!(m.cpu.regs[RAX], 8, "目标 = 和");
    assert_eq!(m.cpu.regs[RBX], 5, "源 = 旧的目标值");
}

#[test]
fn bswap_reverses_bytes() {
    // mov $0x11223344,%eax ; bswap %eax
    let mut m = mach(&[0xb8, 0x44, 0x33, 0x22, 0x11, 0x0f, 0xc8]);
    steps(&mut m, 2);
    assert_eq!(m.cpu.regs[RAX], 0x4433_2211);
}

#[test]
fn bit_test_and_set() {
    // mov $0,%eax ; bts $3,%eax ; bt $3,%eax
    let mut m = mach(&[
        0xb8, 0, 0, 0, 0, 0x0f, 0xba, 0xe8, 0x03, 0x0f, 0xba, 0xe0, 0x03,
    ]);
    steps(&mut m, 2);
    assert_eq!(m.cpu.regs[RAX], 8);
    steps(&mut m, 1);
    assert!(m.cpu.flags.cf, "bt 把该位放进 CF");
}

#[test]
fn bsf_bsr_and_zero_source() {
    // mov $0x100,%eax ; bsf %eax,%ebx ; bsr %eax,%ecx
    let mut m = mach(&[0xb8, 0x00, 0x01, 0, 0, 0x0f, 0xbc, 0xd8, 0x0f, 0xbd, 0xc8]);
    steps(&mut m, 3);
    assert_eq!(m.cpu.regs[RBX], 8);
    assert_eq!(m.cpu.regs[RCX], 8);

    // 源为 0 时 ZF 置位
    let mut m = mach(&[0x31, 0xc0, 0x0f, 0xbc, 0xd8]);
    steps(&mut m, 2);
    assert!(m.cpu.flags.zf);
}

#[test]
fn leave_restores_stack_from_rbp() {
    // mov %rsp,%rbp 之后 sub ，再 leave 应把 rsp 还原
    let mut m = mach(&[
        0x55, // push %rbp
        0x48, 0x89, 0xe5, // mov %rsp,%rbp
        0x48, 0x83, 0xec, 0x20, // sub $0x20,%rsp
        0xc9, // leave
    ]);
    let sp0 = m.cpu.rsp();
    steps(&mut m, 4);
    assert_eq!(m.cpu.rsp(), sp0, "leave 之后栈应完全复位");
}

#[test]
fn pushf_popf_roundtrip() {
    // stc ; pushfq ; popq %rax ; test CF 位
    let mut m = mach(&[0xf9, 0x9c, 0x58]);
    steps(&mut m, 3);
    assert_ne!(m.cpu.regs[RAX] & 1, 0, "CF 应出现在 RFLAGS bit0");
}

// -------------------------------------------------------------- SSE

#[test]
fn pxor_zeroes_register() {
    // pxor %xmm0,%xmm0  (66 0f ef c0)
    let mut m = mach(&[0x66, 0x0f, 0xef, 0xc0]);
    m.cpu.xmm[0] = [0xff; 16];
    steps(&mut m, 1);
    assert_eq!(m.cpu.xmm[0], [0u8; 16]);
}

#[test]
fn movdqu_load_and_store() {
    // movdqu (%rax),%xmm0 ; movdqu %xmm0,0x20(%rax)
    let mut code = vec![0x48, 0xb8];
    code.extend_from_slice(&DATA.to_le_bytes());
    code.extend_from_slice(&[0xf3, 0x0f, 0x6f, 0x00]); // movdqu (%rax),%xmm0
    code.extend_from_slice(&[0xf3, 0x0f, 0x7f, 0x40, 0x20]); // movdqu %xmm0,0x20(%rax)
    let mut m = mach(&code);
    let src: [u8; 16] = *b"0123456789abcdef";
    m.mem.write(DATA, &src).unwrap();
    steps(&mut m, 3);
    assert_eq!(m.cpu.xmm[0], src);
    let mut out = [0u8; 16];
    m.mem.read(DATA + 0x20, &mut out).unwrap();
    assert_eq!(out, src);
}

#[test]
fn pcmpeqb_plus_pmovmskb_is_the_strlen_primitive() {
    // 这两条组合起来就是 SSE2 strlen/memchr 的核心：比较 16 字节，
    // 把结果变成通用寄存器里的位图。
    // pxor %xmm1,%xmm1 ; movdqu (%rax),%xmm0 ; pcmpeqb %xmm1,%xmm0 ; pmovmskb %xmm0,%ecx
    let mut code = vec![0x48, 0xb8];
    code.extend_from_slice(&DATA.to_le_bytes());
    code.extend_from_slice(&[0x66, 0x0f, 0xef, 0xc9]); // pxor %xmm1,%xmm1
    code.extend_from_slice(&[0xf3, 0x0f, 0x6f, 0x00]); // movdqu (%rax),%xmm0
    code.extend_from_slice(&[0x66, 0x0f, 0x74, 0xc1]); // pcmpeqb %xmm1,%xmm0
    code.extend_from_slice(&[0x66, 0x0f, 0xd7, 0xc8]); // pmovmskb %xmm0,%ecx
    let mut m = mach(&code);
    // "abc" + NUL，其余也是 NUL：位 3..15 应为 1
    m.mem.write(DATA, b"abc\0\0\0\0\0\0\0\0\0\0\0\0\0").unwrap();
    steps(&mut m, 5);
    assert_eq!(m.cpu.regs[RCX], 0xfff8, "只有非零的前 3 字节对应位是 0");
    // strlen 就是 tzcnt(mask)
    assert_eq!((m.cpu.regs[RCX] as u16).trailing_zeros(), 3);
}

#[test]
fn punpcklqdq_takes_low_halves() {
    // 回归测试：0x6c 编码大于 0x68，但取的是**低**半。
    // 这个 bug 曾让 glibc 的 __tls_init_tp 拿到 0 指针而崩在很远的地方。
    // movq %rax,%xmm0 ; punpcklqdq %xmm0,%xmm0
    let mut code = vec![0x48, 0xb8];
    code.extend_from_slice(&0x1122_3344_5566_7788u64.to_le_bytes());
    code.extend_from_slice(&[0x66, 0x48, 0x0f, 0x6e, 0xc0]); // movq %rax,%xmm0
    code.extend_from_slice(&[0x66, 0x0f, 0x6c, 0xc0]); // punpcklqdq %xmm0,%xmm0
    let mut m = mach(&code);
    steps(&mut m, 3);
    let v = m.cpu.xmm[0];
    assert_eq!(&v[0..8], &0x1122_3344_5566_7788u64.to_le_bytes());
    assert_eq!(&v[8..16], &0x1122_3344_5566_7788u64.to_le_bytes());
}

#[test]
fn punpckhqdq_takes_high_halves() {
    // movdqu (%rax),%xmm0 ; punpckhqdq %xmm0,%xmm0
    let mut code = vec![0x48, 0xb8];
    code.extend_from_slice(&DATA.to_le_bytes());
    code.extend_from_slice(&[0xf3, 0x0f, 0x6f, 0x00]);
    code.extend_from_slice(&[0x66, 0x0f, 0x6d, 0xc0]);
    let mut m = mach(&code);
    m.mem.write(DATA, b"LOWLOWLOHIGHHIGH").unwrap();
    steps(&mut m, 3);
    assert_eq!(&m.cpu.xmm[0][0..8], b"HIGHHIGH");
    assert_eq!(&m.cpu.xmm[0][8..16], b"HIGHHIGH");
}

#[test]
fn punpcklbw_interleaves_bytes() {
    let mut code = vec![0x48, 0xb8];
    code.extend_from_slice(&DATA.to_le_bytes());
    code.extend_from_slice(&[0xf3, 0x0f, 0x6f, 0x00]); // movdqu (%rax),%xmm0
    code.extend_from_slice(&[0x66, 0x0f, 0x60, 0xc0]); // punpcklbw %xmm0,%xmm0
    let mut m = mach(&code);
    m.mem.write(DATA, b"AB..............").unwrap();
    steps(&mut m, 3);
    assert_eq!(&m.cpu.xmm[0][0..4], b"AABB");
}

#[test]
fn pminub_finds_per_byte_minimum() {
    // pminub 是 strlen 里合并多个比较结果的常用手段
    let mut m = mach(&[0x66, 0x0f, 0xda, 0xc1]); // pminub %xmm1,%xmm0
    m.cpu.xmm[0] = [5; 16];
    m.cpu.xmm[1] = [3; 16];
    steps(&mut m, 1);
    assert_eq!(m.cpu.xmm[0], [3; 16]);
}

#[test]
fn pshufd_permutes_dwords() {
    // pshufd $0x00,%xmm0,%xmm0 —— 把 lane0 广播到全部 4 个 lane
    let mut m = mach(&[0x66, 0x0f, 0x70, 0xc0, 0x00]);
    m.cpu.xmm[0][0..4].copy_from_slice(&0xdeadbeefu32.to_le_bytes());
    steps(&mut m, 1);
    for i in 0..4 {
        assert_eq!(
            &m.cpu.xmm[0][i * 4..i * 4 + 4],
            &0xdeadbeefu32.to_le_bytes(),
            "lane {i}"
        );
    }
}

#[test]
fn pslldq_psrldq_shift_whole_register_by_bytes() {
    // pslldq $4,%xmm0
    let mut m = mach(&[0x66, 0x0f, 0x73, 0xf8, 0x04]);
    m.cpu.xmm[0] = *b"0123456789abcdef";
    steps(&mut m, 1);
    assert_eq!(&m.cpu.xmm[0][0..4], &[0, 0, 0, 0]);
    assert_eq!(&m.cpu.xmm[0][4..8], b"0123");

    // psrldq $4,%xmm0
    let mut m = mach(&[0x66, 0x0f, 0x73, 0xd8, 0x04]);
    m.cpu.xmm[0] = *b"0123456789abcdef";
    steps(&mut m, 1);
    assert_eq!(&m.cpu.xmm[0][0..4], b"4567");
    assert_eq!(&m.cpu.xmm[0][12..16], &[0, 0, 0, 0]);
}

#[test]
fn movd_movq_between_gpr_and_xmm() {
    // movq %rax,%xmm0 ; movq %xmm0,%rbx
    let mut code = vec![0x48, 0xb8];
    code.extend_from_slice(&0xcafe_babe_dead_beefu64.to_le_bytes());
    code.extend_from_slice(&[0x66, 0x48, 0x0f, 0x6e, 0xc0]); // movq %rax,%xmm0
    code.extend_from_slice(&[0x66, 0x48, 0x0f, 0x7e, 0xc3]); // movq %xmm0,%rbx
    let mut m = mach(&code);
    steps(&mut m, 3);
    assert_eq!(m.cpu.regs[RBX], 0xcafe_babe_dead_beef);
}

// -------------------------------------------------------- syscall / 异常

#[test]
fn syscall_write_and_exit() {
    // mov $1,%eax(write) ; mov $1,%edi ; lea msg,%rsi ; mov $2,%edx ; syscall
    // 然后 mov $60,%eax ; mov $5,%edi ; syscall
    let mut code = vec![
        0xb8, 0x01, 0, 0, 0, // mov $1,%eax
        0xbf, 0x01, 0, 0, 0, // mov $1,%edi
    ];
    code.extend_from_slice(&[0x48, 0xbe]);
    code.extend_from_slice(&DATA.to_le_bytes()); // movabs $DATA,%rsi
    code.extend_from_slice(&[0xba, 0x02, 0, 0, 0]); // mov $2,%edx
    code.extend_from_slice(&[0x0f, 0x05]); // syscall
    code.extend_from_slice(&[0xb8, 0x3c, 0, 0, 0]); // mov $60,%eax
    code.extend_from_slice(&[0xbf, 0x05, 0, 0, 0]); // mov $5,%edi
    code.extend_from_slice(&[0x0f, 0x05]); // syscall
    let mut m = mach(&code);
    m.mem.write(DATA, b"ok").unwrap();
    let code_out = m.run(1000).expect("应正常退出");
    assert_eq!(code_out, 5, "exit(5) 的退出码要透传");
}

#[test]
fn syscall_clobbers_rcx_and_r11_like_hardware() {
    // getpid 之后 rcx 应等于 syscall 之后的地址
    let mut m = mach(&[0xb8, 0x27, 0, 0, 0, 0x0f, 0x05]); // mov $39,%eax ; syscall
    steps(&mut m, 2);
    assert_eq!(m.cpu.regs[RCX], CODE + 7, "rcx = 返回地址");
    assert_ne!(m.cpu.regs[R11], 0, "r11 = rflags");
}

#[test]
fn unimplemented_syscall_returns_enosys_not_success() {
    // 用一个我们没实现的号（1000）
    let mut m = mach(&[0xb8, 0xe8, 0x03, 0, 0, 0x0f, 0x05]);
    steps(&mut m, 2);
    assert_eq!(m.cpu.regs[RAX] as i64, -38, "必须是 -ENOSYS，不能假装成功");
}

#[test]
fn unmapped_write_reports_fault_with_address() {
    // movabs $0xdeadbeef000,%rax ; movq $1,(%rax)
    let mut code = vec![0x48, 0xb8];
    code.extend_from_slice(&0x0dea_dbee_f000u64.to_le_bytes());
    code.extend_from_slice(&[0x48, 0xc7, 0x00, 0x01, 0, 0, 0]);
    let mut m = mach(&code);
    steps(&mut m, 1);
    match m.step() {
        Err(Exception::Fault(f)) => assert_eq!(f.addr, 0x0dea_dbee_f000),
        other => panic!("越界写应报 fault，实际 {other:?}"),
    }
}

#[test]
fn unknown_opcode_reports_bytes_for_diagnosis() {
    // 0x0f 0x0b = ud2
    let mut m = mach(&[0x0f, 0x0b]);
    match m.step() {
        Err(Exception::Undefined { rip, bytes }) => {
            assert_eq!(rip, CODE);
            assert_eq!(&bytes[..2], &[0x0f, 0x0b], "要打出原始字节便于对照 objdump");
        }
        other => panic!("未实现指令应报 Undefined，实际 {other:?}"),
    }
}

#[test]
fn instruction_budget_stops_runaway_guest() {
    // jmp .  —— 原地死循环
    let mut m = mach(&[0xeb, 0xfe]);
    match m.run(500) {
        Err(Exception::Killed { signo }) => assert_eq!(signo, 24, "SIGXCPU"),
        other => panic!("死循环应被指令预算打断，实际 {other:?}"),
    }
    assert!(m.cpu.icount >= 500);
}

#[test]
fn fs_segment_prefix_adds_tls_base() {
    // arch_prctl(ARCH_SET_FS, DATA) 然后 mov %fs:0x8,%rax
    let mut code = vec![
        0xb8, 0x9e, 0, 0, 0, // mov $158,%eax (arch_prctl)
        0xbf, 0x02, 0x10, 0, 0, // mov $0x1002,%edi (ARCH_SET_FS)
    ];
    code.extend_from_slice(&[0x48, 0xbe]);
    code.extend_from_slice(&DATA.to_le_bytes()); // movabs $DATA,%rsi
    code.extend_from_slice(&[0x0f, 0x05]); // syscall
    code.extend_from_slice(&[0x64, 0x48, 0x8b, 0x04, 0x25, 0x08, 0, 0, 0]); // mov %fs:0x8,%rax
    let mut m = mach(&code);
    m.mem.write_u64(DATA + 8, 0xabcd).unwrap();
    steps(&mut m, 5);
    assert_eq!(m.cpu.fs_base, DATA);
    assert_eq!(m.cpu.regs[RAX], 0xabcd, "fs: 前缀要加上 TLS 基址");
}

#[test]
fn fxsave_fxrstor_round_trip_xmm_and_mxcsr() {
    // rax=DATA; fxsave64 [rax]; 清状态；fxrstor64 [rax]
    let mut code = vec![0x48, 0xb8];
    code.extend_from_slice(&DATA.to_le_bytes());
    code.extend_from_slice(&[
        0x48, 0x0f, 0xae, 0x00, // fxsave64 (%rax)
        0x48, 0x0f, 0xae, 0x08, // fxrstor64 (%rax)
    ]);
    let mut m = mach(&code);
    let saved = *b"0123456789abcdef";
    m.cpu.xmm[0] = saved;
    m.cpu.mxcsr = 0x1fa0;

    steps(&mut m, 2);
    assert_eq!(
        m.mem.read_u32(DATA + 24).unwrap(),
        0x1fa0,
        "FXSAVE must write MXCSR at the architectural offset"
    );
    let mut saved_xmm = [0u8; 16];
    m.mem.read(DATA + 160, &mut saved_xmm).unwrap();
    assert_eq!(saved_xmm, saved);
    m.cpu.xmm[0] = [0; 16];
    m.cpu.mxcsr = 0;
    steps(&mut m, 1);
    assert_eq!(m.cpu.xmm[0], saved);
    assert_eq!(m.cpu.mxcsr, 0x1fa0);
}

#[test]
fn stmxcsr_and_ldmxcsr_round_trip() {
    // rax=DATA; stmxcsr [rax]; ldmxcsr 4[rax]
    let mut code = vec![0x48, 0xb8];
    code.extend_from_slice(&DATA.to_le_bytes());
    code.extend_from_slice(&[
        0x0f, 0xae, 0x18, // stmxcsr (%rax)
        0x0f, 0xae, 0x50, 0x04, // ldmxcsr 4(%rax)
    ]);
    let mut m = mach(&code);
    m.cpu.mxcsr = 0x1f80;
    m.mem.write_u32(DATA + 4, 0x1fa0).unwrap();
    steps(&mut m, 3);
    assert_eq!(m.mem.read_u32(DATA).unwrap(), 0x1f80);
    assert_eq!(m.cpu.mxcsr, 0x1fa0);
}

// ------------------------------------------------------- SSE 浮点

/// 把一个 f64 放进 DATA，再用 movsd 装进 xmm。
fn load_sd(code: &mut Vec<u8>, xmm_hi_nibble: u8) {
    // movsd (%rax),%xmmN  ->  f2 0f 10 /r
    code.extend_from_slice(&[0xf2, 0x0f, 0x10, xmm_hi_nibble]);
}

#[test]
fn addsd_subsd_mulsd_divsd() {
    // rax=DATA; movsd (%rax),%xmm0; movsd 8(%rax),%xmm1; addsd %xmm1,%xmm0
    let mut code = vec![0x48, 0xb8];
    code.extend_from_slice(&DATA.to_le_bytes());
    load_sd(&mut code, 0x00); // xmm0 <- [rax]
    code.extend_from_slice(&[0xf2, 0x0f, 0x10, 0x48, 0x08]); // xmm1 <- 8(%rax)
    code.extend_from_slice(&[0xf2, 0x0f, 0x58, 0xc1]); // addsd %xmm1,%xmm0
    let mut m = mach(&code);
    m.mem.write(DATA, &1.5f64.to_le_bytes()).unwrap();
    m.mem.write(DATA + 8, &2.25f64.to_le_bytes()).unwrap();
    steps(&mut m, 4);
    assert_eq!(
        f64::from_le_bytes(m.cpu.xmm[0][0..8].try_into().unwrap()),
        3.75
    );

    // 同样的编码换成 mulsd(0x59) / subsd(0x5c) / divsd(0x5e)
    for (op, want) in [(0x59u8, 1.5 * 2.25), (0x5c, 1.5 - 2.25), (0x5e, 1.5 / 2.25)] {
        let mut c = code.clone();
        let n = c.len();
        c[n - 2] = op;
        let mut m = mach(&c);
        m.mem.write(DATA, &1.5f64.to_le_bytes()).unwrap();
        m.mem.write(DATA + 8, &2.25f64.to_le_bytes()).unwrap();
        steps(&mut m, 4);
        assert_eq!(
            f64::from_le_bytes(m.cpu.xmm[0][0..8].try_into().unwrap()),
            want,
            "opcode {op:#x}"
        );
    }
}

#[test]
fn scalar_float_op_preserves_high_lane() {
    // 标量运算只写 lane 0，高 64 位必须原样保留——编译器会在高位存活跃值。
    let mut code = vec![0x48, 0xb8];
    code.extend_from_slice(&DATA.to_le_bytes());
    code.extend_from_slice(&[0xf2, 0x0f, 0x58, 0x00]); // addsd (%rax),%xmm0
    let mut m = mach(&code);
    m.mem.write(DATA, &1.0f64.to_le_bytes()).unwrap();
    m.cpu.xmm[0][0..8].copy_from_slice(&2.0f64.to_le_bytes());
    m.cpu.xmm[0][8..16].copy_from_slice(&0xdead_beef_cafe_babeu64.to_le_bytes());
    steps(&mut m, 2);
    assert_eq!(
        f64::from_le_bytes(m.cpu.xmm[0][0..8].try_into().unwrap()),
        3.0
    );
    assert_eq!(
        u64::from_le_bytes(m.cpu.xmm[0][8..16].try_into().unwrap()),
        0xdead_beef_cafe_babe,
        "标量运算不得动高 64 位"
    );
}

#[test]
fn cvtsi2sd_and_cvttsd2si_roundtrip() {
    // mov $-7,%eax ; cvtsi2sd %eax,%xmm0 ; cvttsd2si %xmm0,%ecx
    let mut m = mach(&[
        0xb8, 0xf9, 0xff, 0xff, 0xff, // mov $-7,%eax
        0xf2, 0x0f, 0x2a, 0xc0, // cvtsi2sd %eax,%xmm0
        0xf2, 0x0f, 0x2c, 0xc8, // cvttsd2si %xmm0,%ecx
    ]);
    steps(&mut m, 3);
    assert_eq!(
        f64::from_le_bytes(m.cpu.xmm[0][0..8].try_into().unwrap()),
        -7.0
    );
    assert_eq!(m.cpu.regs[RCX] as u32 as i32, -7);
}

#[test]
fn cvttsd2si_truncates_toward_zero() {
    // -2.7 截断应是 -2（不是 -3）
    let mut code = vec![0x48, 0xb8];
    code.extend_from_slice(&DATA.to_le_bytes());
    code.extend_from_slice(&[0xf2, 0x0f, 0x10, 0x00]); // movsd (%rax),%xmm0
    code.extend_from_slice(&[0xf2, 0x0f, 0x2c, 0xc8]); // cvttsd2si %xmm0,%ecx
    let mut m = mach(&code);
    m.mem.write(DATA, &(-2.7f64).to_le_bytes()).unwrap();
    steps(&mut m, 3);
    assert_eq!(m.cpu.regs[RCX] as u32 as i32, -2);
}

#[test]
fn cvttsd2si_out_of_range_returns_integer_indefinite() {
    let mut code = vec![0x48, 0xb8];
    code.extend_from_slice(&DATA.to_le_bytes());
    code.extend_from_slice(&[0xf2, 0x0f, 0x10, 0x00]);
    code.extend_from_slice(&[0xf2, 0x0f, 0x2c, 0xc8]); // 32 位目标
    let mut m = mach(&code);
    m.mem.write(DATA, &1e30f64.to_le_bytes()).unwrap();
    steps(&mut m, 3);
    assert_eq!(
        m.cpu.regs[RCX] as u32 as i32,
        i32::MIN,
        "超范围要返回整数不定值"
    );
}

#[test]
fn ucomisd_sets_flags_and_flags_nan_as_unordered() {
    // movsd (%rax),%xmm0 ; ucomisd 8(%rax),%xmm0
    let mut code = vec![0x48, 0xb8];
    code.extend_from_slice(&DATA.to_le_bytes());
    code.extend_from_slice(&[0xf2, 0x0f, 0x10, 0x00]);
    code.extend_from_slice(&[0x66, 0x0f, 0x2e, 0x40, 0x08]);

    // 1.0 vs 2.0 -> 小于：CF 置，ZF 清
    let mut m = mach(&code);
    m.mem.write(DATA, &1.0f64.to_le_bytes()).unwrap();
    m.mem.write(DATA + 8, &2.0f64.to_le_bytes()).unwrap();
    steps(&mut m, 3);
    assert!(m.cpu.flags.cf && !m.cpu.flags.zf && !m.cpu.flags.pf);

    // 相等 -> ZF 置，CF 清
    let mut m = mach(&code);
    m.mem.write(DATA, &3.0f64.to_le_bytes()).unwrap();
    m.mem.write(DATA + 8, &3.0f64.to_le_bytes()).unwrap();
    steps(&mut m, 3);
    assert!(m.cpu.flags.zf && !m.cpu.flags.cf && !m.cpu.flags.pf);

    // NaN -> 无序：ZF/PF/CF 全置（`jp` 靠 PF 检测 NaN）
    let mut m = mach(&code);
    m.mem.write(DATA, &f64::NAN.to_le_bytes()).unwrap();
    m.mem.write(DATA + 8, &1.0f64.to_le_bytes()).unwrap();
    steps(&mut m, 3);
    assert!(
        m.cpu.flags.pf && m.cpu.flags.zf && m.cpu.flags.cf,
        "NaN 必须报无序"
    );
}

#[test]
fn sqrtsd_computes_square_root() {
    let mut code = vec![0x48, 0xb8];
    code.extend_from_slice(&DATA.to_le_bytes());
    code.extend_from_slice(&[0xf2, 0x0f, 0x51, 0x00]); // sqrtsd (%rax),%xmm0
    let mut m = mach(&code);
    m.mem.write(DATA, &2.25f64.to_le_bytes()).unwrap();
    steps(&mut m, 2);
    assert_eq!(
        f64::from_le_bytes(m.cpu.xmm[0][0..8].try_into().unwrap()),
        1.5
    );
}

#[test]
fn minsd_returns_second_operand_on_nan() {
    // x86 的 MINSD：任一操作数是 NaN 就返回**源**操作数，
    // 与 Rust 的 f64::min（挑非 NaN 的）不同。
    let mut code = vec![0x48, 0xb8];
    code.extend_from_slice(&DATA.to_le_bytes());
    code.extend_from_slice(&[0xf2, 0x0f, 0x5d, 0x00]); // minsd (%rax),%xmm0
    let mut m = mach(&code);
    m.mem.write(DATA, &5.0f64.to_le_bytes()).unwrap(); // src = 5.0
    m.cpu.xmm[0][0..8].copy_from_slice(&f64::NAN.to_le_bytes()); // dst = NaN
    steps(&mut m, 2);
    assert_eq!(
        f64::from_le_bytes(m.cpu.xmm[0][0..8].try_into().unwrap()),
        5.0,
        "dst 是 NaN 时应返回 src"
    );
}

#[test]
fn cvtss2sd_widens_float_to_double() {
    let mut code = vec![0x48, 0xb8];
    code.extend_from_slice(&DATA.to_le_bytes());
    code.extend_from_slice(&[0xf3, 0x0f, 0x5a, 0x00]); // cvtss2sd (%rax),%xmm0
    let mut m = mach(&code);
    m.mem.write(DATA, &0.5f32.to_le_bytes()).unwrap();
    steps(&mut m, 2);
    assert_eq!(
        f64::from_le_bytes(m.cpu.xmm[0][0..8].try_into().unwrap()),
        0.5
    );
}

#[test]
fn shufpd_selects_qwords_from_dst_and_src() {
    // shufpd $0x1,%xmm1,%xmm0：低 64 位取 dst 的 qword1，高 64 位取 src 的 qword0
    let mut m = mach(&[0x66, 0x0f, 0xc6, 0xc1, 0x01]);
    m.cpu.xmm[0] = *b"DDDD0000DDDD1111";
    m.cpu.xmm[1] = *b"SSSS0000SSSS1111";
    steps(&mut m, 1);
    assert_eq!(&m.cpu.xmm[0][0..8], b"DDDD1111", "低半来自 dst 的高 qword");
    assert_eq!(&m.cpu.xmm[0][8..16], b"SSSS0000", "高半来自 src 的低 qword");
}

#[test]
fn packuswb_saturates_to_unsigned_bytes() {
    // packuswb %xmm1,%xmm0：16 位有符号 -> 8 位无符号饱和
    let mut m = mach(&[0x66, 0x0f, 0x67, 0xc1]);
    // dst lane0 = 300（饱和到 255），lane1 = -5（饱和到 0）
    m.cpu.xmm[0][0..2].copy_from_slice(&300i16.to_le_bytes());
    m.cpu.xmm[0][2..4].copy_from_slice(&(-5i16).to_le_bytes());
    m.cpu.xmm[0][4..6].copy_from_slice(&7i16.to_le_bytes());
    steps(&mut m, 1);
    assert_eq!(m.cpu.xmm[0][0], 255, "正向饱和");
    assert_eq!(m.cpu.xmm[0][1], 0, "负数饱和到 0");
    assert_eq!(m.cpu.xmm[0][2], 7);
}

#[test]
fn cmpsd_produces_all_ones_or_all_zeros_mask() {
    // cmpsd $0,%xmm1,%xmm0（谓词 0 = eq）
    let mut m = mach(&[0xf2, 0x0f, 0xc2, 0xc1, 0x00]);
    m.cpu.xmm[0][0..8].copy_from_slice(&1.0f64.to_le_bytes());
    m.cpu.xmm[1][0..8].copy_from_slice(&1.0f64.to_le_bytes());
    steps(&mut m, 1);
    assert_eq!(&m.cpu.xmm[0][0..8], &[0xff; 8], "相等 -> 全 1");

    let mut m = mach(&[0xf2, 0x0f, 0xc2, 0xc1, 0x00]);
    m.cpu.xmm[0][0..8].copy_from_slice(&1.0f64.to_le_bytes());
    m.cpu.xmm[1][0..8].copy_from_slice(&2.0f64.to_le_bytes());
    steps(&mut m, 1);
    assert_eq!(&m.cpu.xmm[0][0..8], &[0x00; 8], "不等 -> 全 0");
}
