use kernel_refactored::*;

#[test]
fn basic_ring_write_read() {
    let mut rb = CircBuf::new(8);
    let input = [10u8, 20, 30, 40, 50];
    for &v in &input {
        assert!(rb.push(v));
    }
    for &expected in &input {
        assert_eq!(rb.pop(), Some(expected));
    }
    assert!(rb.empty());
}

#[test]
fn basic_ring_full_reject() {
    let mut rb = CircBuf::new(4);
    for i in 0..4u8 {
        assert!(rb.push(i));
    }
    assert_eq!(rb.len(), 4);
    assert!(!rb.push(0xFF));
}

#[test]
fn basic_ring_wrap_around() {
    let mut rb = CircBuf::new(4);
    for i in 0..3u8 {
        assert!(rb.push(i + 1));
    }
    for i in 0..3u8 {
        assert_eq!(rb.pop(), Some(i + 1));
    }
    assert!(rb.empty());

    let second = [0xA0u8, 0xB0, 0xC0];
    for &v in &second {
        assert!(rb.push(v));
    }
    for &expected in &second {
        assert_eq!(rb.pop(), Some(expected));
    }
    assert!(rb.empty());
}
