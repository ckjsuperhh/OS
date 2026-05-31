use kernel_refactored::*;

#[test]
fn basic_save_restore_context() {
    let mut regs = [0u64; N_REGS];
    regs[0] = 0xAA;
    regs[1] = 0xBB;
    regs[2] = 0xCC;
    let ctx = Context::capture(&regs);
    let restored = ctx.apply();
    assert_eq!(restored[0], 0xAA);
}

#[test]
fn basic_interrupt_mask_set() {
    let tc = TrapCtl::new();
    tc.configure(0xFF, 0x00);
    assert_eq!(tc.hw(), 0x00);
}

#[test]
fn basic_page_fault_in_process_context() {
    let tc = TrapCtl::new();
    let result = tc.on_pgfault(0x1000);
    assert!(result.is_ok());
}
